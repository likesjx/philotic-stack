#!/usr/bin/env python3
"""Skill administration plane smoke — run against a LIVE hotel socket.

Usage: smoke-skill-admin.py <hotel-socket> [agent-id]
  e.g. scripts/smoke-skill-admin.py ~/.philotic/bjork/aiua-mac-jane.sock agent-bjork-01
       sudo -u philotic python3 scripts/smoke-skill-admin.py /run/philotic/vps-jane.sock agent-beacon-01

Speaks newline-framed JSON IpcRequests over the hotel UDS.
Proves, against the *installed, supervised* runtime:
  1. unauthenticated ListSkills is rejected (new gate)
  2. orchestrator can register skills carrying SkillDAG edges (persisted, visible)
  3. skill.set_state suspends/reinstates with projection-state visible in skill.list
  4. the audit trail records register/update/assign-style actions with actor identity
  5. non-orchestrator role is rejected by the centralized gate
  6. boot seeds reconciled: skill.crafting implies skill.set_state + skill.audit
"""
import json
import socket
import sys

SOCK = sys.argv[1]
AGENT = sys.argv[2] if len(sys.argv) > 2 else "agent-bjork-01"


class Ipc:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.settimeout(15)
        self.s.connect(SOCK)
        self.buf = b""

    def _read_exact(self, n):
        while len(self.buf) < n:
            chunk = self.s.recv(65536)
            if not chunk:
                raise RuntimeError("connection closed")
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def _read_line(self):
        # 4-byte big-endian length-prefixed JSON frames
        import struct
        (length,) = struct.unpack(">I", self._read_exact(4))
        return json.loads(self._read_exact(length))

    def call(self, operation, payload, expect_keys=()):
        import struct
        body = json.dumps({"operation": operation, "payload": payload}).encode()
        self.s.sendall(struct.pack(">I", len(body)) + body)
        # The hotel pushes unsolicited messages (MuninnStatus, InboundTask, …)
        # on the same stream; skip anything that is neither a Standard
        # ok/error envelope nor carries a key we expect for this call.
        for _ in range(50):
            r = self._read_line()
            if isinstance(r, dict) and (
                "ok" in r or "code" in r or any(k in r for k in expect_keys)
            ):
                return r
            print(f"  (skipping push: {str(r)[:120]})", flush=True)
        raise RuntimeError(f"no response for {operation}")

    def close(self):
        self.s.close()


results = []


def check(name, ok, detail=""):
    results.append((name, ok, detail))
    print(("PASS " if ok else "FAIL ") + name + (" — " + str(detail)[:200] if detail else ""))


# ── 1. unauthenticated ListSkills rejected ────────────────────────────────
c = Ipc()
r = c.call("list_skills", {}, expect_keys=("skills",))
check(
    "unauth skill.list rejected",
    r.get("ok") is False and r.get("code") == "LIST_SKILLS_UNREGISTERED",
    r.get("code"),
)
c.close()

# ── 2. non-orchestrator role rejected by the centralized gate ─────────────
c = Ipc()
c.call("register", {"guest_id": f"{AGENT}:skilldrill-worker", "role": "worker", "supported_tools": []})
r = c.call(
    "register_skill",
    {
        "skill_name": "smoke.forbidden",
        "description": "must be rejected",
        "subagent_kind": "philote-worker",
        "goal": "noop",
    },
    expect_keys=("skill_name",),
)
check(
    "non-orchestrator register rejected",
    r.get("ok") is False and r.get("code") == "REGISTER_FORBIDDEN",
    r.get("code"),
)
c.close()

# ── orchestrator connection for the rest ──────────────────────────────────
c = Ipc()
c.call("register", {"guest_id": f"{AGENT}:skilldrill", "role": "orchestrator", "supported_tools": []})

# ── 3. register dep + skill with a DAG edge ───────────────────────────────
r = c.call(
    "register_skill",
    {
        "skill_name": "smoke.skill-admin-dep",
        "description": "Live-drill dependency skill (PR #430).",
        "subagent_kind": "philote-worker",
        "goal": "noop dependency",
        "allowed_tools": ["echo"],
    },
    expect_keys=("skill_name",),
)
check("register dep skill", r.get("skill_name") == "smoke.skill-admin-dep", r)

