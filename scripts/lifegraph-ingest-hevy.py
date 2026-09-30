#!/usr/bin/env python3
"""Hevy → LifeGraph importer: tracked workouts as first-class Workout nodes.

First real ingestion lane for the health & fitness domain (memory-RAG audit
follow-up, 2026-09-30): pulls recent workouts from the Hevy API and lands each
one as a `Workout` node (V007 labels) through the governed `life.observe.batch`
path over the hotel IPC socket — same transport as
`scripts/lifegraph_batch_probe.py`, so it needs no Rust build and every write
carries a full provenance envelope, SCOPED_TO anchoring, and per-item isolation.

Idempotent by construction: node ids are `life:workout:hevy-<workout_id>`, so a
re-run MERGE-updates the same nodes instead of duplicating them.

Provenance: `source_kind: imported_record`, `basis: imported_authority`,
`validation_state: inferred` — an imported record from the operator's own
tracker is stronger than agent speculation (`proposed`) but still not
operator-confirmed truth; `life.commit` promotion stays governed.

Usage (local hotel):

    HEVY_API_KEY=... \
    PHILOTIC_HOTEL_SOCKET=~/.philotic/<profile>/aiua-<hotel>.sock \
    PHILOTIC_TARGET_NODE=<hotel-node-id> \
    python3 scripts/lifegraph-ingest-hevy.py

Cross-mesh (a Mac driving the vps runner): also set PHILOTIC_REPLY_NODE to the
LOCAL node id so the runner's ack has somewhere to land.

Env knobs: HEVY_LIMIT (default 10 workouts, newest first), DRY_RUN=1 (print the
batch, send nothing), PHILOTIC_OBSERVED_BY (default agent-coach-01 — the
"human" domain steward, so nodes anchor under the health Role).
"""

import json
import os
import socket
import struct
import sys
import time
import urllib.request

HEVY_API = "https://api.hevyapp.com/v1/workouts"
API_KEY = os.environ.get("HEVY_API_KEY", "")
LIMIT = int(os.environ.get("HEVY_LIMIT", "10"))
DRY_RUN = os.environ.get("DRY_RUN") == "1"

SOCK = os.path.expanduser(
    os.environ.get("PHILOTIC_HOTEL_SOCKET", "/run/philotic/vps-jane.sock")
)
TARGET_NODE = os.environ.get("PHILOTIC_TARGET_NODE", "vps-jane-aiua-01")
REPLY_NODE = os.environ.get("PHILOTIC_REPLY_NODE", TARGET_NODE)
OBSERVED_BY = os.environ.get("PHILOTIC_OBSERVED_BY", "agent-coach-01")
GUEST = "lifegraph-hevy-ingest"
ROLE = "life-graph.hevy.ingest.reply"
WAIT = float(os.environ.get("INGEST_WAIT", "300"))
BATCH_MAX = 25  # runner contract: MAX_OBSERVE_BATCH


class Ipc:
    def __init__(self, path):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.connect(path)
        self.buf = b""

    def send(self, operation, payload):
        frame = json.dumps({"operation": operation, "payload": payload}).encode()
        self.s.sendall(struct.pack(">I", len(frame)) + frame)

    def read_frame(self, deadline):
        while True:
            if len(self.buf) >= 4:
                (ln,) = struct.unpack(">I", self.buf[:4])
                if len(self.buf) >= 4 + ln:
                    payload = self.buf[4 : 4 + ln]
                    self.buf = self.buf[4 + ln :]
                    return json.loads(payload)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("no IPC frame before deadline")
            self.s.settimeout(remaining)
            chunk = self.s.recv(65536)
            if not chunk:
                raise RuntimeError("IPC stream closed")
            self.buf += chunk


