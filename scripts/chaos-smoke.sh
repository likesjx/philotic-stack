#!/usr/bin/env bash
# scripts/chaos-smoke.sh — Substrate Hardening Slice S4 (chaos-smokes).
#
# Scheduled, bounded chaos smoke against ONE designated hotel. Executes ONE
# scenario per run (round-robin between the two real scenarios by default, or
# pass a scenario name explicitly).
#
# Scenarios:
#   guest-kill      SIGKILL a designated low-stakes guest process (default:
#                   tool-runner's default guest_id "tool-runner-01" — see
#                   crates/tool-runner/src/main.rs). Asserts the supervisor
#                   respawns it (new, live PID in `materialized_guests`)
#                   within a budget window, and that no heal-queue work item
#                   for that guest is left stuck open afterward.
#   config-corrupt  Writes a bogus value to a dedicated sacrificial config
#                   key (chaos_smoke.canary_value — nothing in the codebase
#                   reads this key). Asserts the hotel stays healthy
#                   (`phil doctor` + heal-queue depth), then restores the key.
#   mesh-peer-drop  STUB. Not implemented — see the function body below for
#                   why (safely dropping/restoring a live mesh UDP peer link
#                   from an external script is not tractable without a
#                   dedicated test-hook IPC verb, which does not exist yet).
#                   Never selected by the automatic round-robin; only runs
#                   when named explicitly, and never files or records
#                   anything — a stub must not fabricate evidence.
#
# Safety rails (all enforced in code here, not just documented):
#   1. PHILOTIC_CHAOS_SMOKE_DISABLE=1 short-circuits before ANYTHING else —
#      checked first, unconditionally.
#   2. Refuses to run (no action taken) if `phil doctor` reports unhealthy
#      for the target hotel, or if the heal queue already has more than
#      PHILOTIC_CHAOS_HEAL_QUEUE_MAX open items.
#   3. guest-kill only ever targets PHILOTIC_CHAOS_GUEST_ID, checked against a
#      hard denylist (philote agent guests, membrane, heal-dispatcher).
#   4. config-corrupt only ever touches PHILOTIC_CHAOS_CONFIG_KEY, which
#      cannot be pointed outside the "chaos_smoke." namespace.
#   5. --dry-run prints the full plan (including the live pre-flight health
#      read) and exits 0 without killing anything, writing any config, or
#      POSTing anything to the intel graph.
#   6. Every action taken is logged with a timestamp.
#
# Evidence / filing:
#   On scenario FAILURE: files an intel-graph decision record via
#   POST :8900/api/decide — the same A3 REST pattern
#   crates/heal-dispatcher/src/main.rs's push_intel_graph_record() uses
#   (there is no dedicated proposal-create REST route; see that function's
#   doc comment) — with the evidence, and exits nonzero.
#   On scenario SUCCESS: records a TestRun via POST :8900/api/test-run
#   (target_id doc:substrate-hardening-proposal) so `phil graph green` shows
#   chaos evidence, mirroring scripts/test-and-record.sh's pattern. Both are
#   best-effort against an unreachable graph server — a down graph server
#   never hides a real chaos-smoke failure; the script's own log is the
#   durable record either way.
#
# Heal-queue reads are never allowed to fail silent-clean: `heal_list_or_fail`
# (below) treats a nonzero/unreachable `phil heal list --hotel "$HOTEL"` as
# an unverified state, never as "0 open items" — this closes a real gap
# found while building this slice: `phil heal` previously hardcoded hotel
# "aiua" regardless of --hotel, so a heal-queue check against any
# differently-named hotel would silently connect to a socket that doesn't
# exist and get treated as "clean." Fixed in the same commit
# (crates/philotic-web/src/heal.rs now takes --hotel, mirroring phil config).
#
# Usage:
#   scripts/chaos-smoke.sh [guest-kill|config-corrupt|mesh-peer-drop] [--dry-run]
#   PHILOTIC_CHAOS_SMOKE_DISABLE=1 scripts/chaos-smoke.sh   # kill switch
#
# Env:
#   PHILOTIC_CHAOS_SMOKE_DISABLE   set to 1 to disable entirely (checked first)
#   PHILOTIC_CHAOS_HOTEL           hotel name for phil doctor/config --hotel
#                                  (default: "default", matching phil's own
#                                  CLI default — MUST be overridden to the
#                                  real deployment's hotel_name in production)
#   PHILOTIC_CHAOS_PROFILE         profile for `phil doctor --profile` (default:
#                                  ambient PHILOTIC_PROFILE / doctor's own fallback)
#   PHILOTIC_CHAOS_GUEST_ID        sacrificial guest_id for guest-kill
#                                  (default: tool-runner-01 — a PLACEHOLDER;
#                                  override to match the real deployment)
#   PHILOTIC_CHAOS_CONFIG_KEY      sacrificial config key for config-corrupt
#                                  (default: chaos_smoke.canary_value; must
#                                  stay inside the "chaos_smoke." namespace)
#   PHILOTIC_CHAOS_BUDGET_SECS     respawn/health budget window (default: 120)
#   PHILOTIC_CHAOS_POLL_SECS       poll interval within the budget (default: 5)
#   PHILOTIC_CHAOS_OBSERVE_SECS    config-corrupt post-write settle time before
#                                  the health re-check (default: 15)
#   PHILOTIC_CHAOS_HEAL_QUEUE_MAX  pre-flight refusal threshold (default: 3)
#   GRAPH_HOST                     intel-graph REST base (default: http://127.0.0.1:8900)
#   PHIL_BIN                       path to the `phil` binary (default: `phil` on PATH)
set -uo pipefail  # NOT -e: a failed assertion must still run cleanup/restore/filing