r = c.call(
    "register_skill",
    {
        "skill_name": "smoke.skill-admin-live",
        "description": "Live-drill skill with a SkillDAG edge (PR #430).",
        "subagent_kind": "philote-worker",
        "goal": "Prove the admin plane live on {{hotel}}.",
        "allowed_tools": ["session.status"],
        "allowed_classes": ["utility"],
        "allowed_skills": ["smoke.skill-admin-dep"],
    },
    expect_keys=("skill_name",),
)
check("register skill with DAG edge", r.get("skill_name") == "smoke.skill-admin-live", r)

# ── 4. skill.list shows edges, classes, states ────────────────────────────
r = c.call("list_skills", {}, expect_keys=("skills",))
skills = {s["skill_name"]: s for s in r.get("skills", [])}
live = skills.get("smoke.skill-admin-live", {})
check(
    "skill.list shows persisted DAG edge",
    live.get("allowed_skills") == ["smoke.skill-admin-dep"]
    and live.get("implied_classes") == ["utility"],
    {k: live.get(k) for k in ("allowed_skills", "implied_classes", "validation_state")},
)
crafting = skills.get("skill.crafting", {})
check(
    "seed reconciled: skill.crafting implies set_state+audit",
    "skill.set_state" in crafting.get("implied_tools", [])
    and "skill.audit" in crafting.get("implied_tools", []),
    crafting.get("implied_tools"),
)

# ── 5. lifecycle: suspend → visible; reactivate → visible ─────────────────
r = c.call(
    "set_skill_state",
    {"skill_name": "smoke.skill-admin-dep", "state": "suspended", "reason": "live drill suspension"},
    expect_keys=("skill_state",),
)
check("suspend dep skill", r.get("skill_state") == "suspended", r)

r = c.call("list_skills", {}, expect_keys=("skills",))
skills = {s["skill_name"]: s for s in r.get("skills", [])}
check(
    "skill.list reflects suspended state",
    skills.get("smoke.skill-admin-dep", {}).get("validation_state") == "suspended",
    skills.get("smoke.skill-admin-dep", {}).get("validation_state"),
)

r = c.call("set_skill_state", {"skill_name": "smoke.skill-admin-dep", "state": "active"}, expect_keys=("skill_state",))
check("reinstate dep skill", r.get("skill_state") == "active", r)

# invalid state rejected
r = c.call("set_skill_state", {"skill_name": "smoke.skill-admin-dep", "state": "banished"}, expect_keys=("skill_state",))
check(
    "invalid state rejected",
    r.get("ok") is False and r.get("code") == "SET_SKILL_STATE_INVALID",
    r.get("code"),
)

# ── 6. audit trail ────────────────────────────────────────────────────────
r = c.call("list_skill_audits", {"limit": 20}, expect_keys=("skill_audits",))
audits = r.get("skill_audits", [])
actions = [(a.get("skill_name"), a.get("action")) for a in audits]
check(
    "audit trail has register + set_state entries",
    ("smoke.skill-admin-live", "register") in actions
    and ("smoke.skill-admin-dep", "set_state") in actions,
    actions[:8],
)
by_ok = all(a.get("by") == f"{AGENT}:skilldrill" for a in audits if a.get("skill_name", "").startswith("smoke.skill-admin"))
check("audit records actor identity", by_ok)

# ── retire the drill skills (leaves them non-projecting) ──────────────────
for name in ("smoke.skill-admin-live", "smoke.skill-admin-dep"):
    r = c.call("set_skill_state", {"skill_name": name, "state": "deprecated", "reason": "drill complete"}, expect_keys=("skill_state",))
    check(f"retire {name}", r.get("skill_state") == "deprecated", r.get("skill_state"))

c.close()

fails = [n for n, ok, _ in results if not ok]
print()
print(f"RESULT: {len(results) - len(fails)}/{len(results)} passed" + (f" — FAILURES: {fails}" if fails else " — ALL GREEN"))
sys.exit(1 if fails else 0)
