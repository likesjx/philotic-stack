#!/usr/bin/env bash
# test-and-record.sh — run the workspace test suite and record results to the
# intel graph by default, instead of only when someone remembers
# `just test-and-record <target>`.
#
# Never fails `just test` just because the graph is down: an unreachable
# graph server is a one-line notice, not an error. The underlying test exit
# code is always propagated so CI/gating still sees real test failures.
#
# target_id resolution order:
#   1. $GRAPH_TEST_TARGET env var, if set.
#   2. Current branch codex/<slug> -> linked_proposal looked up via
#      GET /api/worktrees (populated by `phil graph scan`'s worktree scanner,
#      which already matches codex/<slug> branches to `doc:<slug>` proposals).
#   3. Fallback: the standing baseline node id `workspace:test-baseline`
#      (create-if-missing: /api/test-run upserts the target-linked TestRun
#      node regardless of whether `workspace:test-baseline` itself exists yet
#      as a Node — the TestedBy edge just points at that id).
#
# Usage: scripts/test-and-record.sh [-- <extra cargo test args>]
set -uo pipefail  # intentionally NOT -e: a test failure must not abort recording

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GRAPH_HOST="${GRAPH_HOST:-http://127.0.0.1:8900}"
OUTPUT_FILE="$(mktemp -t philotic-test-output.XXXXXX)"
trap 'rm -f "$OUTPUT_FILE"' EXIT

cd "$ROOT_DIR"

if [ "${1:-}" = "--" ]; then
    shift
fi

START_TIME=$(date +%s%N)
cargo test --workspace "$@" 2>&1 | tee "$OUTPUT_FILE"
TEST_EXIT_CODE=${PIPESTATUS[0]}
END_TIME=$(date +%s%N)
DURATION_MS=$(( (END_TIME - START_TIME) / 1000000 ))

# Sum pass/fail across every "test result:" line — a workspace run emits one
# per test binary, so tailing just the last line undercounts badly.
PASS_COUNT=0
FAIL_COUNT=0
while IFS= read -r line; do
    p=$(echo "$line" | grep -oE '[0-9]+ passed' | grep -oE '[0-9]+')
    f=$(echo "$line" | grep -oE '[0-9]+ failed' | grep -oE '[0-9]+')
    PASS_COUNT=$(( PASS_COUNT + ${p:-0} ))
    FAIL_COUNT=$(( FAIL_COUNT + ${f:-0} ))
done < <(grep '^test result:' "$OUTPUT_FILE" || true)
TEST_COUNT=$(( PASS_COUNT + FAIL_COUNT ))

# Graceful no-op when the graph server isn't up — not a `just test` failure.
if ! curl -s -o /dev/null -m 2 --fail "${GRAPH_HOST}/api/health"; then
    echo "[test-and-record] intel graph not reachable at ${GRAPH_HOST} — skipping test-run recording (not a failure)"
    exit "$TEST_EXIT_CODE"
fi

TARGET_ID="${GRAPH_TEST_TARGET:-}"

if [ -z "$TARGET_ID" ]; then
    BRANCH="$(git -C "$ROOT_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")"
    if [[ "$BRANCH" == codex/* ]]; then
        LINKED="$(curl -s -m 3 "${GRAPH_HOST}/api/worktrees" 2>/dev/null | \
          jq -r --arg b "$BRANCH" \
          '[.[] | select(.properties.branch == $b) | .properties.linked_proposal] | map(select(. != null and . != "")) | first // empty' \
          2>/dev/null)"
        if [ -n "$LINKED" ] && [ "$LINKED" != "null" ]; then
            TARGET_ID="$LINKED"
        fi
    fi
fi

if [ -z "$TARGET_ID" ]; then
    TARGET_ID="workspace:test-baseline"
fi

echo "[test-and-record] recording ${PASS_COUNT}/${TEST_COUNT} passed (${FAIL_COUNT} failed) to target_id=${TARGET_ID}"

curl -s -m 5 -X POST "${GRAPH_HOST}/api/test-run" \
  -H "Content-Type: application/json" \
  -d "{\"target_id\":\"${TARGET_ID}\",\"test_count\":${TEST_COUNT},\"pass_count\":${PASS_COUNT},\"fail_count\":${FAIL_COUNT},\"duration_ms\":${DURATION_MS}}" \
  > /dev/null || echo "[test-and-record] warning: recording POST failed (graph may have gone down mid-run)"

exit "$TEST_EXIT_CODE"
