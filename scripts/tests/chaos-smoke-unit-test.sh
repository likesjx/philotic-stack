#!/usr/bin/env bash
# Unit tests for scripts/chaos-smoke.sh's assertion/parsing logic — the
# pieces that decide pass/fail/refuse without touching any real hotel:
#   - json_field()            doctor-JSON field extraction
#   - guest_id_denied()       guest-kill denylist (rail 3)
#   - config_key_denied()     config-corrupt namespace guard (rail 4)
#   - heal_open_for_guest()   `phil heal list` open-item-for-guest counting
#   - preflight_check()'s open_count parsing of `phil heal list` output
#
# No real `phil`/aiua/sqlite3 process is started — `phil heal list` calls are
# intercepted by a fake `phil` stub placed first on PATH, matching how
# other IPC-shaped logic in this repo is unit-tested against fixture data
# rather than a live daemon.
#
# Usage: bash scripts/tests/chaos-smoke-unit-test.sh
set -uo pipefail  # NOT -e: we want every assertion to run and report, not stop at the first failure

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHAOS_SMOKE="${ROOT_DIR}/scripts/chaos-smoke.sh"

PASS=0
FAIL=0

assert_eq() {
    local label="$1" expected="$2" actual="$3"
    if [[ "$expected" == "$actual" ]]; then
        printf '  \033[0;32m✓\033[0m %s\n' "$label"
        (( PASS += 1 )) || true
    else
        printf '  \033[0;31m✗\033[0m %s (expected %q, got %q)\n' "$label" "$expected" "$actual"
        (( FAIL += 1 )) || true
    fi
}

assert_true() {
    local label="$1"
    if "$2" "$3"; then
        printf '  \033[0;32m✓\033[0m %s\n' "$label"
        (( PASS += 1 )) || true
    else
        printf '  \033[0;31m✗\033[0m %s (expected true)\n' "$label"
        (( FAIL += 1 )) || true
    fi
}

assert_false() {
    local label="$1"
    if ! "$2" "$3"; then
        printf '  \033[0;32m✓\033[0m %s\n' "$label"
        (( PASS += 1 )) || true
    else
        printf '  \033[0;31m✗\033[0m %s (expected false)\n' "$label"
        (( FAIL += 1 )) || true
    fi
}

# Source chaos-smoke.sh for its function definitions without running main()
# — the BASH_SOURCE-vs-0 guard at the bottom of the file makes this safe.
PHILOTIC_CHAOS_SMOKE_DISABLE=0 source "$CHAOS_SMOKE"

echo "== json_field() =="
DOCTOR_JSON='{"ok": true, "hotel": "default", "db_path": "/tmp/x/context.db", "checks_run": 12, "nested": {"a": 1}}'
assert_eq "extracts top-level bool as python str" "True" "$(json_field "$DOCTOR_JSON" "ok")"
assert_eq "extracts top-level string" "default" "$(json_field "$DOCTOR_JSON" "hotel")"
assert_eq "extracts top-level path" "/tmp/x/context.db" "$(json_field "$DOCTOR_JSON" "db_path")"
assert_eq "missing field returns empty" "" "$(json_field "$DOCTOR_JSON" "does_not_exist")"
assert_eq "empty input returns empty" "" "$(json_field "" "ok")"
assert_eq "malformed json returns empty, not a crash" "" "$(json_field "not-json-at-all" "ok")"
DOCTOR_JSON_FALSE='{"ok": false, "db_path": "/tmp/y/context.db"}'
assert_eq "extracts false correctly (not truthy-matched)" "False" "$(json_field "$DOCTOR_JSON_FALSE" "ok")"

echo "== guest_id_denied() (rail 3) =="
assert_true  "philote guest denied"        guest_id_denied "agent-jane"
assert_true  "philote-prefixed denied"     guest_id_denied "agent-bjork-01"
assert_true  "membrane denied"             guest_id_denied "membrane"
assert_true  "membrane-telegram denied"    guest_id_denied "membrane-telegram-01"
assert_true  "heal-dispatcher denied"      guest_id_denied "heal-dispatcher"
assert_true  "empty guest id denied"       guest_id_denied ""
assert_false "tool-runner allowed"         guest_id_denied "tool-runner-01"
assert_false "table-datasource allowed"    guest_id_denied "table-datasource-01"

echo "== config_key_denied() (rail 4) =="
assert_false "chaos_smoke.canary_value allowed"   config_key_denied "chaos_smoke.canary_value"
assert_false "any chaos_smoke.* key allowed"      config_key_denied "chaos_smoke.other_key"
assert_true  "telegram_bot_token denied"          config_key_denied "telegram_bot_token"
assert_true  "vault key denied"                   config_key_denied "vault_beacon_telegram_bot_token"
assert_true  "empty key denied"                   config_key_denied ""