def fetch_workouts(limit):
    workouts = []
    page = 1
    while len(workouts) < limit:
        page_size = min(10, limit - len(workouts))
        req = urllib.request.Request(
            f"{HEVY_API}?page={page}&pageSize={page_size}",
            headers={"api-key": API_KEY, "accept": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=30) as resp:
            body = json.load(resp)
        batch = body.get("workouts", [])
        if not batch:
            break
        workouts.extend(batch)
        if page >= int(body.get("page_count", page)):
            break
        page += 1
    return workouts[:limit]


def summarize(workout):
    exercises = workout.get("exercises", [])
    sets = sum(len(e.get("sets", [])) for e in exercises)
    volume = 0.0
    for e in exercises:
        for s in e.get("sets", []):
            w = s.get("weight_kg") or 0
            r = s.get("reps") or 0
            volume += (w or 0) * (r or 0)
    names = ", ".join(e.get("title", "?") for e in exercises[:5])
    if len(exercises) > 5:
        names += f", +{len(exercises) - 5} more"
    title = workout.get("title") or "Workout"
    start = workout.get("start_time") or ""
    end = workout.get("end_time") or ""
    return (
        f"{title} on {start[:10]}: {len(exercises)} exercises, {sets} sets, "
        f"~{volume:.0f} kg total volume ({names})."
    ), start, end, sets, volume, len(exercises)


def observation(workout):
    summary, start, end, sets, volume, n_ex = summarize(workout)
    wid = workout.get("id")
    node_id = f"life:workout:hevy-{wid}"
    return {
        "edges": [],
        "observation_id": f"obs-hevy-{wid}",
        "observed_by": OBSERVED_BY,
        "evidence": {
            "packet_id": f"pkt-hevy-{wid}",
            "claim_ref": {"id": node_id, "label": "Workout", "datasource": "life-graph"},
            "claim_summary": summary,
            "source_refs": [
                {
                    "source_id": f"hevy:workout:{wid}",
                    "source_kind": "imported_record",
                    "reliability": {"score": 0.9, "basis": "imported_authority"},
                }
            ],
            "passage_refs": [],
            "confidence": 0.9,
            "validation_state": "inferred",
            "observed_at": end or start or None,
            "source_reliability": 0.9,
            "conflict_ids": [],
            "adjudication_status": "not_needed",
            "metadata": {
                "route": "lifegraph_hevy_ingest",
                "hevy_workout_id": wid,
                "starts_at": start,
                "ends_at": end,
                "exercise_count": n_ex,
                "set_count": sets,
                "volume_kg": round(volume, 1),
            },
        },
        "proposed_graph_refs": [],
    }


def main():
    if not API_KEY:
        print("HEVY_API_KEY is required", file=sys.stderr)
        return 2
    workouts = fetch_workouts(LIMIT)
    if not workouts:
        print("[hevy-ingest] no workouts returned — nothing to do")
        return 0
    items = [observation(w) for w in workouts]
    print(f"[hevy-ingest] {len(items)} workouts → {TARGET_NODE} as Workout nodes")
    if DRY_RUN:
        print(json.dumps(items, indent=2)[:4000])
        return 0

    ipc = Ipc(SOCK)
    ipc.send("register", {"guest_id": GUEST, "role": ROLE, "supported_tools": []})
    ipc.send("subscribe_inbox", {"role": ROLE})

    ok = err = 0
    for chunk_start in range(0, len(items), BATCH_MAX):
        chunk = items[chunk_start : chunk_start + BATCH_MAX]
        turn_id = f"hevy-ingest-{int(time.time())}-{chunk_start}"
        task = {
            "action": "execute_tool",
            "tool_name": "life.observe.batch",
            "arguments": {"observations": chunk},
            "session_id": f"ingest:hevy:{turn_id}",
            "turn_id": turn_id,
            "chat_id": "hevy-ingest",
            "agent_id": OBSERVED_BY,
            "reply_to": REPLY_NODE,
            "reply_role": ROLE,
        }
        ipc.send(
            "emit_task",
            {
                "target_node": TARGET_NODE,
                "target_role": "life-graph-runner",
                "target_guest_id": None,
                "task_json": json.dumps(task),
            },
        )
        deadline = time.monotonic() + WAIT
        while True:
            try:
                frame = ipc.read_frame(deadline)
            except TimeoutError:
                print(f"[hevy-ingest] batch at {chunk_start}: NO RESPONSE — check runner")
                return 2
            inbound = frame.get("InboundTask") or (
                frame.get("payload") if frame.get("response") == "inbound_task" else None
            )
            if isinstance(frame, dict) and "task_json" in frame:
                inbound = frame
            if not inbound or "task_json" not in inbound:
                continue
            payload = json.loads(inbound["task_json"])
            if payload.get("action") != "datasource_response":
                continue
            if payload.get("turn_id") != turn_id:
                continue
            if payload.get("error"):
                print(f"[hevy-ingest] batch error: {payload['error']}")
                err += len(chunk)
            else:
                results = (payload.get("result") or {}).get("results") or []
                batch_ok = sum(1 for r in results if r.get("status") == "ok")
                batch_err = len(results) - batch_ok
                if not results:
                    batch_ok = len(chunk)
                    batch_err = 0
                ok += batch_ok
                err += batch_err
                print(f"[hevy-ingest] batch at {chunk_start}: {batch_ok} ok, {batch_err} errors")
            break

    print(f"[hevy-ingest] DONE: {ok} written/merged, {err} errors")
    return 0 if err == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
