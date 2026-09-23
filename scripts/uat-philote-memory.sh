#!/usr/bin/env bash
# UAT for the philote memory loop (Muninn Memory Core, proposal § Phase 2).
#
#   bash scripts/uat-philote-memory.sh            # read-only checks against the live fleet
#   bash scripts/uat-philote-memory.sh --write    # also runs one end-to-end write, then undoes it
#
# Read-only by default: it reads health, hotel ledgers and logs, and reads memories
# back by id. Nothing is written unless --write is passed, and that write is
# forgotten again before the run ends.
#
# NOT `just uat`: that recipe boots an ephemeral hotel and has killed the live
# mac-jane hotel before. This checks the fleet as it runs.
#
# Each check prints PASS / FAIL / WARN. Exit status is non-zero if anything FAILed.
set -uo pipefail

MAC_SSH=""                                             # local
MBP_SSH="${PHILOTIC_MBP_SSH:-jaredlikes@100.79.239.64}"
VPS_SSH="${PHILOTIC_VPS_SSH:-deploy@jane-vps}"
MAC_DB="${PHILOTIC_MAC_DB:-$HOME/.philotic/bjork/context.db}"
MAC_LOG="${PHILOTIC_MAC_LOG:-$HOME/.philotic/bjork/aiua.log}"
MBP_DB="${PHILOTIC_MBP_DB:-~/.philotic/jane/context.db}"
MBP_LOG="${PHILOTIC_MBP_LOG:-~/.philotic/jane/aiua.log}"
VPS_DB="${PHILOTIC_VPS_DB:-/opt/philotic/data/aiua_context.db}"
VPS_LOG_GLOB="${PHILOTIC_VPS_LOG:-/opt/philotic/data/logs/aiua*.log}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROBE="$ROOT_DIR/scripts/uat_memory_probe.py"
TMP="$(mktemp -d)"

WRITE_TEST=0
[[ "${1:-}" == "--write" ]] && WRITE_TEST=1
FAILURES=0
WARNINGS=0

say() { printf '%s\n' "$*"; }
pass() { say "  PASS  $*"; }
warn() { say "  WARN  $*"; WARNINGS=$((WARNINGS + 1)); }
fail() { say "  FAIL  $*"; FAILURES=$((FAILURES + 1)); }

# Run the probe on a node. $1 = ssh target ("" for local), rest = probe args.
REMOTE_PROBE="/tmp/uat_memory_probe.$$.py"
PROBE_COPIED=""

# Put the probe on a remote host once per run, so its stdin stays free for data.
ensure_probe() {
  local target="$1"
  [[ "$PROBE_COPIED" == *"|$target|"* ]] && return 0
  scp -q -o ConnectTimeout=15 -o BatchMode=yes "$PROBE" "$target:$REMOTE_PROBE" || return 1
  PROBE_COPIED="$PROBE_COPIED|$target|"
}

cleanup_probes() {
  local target
  for target in $MBP_SSH $VPS_SSH; do
    [[ "$PROBE_COPIED" == *"|$target|"* ]] &&
      ssh -o ConnectTimeout=10 -o BatchMode=yes "$target" "rm -f $REMOTE_PROBE" >/dev/null 2>&1
  done
}
trap 'rm -rf "$TMP"; cleanup_probes' EXIT

# Run the probe on a node. $1 = ssh target ("" for local), rest = probe args.
# Stdin is passed through, so muninn-have can be fed its id list.
probe() {
  local target="$1"; shift
  if [[ -z "$target" ]]; then
    python3 "$PROBE" "$@"
    return
  fi
  ensure_probe "$target" || return 1
  if [[ "$target" == "$VPS_SSH" ]]; then
    # The Cortex hotel DB and Muninn env are owned by the service user.
    ssh -o ConnectTimeout=15 -o BatchMode=yes "$target" \
      "set -a; . ~/.muninn/muninn.env >/dev/null 2>&1; set +a; sudo -n -u philotic env MUNINN_MCP_TOKEN=\"\$MUNINN_MCP_TOKEN\" python3 $REMOTE_PROBE $*"
  else
    ssh -o ConnectTimeout=15 -o BatchMode=yes "$target" "python3 $REMOTE_PROBE $*"
  fi
}

jqf() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }

say "=== philote memory UAT — $(date -u +%Y-%m-%dT%H:%MZ) ==="

# ── 1. Muninn is up on every node ────────────────────────────────────────────
say "[1] Muninn health"
for node in "mac:$MAC_SSH" "mbp:$MBP_SSH" "cortex:$VPS_SSH"; do
  name="${node%%:*}"; target="${node#*:}"
  out="$(probe "$target" muninn-health 2>/dev/null)"
  status="$(printf '%s' "$out" | jqf "d.get('status')")"
  version="$(printf '%s' "$out" | jqf "d.get('version','?')")"
  writable="$(printf '%s' "$out" | jqf "d.get('db_writable')")"
  if [[ "$status" == "ok" ]]; then
    pass "$name Muninn ok ($version, db_writable=$writable)"
  else
    fail "$name Muninn not ok: ${out:0:120}"
  fi
done

# ── 2. Replication: recent Cortex memories are on both observers ─────────────
say "[2] Replication of recent memories to observers"
probe "$VPS_SSH" muninn-recent 25 > "$TMP/recent.json" 2>/dev/null
total_ids="$(jqf "sum(len(v) for v in d.values())" < "$TMP/recent.json")"
if [[ -z "${total_ids:-}" || "$total_ids" == "0" ]]; then
  warn "could not list recent Cortex memories (skipping replication check)"
else
  for node in "mac:$MAC_SSH" "mbp:$MBP_SSH"; do
    name="${node%%:*}"; target="${node#*:}"
    missing_json="$(probe "$target" muninn-have < "$TMP/recent.json" 2>/dev/null)"
    missing="$(printf '%s' "$missing_json" | jqf "sum(len(v) for v in d.values())")"
    if [[ "${missing:-1}" == "0" ]]; then
      pass "$name has all $total_ids recent memories"
    else
      # One retry: replication is asynchronous.
      sleep 10
      missing_json="$(probe "$target" muninn-have < "$TMP/recent.json" 2>/dev/null)"
      missing="$(printf '%s' "$missing_json" | jqf "sum(len(v) for v in d.values())")"
      if [[ "${missing:-1}" == "0" ]]; then
        pass "$name has all $total_ids recent memories (after retry)"
      else
        fail "$name is missing $missing of $total_ids recent memories: ${missing_json:0:160}"
      fi
    fi
  done
fi