# Rail 6 protects real muninn vaults from the memory-token-wipe drill. These
# are the exact vault names live on the fleet (see config:vault_registry) —
# if any of them ever becomes drillable, a chaos run can strand real memory.
echo "== vault_name_denied() (rail 6, memory-token-wipe) =="
assert_false "chaos_smoke_token_drill allowed"    vault_name_denied "chaos_smoke_token_drill"
assert_false "any chaos_smoke* vault allowed"     vault_name_denied "chaos_smoke_alt"
assert_true  "default vault denied"               vault_name_denied "default"
assert_true  "self_agent-bjork-01 denied"         vault_name_denied "self_agent-bjork-01"
assert_true  "self_agent-coach denied"            vault_name_denied "self_agent-coach"
assert_true  "user_likesjx denied"                vault_name_denied "user_likesjx"
assert_true  "session_* vault denied"             vault_name_denied "session_abc123"
assert_true  "api-key vault denied"               vault_name_denied "openai_api_key"
assert_true  "empty vault denied"                 vault_name_denied ""

echo "== heal_open_for_guest() + preflight_check() open_count parsing =="
FAKE_PHIL_DIR="$(mktemp -d)"
trap 'rm -rf "$FAKE_PHIL_DIR"' EXIT

write_fake_phil() {
    # $1 = body of the `heal list` branch's stdout
    cat >"${FAKE_PHIL_DIR}/phil" <<FAKE_EOF
#!/usr/bin/env bash
if [[ "\$1" == "heal" && "\$2" == "list" ]]; then
cat <<'HEAL_EOF'
$1
HEAL_EOF
    exit 0
fi
echo "fake phil: unhandled args: \$*" >&2
exit 2
FAKE_EOF
    chmod +x "${FAKE_PHIL_DIR}/phil"
}

write_fake_phil "no open heal work items"
PHIL_BIN="${FAKE_PHIL_DIR}/phil"
assert_eq "heal_open_for_guest: none open -> 0" "0" "$(heal_open_for_guest "tool-runner-01")"

write_fake_phil "wi-1  [open]  pattern=oom guest=tool-runner-01 count=2
wi-2  [open]  pattern=crash guest=agent-jane count=1"
PHIL_BIN="${FAKE_PHIL_DIR}/phil"
assert_eq "heal_open_for_guest: matches only the named guest" "1" "$(heal_open_for_guest "tool-runner-01")"
assert_eq "heal_open_for_guest: no match for an unrelated guest" "0" "$(heal_open_for_guest "table-datasource-01")"

write_fake_phil "wi-1  [open]  pattern=oom guest=tool-runner-01 count=2
wi-2  [open]  pattern=oom-repeat guest=tool-runner-01 count=1"
PHIL_BIN="${FAKE_PHIL_DIR}/phil"
assert_eq "heal_open_for_guest: counts multiple open items for the same guest" "2" "$(heal_open_for_guest "tool-runner-01")"

# Regression coverage for the "silently reports clean when the heal queue is
# unreadable" bug caught in review: before the fix, an unreachable/erroring
# `phil heal list` produced text that matched neither "no open heal work
# items" nor the item-line regex, so open_count/heal_open_for_guest both
# defaulted to 0 — a wrong-hotel or down-daemon read looked identical to a
# genuinely clean heal queue. heal_list_or_fail() must turn that into an
# explicit, non-numeric "unreadable" sentinel instead.
write_fake_phil_failing() {
    cat >"${FAKE_PHIL_DIR}/phil" <<'FAKE_EOF'
#!/usr/bin/env bash
if [[ "$1" == "heal" && "$2" == "list" ]]; then
    echo "Error: connect to aiua at /tmp/philotic-aiua.sock" >&2
    echo "Caused by:" >&2
    echo "  0: Failed to connect to hotel IPC socket at /tmp/philotic-aiua.sock" >&2
    echo "  1: No such file or directory (os error 2)" >&2
    exit 1
fi
echo "fake phil: unhandled args: $*" >&2
exit 2
FAKE_EOF
    chmod +x "${FAKE_PHIL_DIR}/phil"
}

write_fake_phil_failing
PHIL_BIN="${FAKE_PHIL_DIR}/phil"
assert_eq "heal_open_for_guest: unreadable heal queue -> sentinel, not 0" "unreadable" "$(heal_open_for_guest "tool-runner-01")"
assert_false "heal_list_or_fail: propagates failure (nonzero exit), not a silent empty success" heal_list_or_fail ""

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
if [[ "$FAIL" -eq 0 ]]; then
    printf '\033[0;32mAll %d checks passed\033[0m\n' "$PASS"
    exit 0
else
    printf '\033[0;31m%d checks failed, %d passed\033[0m\n' "$FAIL" "$PASS"
    exit 1
fi