# ── Kill switch (rail 1) — checked before reading any other env or arg ─────
if [[ "${PHILOTIC_CHAOS_SMOKE_DISABLE:-0}" == "1" ]]; then
    echo "[chaos-smoke] PHILOTIC_CHAOS_SMOKE_DISABLE=1 — disabled, doing nothing."
    exit 0
fi

# ── Config ───────────────────────────────────────────────────────────────
HOTEL="${PHILOTIC_CHAOS_HOTEL:-default}"
PROFILE="${PHILOTIC_CHAOS_PROFILE:-}"
GUEST_ID="${PHILOTIC_CHAOS_GUEST_ID:-tool-runner-01}"
CONFIG_KEY="${PHILOTIC_CHAOS_CONFIG_KEY:-chaos_smoke.canary_value}"
BUDGET_SECS="${PHILOTIC_CHAOS_BUDGET_SECS:-120}"
POLL_SECS="${PHILOTIC_CHAOS_POLL_SECS:-5}"
OBSERVE_SECS="${PHILOTIC_CHAOS_OBSERVE_SECS:-15}"
HEAL_QUEUE_MAX="${PHILOTIC_CHAOS_HEAL_QUEUE_MAX:-3}"
GRAPH_HOST="${GRAPH_HOST:-http://127.0.0.1:8900}"
PHIL_BIN="${PHIL_BIN:-phil}"

DRY_RUN=0
SCENARIO=""
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=1 ;;
        guest-kill|config-corrupt|mesh-peer-drop) SCENARIO="$arg" ;;
        -h|--help)
            sed -n '2,80p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "[chaos-smoke] unrecognized argument: $arg" >&2
            exit 2
            ;;
    esac
done

# Round-robin between the two REAL scenarios when none is named. mesh-peer-drop
# is a stub — never auto-selected, only runnable by explicit name.
if [[ -z "$SCENARIO" ]]; then
    WEEK="$(date +%V)"
    if (( WEEK % 2 == 0 )); then
        SCENARIO="guest-kill"
    else
        SCENARIO="config-corrupt"
    fi
fi

RUN_ID="chaos-smoke-$(date +%s)-$$"
START_TS=$(date +%s)

log() {
    # stderr, deliberately: several functions (preflight_check, scenario_*)
    # are called via command substitution to capture their real stdout
    # payload (db_path, heal counts) — log lines must never leak into that
    # captured value.
    printf '[chaos-smoke %s] %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$1" >&2
}

# ── JSON helpers (python3, matching scripts/smoke-agent-workflow.sh's convention) ──
json_field() {
    # json_field '<json>' '<dotted.path>' — best-effort, prints "" on failure.
    python3 -c '
import sys, json
data = json.loads(sys.argv[1]) if sys.argv[1].strip() else {}
path = sys.argv[2].split(".")
cur = data
for p in path:
    if isinstance(cur, dict):
        cur = cur.get(p)
    else:
        cur = None
        break
print(cur if cur is not None else "")
' "$1" "$2" 2>/dev/null || true
}

