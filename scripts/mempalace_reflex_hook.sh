#!/usr/bin/env bash
# Capture one agent lifecycle event into MemPalace's governed episodic lane.
#
# Input: transcript/summary file as $1, stdin, or PHILOTIC_WORK_TRACE.
# Output: the adapter's JSON receipt. Duplicate events are successful no-ops.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADAPTER_PATH="${PHILOTIC_EPISODIC_ADAPTER:-${SCRIPT_DIR}/mempalace_episode.py}"
API_URL="${PHILOTIC_EPISODIC_API_URL:-}"
CLIENT="${PHILOTIC_CLIENT:-codex}"
AGENT_OR_ROLE="${PHILOTIC_AGENT_ID:-codex}"
SESSION_ID="${PHILOTIC_SESSION_ID:-session-unknown}"
SOURCE_EVENT="${PHILOTIC_SOURCE_EVENT:-stop}"
SOURCE_EVENT_ID="${PHILOTIC_SOURCE_EVENT_ID:-}"
PRIVACY_CLASS="${PHILOTIC_PRIVACY_CLASS:-normal}"
RETENTION_CLASS="${PHILOTIC_RETENTION_CLASS:-days90}"
TRANSCRIPT_CONTENT=""

if [[ $# -gt 0 && -f "$1" ]]; then
    TRANSCRIPT_CONTENT="$(<"$1")"
elif [[ ! -t 0 ]]; then
    TRANSCRIPT_CONTENT="$(cat)"
elif [[ -n "${PHILOTIC_WORK_TRACE:-}" && -f "${PHILOTIC_WORK_TRACE}" ]]; then
    TRANSCRIPT_CONTENT="$(<"${PHILOTIC_WORK_TRACE}")"
fi

if [[ -z "$TRANSCRIPT_CONTENT" ]]; then
    printf '%s\n' '{"status":"skipped","reason":"no_content"}'
    exit 0
fi

CAPTURED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
CONTENT_HASH="sha256:$(printf '%s' "$TRANSCRIPT_CONTENT" | shasum -a 256 | awk '{print $1}')"
PAYLOAD="$(
    jq -n \
        --arg session_id "$SESSION_ID" \
        --arg client "$CLIENT" \
        --arg agent_or_role "$AGENT_OR_ROLE" \
        --arg captured_at "$CAPTURED_AT" \
        --arg source_event "$SOURCE_EVENT" \
        --arg source_event_id "$SOURCE_EVENT_ID" \
        --arg content_or_summary "$TRANSCRIPT_CONTENT" \
        --arg content_hash "$CONTENT_HASH" \
        --arg privacy_class "$PRIVACY_CLASS" \
        --arg retention_class "$RETENTION_CLASS" \
        '{
            session_id: $session_id,
            client: $client,
            agent_or_role: $agent_or_role,
            captured_at: $captured_at,
            source_event: $source_event,
            source_event_id: $source_event_id,
            content_or_summary: $content_or_summary,
            content_hash: $content_hash,
            provenance: {
                hook: "mempalace_reflex_hook",
                local_first: true
            },
            privacy_class: $privacy_class,
            retention_class: $retention_class,
            related_context_refs: [],
            metadata: {}
        }'
)"

if [[ -n "$API_URL" ]]; then
    RESPONSE="$(
        curl --silent --show-error --fail-with-body \
            -X POST "$API_URL" \
            -H "Content-Type: application/json" \
            --data-binary "$PAYLOAD"
    )"
else
    RESPONSE="$(printf '%s' "$PAYLOAD" | python3 "$ADAPTER_PATH" capture)"
fi

printf '%s\n' "$RESPONSE"
STATUS="$(printf '%s' "$RESPONSE" | jq -r '.status // "error"')"
case "$STATUS" in
    captured|duplicate|skipped)
        exit 0
        ;;
    *)
        exit 1
        ;;
esac