# ── 3–6. Per-hotel: recall health, writes, tool grants, credential state ─────
say "[3] Hotel checks (last 24h)"
for node in "mac:$MAC_SSH:$MAC_DB:$MAC_LOG" "mbp:$MBP_SSH:$MBP_DB:$MBP_LOG" "cortex:$VPS_SSH:$VPS_DB:$VPS_LOG_GLOB"; do
  IFS=: read -r name target db log <<< "$node"
  log_path="$log"
  [[ "$name" == "cortex" ]] && log_path="$(ssh -o ConnectTimeout=15 -o BatchMode=yes "$VPS_SSH" "ls -t $VPS_LOG_GLOB 2>/dev/null | head -1")"
  out="$(probe "$target" hotel "$db" "$log_path" 2>/dev/null)"
  if [[ -z "$out" ]]; then fail "$name hotel probe returned nothing"; continue; fi
  printf '%s' "$out" > "$TMP/$name-hotel.json"

  completed="$(jqf "d['recall_events']['memory_auto_recall_completed']" < "$TMP/$name-hotel.json")"
  failed="$(jqf "d['recall_events']['memory_auto_recall_failed']" < "$TMP/$name-hotel.json")"
  skipped="$(jqf "d['recall_events']['memory_auto_recall_skipped']" < "$TMP/$name-hotel.json")"
  median="$(jqf "d['recall_latency_ms']['median']" < "$TMP/$name-hotel.json")"
  bands="$(jqf "d['recall_bands']" < "$TMP/$name-hotel.json")"
  drops="$(jqf "d['recall_gate_drops']" < "$TMP/$name-hotel.json")"
  ran=$(( ${completed:-0} + ${failed:-0} ))
  if (( ran == 0 )); then
    warn "$name: no recalls in 24h (idle hotel?)"
  elif (( failed * 10 > ran )); then
    age="$(jqf "d['recall_newest_failure_age_h']" < "$TMP/$name-hotel.json")"
    if [[ -n "${age:-}" && "$age" != "None" ]] && (( $(printf '%.0f' "$age") >= 2 )); then
      warn "$name recall failures ${failed}/${ran} in 24h, but the newest is ${age}h old (already fixed?) — re-run later to confirm it clears"
    else
      fail "$name recall failures ${failed}/${ran} (>10%, newest ${age}h ago) — check 'Auto recall failed' in the hotel log"
    fi
  else
    pass "$name recall ${completed} ok / ${failed} failed / ${skipped} skipped, median ${median}ms, bands ${bands}, gate dropped ${drops}"
  fi

  rejected="$(jqf "d['rejected_writes_24h']" < "$TMP/$name-hotel.json")"
  if [[ "${rejected:-0}" == "0" ]]; then
    pass "$name no memory writes rejected by an observer (HTTP 421)"
  else
    fail "$name ${rejected} memory write(s) rejected with 421 — write routing is not reaching the Cortex"
  fi

  gaps="$(jqf "','.join(d['profiles_missing_memory_tools']) or 'none'" < "$TMP/$name-hotel.json")"
  if [[ "$gaps" == "none" ]]; then
    pass "$name every toolset profile grants memory.recall + memory.remember"
  else
    fail "$name profiles without memory tools: $gaps"
  fi

  plaintext="$(jqf "d['muninn_admin_password_in_config']" < "$TMP/$name-hotel.json")"
  vaultref="$(jqf "d['muninn_admin_vault_ref']" < "$TMP/$name-hotel.json")"
  if [[ "$plaintext" == "False" && "$vaultref" == "True" ]]; then
    pass "$name Muninn admin credential is vault-held, no plaintext in config"
  elif [[ "$plaintext" == "True" ]]; then
    fail "$name still has a plaintext Muninn admin password in config:muninn"
  else
    warn "$name has no muninn_admin_secret_ref (admin actions will fall back to config)"
  fi
done

# ── 7. Memory sleep on the Cortex ────────────────────────────────────────────
say "[4] Memory sleep (Cortex)"
sleep_out="$(probe "$VPS_SSH" sleep-run "$VPS_DB" 2>/dev/null)"
if [[ "$(printf '%s' "$sleep_out" | jqf "d.get('present')")" != "True" ]]; then
  warn "no memory-sleep run recorded yet (nightly 03:30 UTC)"
else
  age="$(printf '%s' "$sleep_out" | jqf "d['age_hours']")"
  mutate="$(printf '%s' "$sleep_out" | jqf "d['mutate']")"
  failed_vaults="$(printf '%s' "$sleep_out" | jqf "','.join(d.get('failed_vaults') or []) or 'none'")"
  summary="$(printf '%s' "$sleep_out" | jqf "f\"{d['vaults_scanned']} vaults, {d['engrams_scanned']} memories, {d['duplicate_groups']} dup groups, {d['diagnostic']} diagnostic, {d['forgotten']} forgotten\"")"
  if [[ "$failed_vaults" != "none" ]]; then
    fail "sleep run had failed vaults: $failed_vaults"
  elif (( $(printf '%.0f' "${age:-999}") > 48 )); then
    warn "last sleep run was ${age}h ago (expected nightly): $summary"
  else
    pass "sleep ran ${age}h ago (mutate=$mutate): $summary"
  fi
fi

# ── 8. Optional end-to-end write ─────────────────────────────────────────────
if (( WRITE_TEST )); then
  say "[5] End-to-end write (mac-jane -> Cortex -> observers -> forget)"
  python3 - "$MAC_SSH" <<'PY'
import json, os, socket, struct, subprocess, sys, time

SOCK = os.path.expanduser(os.environ.get("PHILOTIC_MAC_SOCKET", "~/.philotic/bjork/aiua-mac-jane.sock"))
VAULT = "self_agent-bjork-01"
CONCEPT = f"uat-write-path-{int(time.time())}"