# ── Rail 3: guest-kill denylist ─────────────────────────────────────────────
guest_id_denied() {
    local gid="$1"
    case "$gid" in
        agent-*) return 0 ;;      # philote incarnations
        *membrane*) return 0 ;;   # any membrane variant (poll lease risk)
        heal-dispatcher) return 0 ;;
        "") return 0 ;;
        *) return 1 ;;
    esac
}

# ── Rail 4: config-corrupt namespace guard ──────────────────────────────────
config_key_denied() {
    case "$1" in
        chaos_smoke.*) return 1 ;;
        *) return 0 ;;
    esac
}

# ── Pre-flight health gate (rail 2) ─────────────────────────────────────────
# Returns 0 and prints "db_path\nheal_open_count" (two lines) on a healthy
# hotel; returns nonzero (nothing usable printed) when unhealthy or doctor
# itself failed to run.
preflight_check() {
    local doctor_args=(doctor --hotel "$HOTEL" --json)
    [[ -n "$PROFILE" ]] && doctor_args+=(--profile "$PROFILE")

    local doctor_out doctor_exit
    doctor_out="$("$PHIL_BIN" "${doctor_args[@]}" 2>&1)"
    doctor_exit=$?

    if [[ $doctor_exit -ne 0 && $doctor_exit -ne 1 ]]; then
        log "REFUSING: phil doctor could not run (exit $doctor_exit): $doctor_out"
        return 2
    fi

    local ok db_path
    ok="$(json_field "$doctor_out" "ok")"
    db_path="$(json_field "$doctor_out" "db_path")"
    if [[ "$ok" != "True" ]]; then
        log "REFUSING: phil doctor reports the hotel is NOT healthy (hotel=$HOTEL, profile=${PROFILE:-<ambient>}). Findings:"
        echo "$doctor_out" | sed 's/^/  /' >&2
        return 1
    fi
    if [[ -z "$db_path" ]]; then
        log "REFUSING: phil doctor did not report a db_path — cannot verify guest state safely."
        return 2
    fi

    local heal_out
    if ! heal_out="$(heal_list_or_fail)"; then
        # heal_list_or_fail already logged the specific IPC error. Treat an
        # unreadable heal queue as unverified, never as "0 open items" —
        # silently defaulting to clean here is exactly the "never silently
        # absent" failure Standing Rule 2 forbids.
        log "REFUSING: could not read the heal queue for hotel=$HOTEL — cannot confirm the hotel isn't already degraded."
        return 2
    fi
    local open_count
    if echo "$heal_out" | grep -q "no open heal work items"; then
        open_count=0
    else
        open_count="$(echo "$heal_out" | grep -c '^[a-zA-Z0-9._-]\+  \[' || true)"
    fi
    if [[ "$open_count" -gt "$HEAL_QUEUE_MAX" ]]; then
        log "REFUSING: heal queue has $open_count open item(s) (max $HEAL_QUEUE_MAX). Not adding chaos on top of an already-degraded hotel."
        echo "$heal_out" | sed 's/^/  /' >&2
        return 1
    fi

    printf '%s\n%s\n' "$db_path" "$open_count"
    return 0
}

# Runs `phil heal list --hotel "$HOTEL"`, printing raw output on stdout and
# returning 0 only on success. On ANY failure (unreachable socket, IPC
# error) it logs loudly and returns nonzero with nothing on stdout —
# callers must never default a read failure to "0 open items, must be
# clean": an unreadable heal queue is unverified, not verified-clean.
heal_list_or_fail() {
    local out exit_code
    out="$("$PHIL_BIN" heal list --hotel "$HOTEL" 2>&1)"
    exit_code=$?
    if [[ $exit_code -ne 0 ]]; then
        log "heal-queue read FAILED (hotel=$HOTEL, exit=$exit_code): $out"
        return 1
    fi
    printf '%s\n' "$out"
    return 0
}

heal_open_for_guest() {
    # Count OPEN heal work items whose guest= field matches $1. Prints the
    # sentinel "unreadable" instead of a number when the heal queue itself
    # cannot be read — callers MUST treat that as "cannot confirm
    # resolution," never as "confirmed resolved" (see heal_list_or_fail).
    local gid="$1"
    local heal_out
    if ! heal_out="$(heal_list_or_fail)"; then
        echo "unreadable"
        return
    fi
    echo "$heal_out" | grep -c "guest=${gid}\b" || true
}

# ── Filing (A3 REST pattern) / test-run recording ───────────────────────────
graph_reachable() {
    curl -s -o /dev/null -m 3 --fail "${GRAPH_HOST}/api/health" 2>/dev/null
}

file_failure() {
    local scenario="$1" reason="$2"
    shift 2
    local evidence_json="[]"
    if [[ $# -gt 0 ]]; then
        evidence_json="$(printf '%s\n' "$@" | python3 -c 'import sys, json; print(json.dumps([l.rstrip() for l in sys.stdin]))')"
    fi
    log "FILING intel-graph proposal evidence for scenario=$scenario"
    if ! graph_reachable; then
        log "  intel graph not reachable at ${GRAPH_HOST} — evidence stays in this run's log only (best-effort mirror skipped)"
        return
    fi
    local body
    body="$(python3 -c '
import json, sys
scenario, reason, hotel, run_id, evidence_json = sys.argv[1:6]
body = {
    "target_node": "doc:substrate-hardening-proposal",
    "action": "chaos_smoke_scenario_failed",
    "to_value": f"{scenario}:{run_id}",
    "reason": reason,
    "agent": "chaos-smoke",
    "evidence": json.loads(evidence_json),
    "reversal": "none — evidence-only filing; operator reviews and files/handles the real defect",
    "trust": "observed",
}
print(json.dumps(body))
' "$scenario" "$reason" "$HOTEL" "$RUN_ID" "$evidence_json")"
    if curl -s -m 5 -X POST "${GRAPH_HOST}/api/decide" \
        -H "Content-Type: application/json" \
        -d "$body" >/dev/null; then
        log "  filed to ${GRAPH_HOST}/api/decide (target doc:substrate-hardening-proposal)"
    else
        log "  WARNING: POST /api/decide failed (best-effort, not fatal)"
    fi
}

record_success() {
    local scenario="$1" duration_ms="$2"
    if ! graph_reachable; then
        log "intel graph not reachable at ${GRAPH_HOST} — skipping test-run recording (not a failure)"
        return
    fi
    if curl -s -m 5 -X POST "${GRAPH_HOST}/api/test-run" \
        -H "Content-Type: application/json" \
        -d "{\"target_id\":\"doc:substrate-hardening-proposal\",\"test_count\":1,\"pass_count\":1,\"fail_count\":0,\"duration_ms\":${duration_ms}}" \
        >/dev/null; then
        log "recorded green test-run for scenario=$scenario to ${GRAPH_HOST}/api/test-run"
    else
        log "WARNING: POST /api/test-run failed (best-effort, not fatal)"
    fi
}

# ── Scenario: guest-kill ─────────────────────────────────────────────────
scenario_guest_kill() {
    local db_path="$1"

    if guest_id_denied "$GUEST_ID"; then
        log "REFUSING: guest_id '$GUEST_ID' matches the denylist (philote/membrane/heal-dispatcher). This scenario only ever targets low-stakes guests."
        return 2
    fi

    local before_pid
    before_pid="$(sqlite3 -readonly "$db_path" "SELECT active_pid FROM materialized_guests WHERE guest_id='${GUEST_ID}';" 2>/dev/null || true)"

    if [[ $DRY_RUN -eq 1 ]]; then
        log "[dry-run] would SIGKILL guest_id=$GUEST_ID (current active_pid=${before_pid:-<none>}), then poll active_pid up to ${BUDGET_SECS}s (every ${POLL_SECS}s) for a new live PID, then check 'phil heal list' for any open item on guest=$GUEST_ID."
        return 0
    fi

    if [[ -z "$before_pid" ]]; then
        log "REFUSING: guest_id=$GUEST_ID has no active_pid in $db_path — it is not currently materialized, nothing to kill."
        return 2
    fi
    if ! kill -0 "$before_pid" 2>/dev/null; then
        log "REFUSING: guest_id=$GUEST_ID's recorded active_pid=$before_pid is not alive — hotel state already inconsistent; not adding chaos."
        return 2
    fi

    local heal_before
    heal_before="$(heal_open_for_guest "$GUEST_ID")"

    log "SIGKILL guest_id=$GUEST_ID pid=$before_pid"
    kill -KILL "$before_pid" 2>&1 | sed 's/^/  /' || true

    local elapsed=0 after_pid=""
    while (( elapsed < BUDGET_SECS )); do
        sleep "$POLL_SECS"
        elapsed=$(( elapsed + POLL_SECS ))
        after_pid="$(sqlite3 -readonly "$db_path" "SELECT active_pid FROM materialized_guests WHERE guest_id='${GUEST_ID}';" 2>/dev/null || true)"
        if [[ -n "$after_pid" && "$after_pid" != "$before_pid" ]] && kill -0 "$after_pid" 2>/dev/null; then
            log "respawn confirmed after ~${elapsed}s: guest_id=$GUEST_ID new active_pid=$after_pid (was $before_pid)"
            break
        fi
        log "  waiting for respawn... (${elapsed}s/${BUDGET_SECS}s, active_pid=${after_pid:-<none>})"
    done

    if [[ -z "$after_pid" || "$after_pid" == "$before_pid" ]] || ! kill -0 "$after_pid" 2>/dev/null; then
        file_failure "guest-kill" \
            "guest_id=$GUEST_ID did not respawn to a new live PID within ${BUDGET_SECS}s of SIGKILL (before_pid=$before_pid, last-seen active_pid=${after_pid:-<none>})" \
            "hotel:$HOTEL" "guest_id:$GUEST_ID" "before_pid:$before_pid" "budget_secs:$BUDGET_SECS" "db_path:$db_path"
        return 1
    fi

    local heal_after
    heal_after="$(heal_open_for_guest "$GUEST_ID")"
    if [[ "$heal_after" == "unreadable" ]]; then
        file_failure "guest-kill" \
            "guest_id=$GUEST_ID respawned (pid=$after_pid) but the heal queue could not be read afterward (hotel=$HOTEL) — cannot confirm no heal-queue entry is stuck open for it" \
            "hotel:$HOTEL" "guest_id:$GUEST_ID" "heal_open_before:$heal_before"
        return 1
    fi
    if [[ "$heal_after" -gt 0 ]]; then
        file_failure "guest-kill" \
            "guest_id=$GUEST_ID respawned (pid=$after_pid) but $heal_after heal-queue item(s) remain open for it after the SIGKILL (were $heal_before before)" \
            "hotel:$HOTEL" "guest_id:$GUEST_ID" "heal_open_before:$heal_before" "heal_open_after:$heal_after"
        return 1
    fi

    log "PASS: guest-kill — respawned within budget, no stuck heal-queue entries."
    return 0
}

# ── Scenario: config-corrupt ────────────────────────────────────────────
scenario_config_corrupt() {
    local db_path="$1"

    if config_key_denied "$CONFIG_KEY"; then
        log "REFUSING: PHILOTIC_CHAOS_CONFIG_KEY='$CONFIG_KEY' is outside the chaos_smoke. namespace — refusing to touch it."
        return 2
    fi

    local before_value
    before_value="$("$PHIL_BIN" config get "$CONFIG_KEY" --hotel "$HOTEL" 2>&1)" || true

    if [[ $DRY_RUN -eq 1 ]]; then
        log "[dry-run] would write a bogus value to config key '$CONFIG_KEY' (current value: $before_value), wait ${OBSERVE_SECS}s, re-run phil doctor + heal-queue check, then restore the key to its prior value."
        return 0
    fi

    local restore_value="$before_value"
    if [[ -z "$restore_value" || "$restore_value" == "null" ]]; then
        restore_value='"chaos_smoke_baseline"'
    fi

    # cleanup runs even on early return/failure below
    local restored=0
    cleanup_config() {
        if [[ $restored -eq 0 ]]; then
            if "$PHIL_BIN" config set "$CONFIG_KEY" "$restore_value" --hotel "$HOTEL" >/dev/null 2>&1; then
                log "restored $CONFIG_KEY to prior value"
            else
                log "WARNING: failed to restore $CONFIG_KEY — manual check needed: phil config get $CONFIG_KEY --hotel $HOTEL"
            fi
            restored=1
        fi
    }
    trap cleanup_config RETURN

    local bogus_value
    bogus_value="$(python3 -c 'import json,time; print(json.dumps({"__chaos_smoke_bogus__": True, "ts": int(time.time())}))')"
    log "writing bogus value to $CONFIG_KEY: $bogus_value"
    if ! "$PHIL_BIN" config set "$CONFIG_KEY" "$bogus_value" --hotel "$HOTEL" 2>&1 | sed 's/^/  /'; then
        log "REFUSING: could not even write the sacrificial config key — IPC may be down; aborting before further assertions."
        return 2
    fi

    log "observing for ${OBSERVE_SECS}s..."
    sleep "$OBSERVE_SECS"

    local doctor_args=(doctor --hotel "$HOTEL" --json)
    [[ -n "$PROFILE" ]] && doctor_args+=(--profile "$PROFILE")
    local doctor_out ok
    doctor_out="$("$PHIL_BIN" "${doctor_args[@]}" 2>&1)"
    ok="$(json_field "$doctor_out" "ok")"

    if [[ "$ok" != "True" ]]; then
        file_failure "config-corrupt" \
            "hotel became unhealthy after writing a bogus value to sacrificial key $CONFIG_KEY (phil doctor findings below)" \
            "hotel:$HOTEL" "config_key:$CONFIG_KEY" "doctor_output:$doctor_out"
        return 1
    fi

    log "PASS: config-corrupt — hotel stayed healthy with a bogus canary value in place."
    return 0
}

# ── Scenario: mesh-peer-drop (STUB) ─────────────────────────────────────
scenario_mesh_peer_drop() {
    cat <<'EOF'
[chaos-smoke] mesh-peer-drop is a STUB — not implemented.

TODO(mesh-peer-drop): safely dropping and restoring a live mesh UDP peer link
(BeaconMessage on :8999, HMAC-PSK optional — see CLAUDE.md "Inter-hotel
(Mesh/UDP)") from an external script is not tractable today without a
dedicated test-hook IPC verb: there is no IPC request to (a) mark a specific
peer's mesh_cursors entry as temporarily unreachable without touching the
real mesh_events ledger, or (b) simulate packet loss to one peer without a
firewall/pf rule change that risks affecting unrelated traffic on a shared
host. Either path needs its own small proposal (a `SimulateMeshPeerDrop`
IPC verb, or a scoped pf anchor a script can install/remove atomically)
before this scenario can assert anything real. Filing that as a named next
seam rather than faking a pass/fail here.

This scenario never files a proposal and never records a test-run — a stub
must not fabricate evidence either way.
EOF
    return 3
}

# ── Main ─────────────────────────────────────────────────────────────────
main() {
    log "scenario=$SCENARIO hotel=$HOTEL profile=${PROFILE:-<ambient>} dry_run=$DRY_RUN run_id=$RUN_ID"

    if [[ "$SCENARIO" == "mesh-peer-drop" ]]; then
        scenario_mesh_peer_drop
        exit 0
    fi

    if ! command -v "$PHIL_BIN" >/dev/null 2>&1; then
        log "REFUSING: '$PHIL_BIN' not found on PATH (set PHIL_BIN=/path/to/phil, or 'just phil-install')."
        exit 2
    fi
    if [[ "$SCENARIO" == "guest-kill" ]] && ! command -v sqlite3 >/dev/null 2>&1; then
        log "REFUSING: sqlite3 not found on PATH — required to verify guest respawn read-only."
        exit 2
    fi

    local preflight_out
    if ! preflight_out="$(preflight_check)"; then
        # preflight_check already logged the specific reason. A refusal is a
        # safety abort, not a chaos-scenario failure — no proposal filed.
        exit 1
    fi
    local db_path
    db_path="$(echo "$preflight_out" | head -1)"
    log "pre-flight OK (hotel healthy, heal queue within threshold). db_path=$db_path"

    local status=0
    case "$SCENARIO" in
        guest-kill)     scenario_guest_kill "$db_path"; status=$? ;;
        config-corrupt) scenario_config_corrupt "$db_path"; status=$? ;;
        *)
            log "unknown scenario: $SCENARIO" >&2
            exit 2
            ;;
    esac

    if [[ $DRY_RUN -eq 1 ]]; then
        log "[dry-run] complete — nothing was touched."
        exit 0
    fi

    local end_ts duration_ms
    end_ts=$(date +%s)
    duration_ms=$(( (end_ts - START_TS) * 1000 ))

    if [[ $status -eq 0 ]]; then
        record_success "$SCENARIO" "$duration_ms"
        log "chaos-smoke scenario=$SCENARIO: PASS (${duration_ms}ms)"
        exit 0
    elif [[ $status -eq 1 ]]; then
        log "chaos-smoke scenario=$SCENARIO: FAIL (${duration_ms}ms) — evidence filed above"
        exit 1
    else
        # status 2: pre-scenario refusal inside the scenario function itself
        # (denylist hit, guest not materialized, etc.) — not a chaos failure.
        log "chaos-smoke scenario=$SCENARIO: REFUSED — no action taken"
        exit 1
    fi
}

# Guarded so scripts/tests/chaos-smoke-unit-test.sh can `source` this file
# (to unit-test json_field / guest_id_denied / config_key_denied / the
# heal-open-count parsing) without triggering a real run.
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