def hotel(op, payload):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.settimeout(30); s.connect(SOCK)
    def send(o):
        b = json.dumps(o).encode(); s.sendall(struct.pack(">I", len(b)) + b)
    def recv():
        hdr = b""
        while len(hdr) < 4:
            hdr += s.recv(4 - len(hdr))
        (n,) = struct.unpack(">I", hdr)
        buf = b""
        while len(buf) < n:
            buf += s.recv(n - len(buf))
        return json.loads(buf)
    send({"operation": "register", "payload": {"guest_id": "uat-memory", "role": "membrane", "supported_tools": []}})
    recv()
    send({"operation": op, "payload": payload})
    out = recv()
    s.close()
    return out


task = {"action": "memory.write_forward", "op": "remember", "vault": VAULT, "concept": CONCEPT,
        "content": "UAT write-path probe; forgotten at the end of the run.",
        "tags": ["delete-me", "uat"], "metadata": None, "origin_node": "uat",
        "origin_agent": "uat", "session_id": "uat"}
resp = hotel("emit_task", {"target_node": os.environ.get("PHILOTIC_CORTEX_NODE", "vps-jane-aiua-01"),
                           "target_role": "hotel.memory_write_forward", "target_guest_id": None,
                           "task_json": json.dumps(task)})
if not resp.get("ok"):
    print("  FAIL  could not enqueue the forwarded write:", resp)
    sys.exit(1)

vps = os.environ.get("PHILOTIC_VPS_SSH", "deploy@jane-vps")
engram = None
for _ in range(20):
    time.sleep(3)
    out = subprocess.run(["ssh", "-o", "ConnectTimeout=15", "-o", "BatchMode=yes", vps,
                          f"grep -ah 'memory.write_forward applied' $(ls -t /opt/philotic/data/logs/aiua*.log | head -1) | tail -3"],
                         capture_output=True).stdout.decode()
    ids = [w.split("=", 1)[1] for line in out.splitlines() for w in line.split() if w.startswith("engram_id=")]
    if ids:
        engram = ids[-1]
        break
if not engram:
    print("  FAIL  the Cortex never logged the forwarded write")
    sys.exit(1)
print(f"  PASS  Cortex stored the forwarded write ({engram})")

wanted = json.dumps({VAULT: [engram]})
ok = True
for name, target in (("mac", ""), ("mbp", os.environ.get("PHILOTIC_MBP_SSH", "jaredlikes@100.79.239.64"))):
    for attempt in range(6):
        probe = os.path.join(os.path.dirname(os.path.abspath(sys.argv[0] or ".")), "uat_memory_probe.py")
        probe = probe if os.path.exists(probe) else "scripts/uat_memory_probe.py"
        if target:
            res = subprocess.run(["ssh", "-o", "ConnectTimeout=15", "-o", "BatchMode=yes", target, "python3 - muninn-have"],
                                 input=open(probe, "rb").read() + b"\n", capture_output=True)
            # send probe then ids: simplest is a temp copy
            res = subprocess.run(["ssh", "-o", "ConnectTimeout=15", "-o", "BatchMode=yes", target,
                                  f"cat > /tmp/uat_probe.py <<'EOF'\n{open(probe).read()}\nEOF\npython3 /tmp/uat_probe.py muninn-have; rm -f /tmp/uat_probe.py"],
                                 input=wanted.encode(), capture_output=True)
        else:
            res = subprocess.run(["python3", probe, "muninn-have"], input=wanted.encode(), capture_output=True)
        missing = json.loads(res.stdout.decode() or "{}")
        if not missing:
            print(f"  PASS  {name} received it by replication")
            break
        time.sleep(5)
    else:
        print(f"  FAIL  {name} never received it")
        ok = False

forget = {"action": "memory.write_forward", "op": "forget", "id": engram, "origin_node": "uat",
          "origin_agent": "uat", "session_id": "uat"}
resp = hotel("emit_task", {"target_node": os.environ.get("PHILOTIC_CORTEX_NODE", "vps-jane-aiua-01"),
                           "target_role": "hotel.memory_write_forward", "target_guest_id": None,
                           "task_json": json.dumps(forget)})
print("  PASS  cleanup forget forwarded" if resp.get("ok") else "  WARN  cleanup forget could not be enqueued")
sys.exit(0 if ok else 1)
PY
  if (( $? != 0 )); then FAILURES=$((FAILURES + 1)); fi
fi

say ""
say "=== $FAILURES failure(s), $WARNINGS warning(s) ==="
(( FAILURES == 0 ))
