#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MUNINN_BASE_URL="${MUNINN_BASE_URL:-http://127.0.0.1:8750/mcp}"
PERPLEXITY_MCP_URL="${PERPLEXITY_MCP_URL:-https://mcp.jaredlikes.com/mcp}"
LIFEGRAPH_MCP_URL="${LIFEGRAPH_MCP_URL:-https://mcp.jaredlikes.com/lifegraph/mcp}"
RUN_REMOTE="${RUN_REMOTE:-0}"

usage() {
  cat <<EOF
Usage: $0 [safe|codex|muninn-local|perplexity-tools|perplexity-capture|lifegraph-tools|lifegraph-recall|remote-native|all|live]

Safe modes do not require live bearer tokens and never print secrets.

Environment:
  MUNINN_BASE_URL          Local Muninn MCP URL (default: http://127.0.0.1:8750/mcp)
  PERPLEXITY_MCP_URL       Perplexity/frontdoor MCP URL (default: https://mcp.jaredlikes.com/mcp)
  PERPLEXITY_MCP_TOKEN     Bearer token for frontdoor tools/list UAT
  PERPLEXITY_CAPTURE_TEXT  Optional context.capture text; generated when absent
  LIFEGRAPH_MCP_URL        LifeGraph MCP URL (default: https://mcp.jaredlikes.com/lifegraph/mcp)
  LIFEGRAPH_MCP_TOKEN      Bearer token for LifeGraph tools/list UAT
  LIFEGRAPH_RECALL_QUERY   Optional life.recall query text
  RUN_REMOTE=1             Include remote native Muninn private smoke in all/safe mode

Modes:
  safe                     codex + muninn-local, plus remote-native when RUN_REMOTE=1
  codex                    Validate repo .mcp.json has muninn-local stdio proxy
  muninn-local             Validate local Muninn MCP health
  perplexity-tools         Validate external frontdoor only exposes context.capture
  perplexity-capture       Call context.capture when PERPLEXITY_MCP_TOKEN is set
  lifegraph-tools          Validate LifeGraph endpoint exposes life.recall without commit/resolve
  lifegraph-recall         Call life.recall when LIFEGRAPH_MCP_TOKEN is set
  remote-native            Run just muninn-private-smoke
  all                      Run safe checks plus token-backed checks when tokens are present
  live                     Run token-backed tools/list and positive-path calls when tokens are present
EOF
}

pass() {
  printf 'PASS %s\n' "$1"
}

skip() {
  printf 'SKIP %s\n' "$1"
}

fail() {
  printf 'FAIL %s\n' "$1" >&2
  exit 1
}

codex_config() {
  python3 - <<'PY'
import json
from pathlib import Path

cfg = json.loads(Path(".mcp.json").read_text())
server = cfg.get("mcpServers", {}).get("muninn-local")
assert server, "missing mcpServers.muninn-local"
assert server.get("type") == "stdio", "muninn-local must be stdio"
assert server.get("command") == "muninn", "muninn-local command must be muninn"
assert server.get("args") == ["mcp"], "muninn-local args must be ['mcp']"
assert server.get("env", {}).get("MUNINN_MCP_URL") == "http://127.0.0.1:8750/mcp", "muninn-local must target loopback"
PY
  pass "Codex .mcp.json declares private muninn-local stdio proxy"
}

muninn_local() {
  python3 "${ROOT_DIR}/scripts/muninn_mcp.py" --base-url "${MUNINN_BASE_URL}" health >/dev/null
  pass "local Muninn MCP health at ${MUNINN_BASE_URL}"
}

mcp_tools() {
  local url="$1"
  local token="$2"
  python3 - "$url" "$token" <<'PY'
import json
import sys
import urllib.request

url, token = sys.argv[1], sys.argv[2]
req = urllib.request.Request(
    url,
    data=json.dumps({
        "jsonrpc": "2.0",
        "id": "tools-list",
        "method": "tools/list",
        "params": {},
    }).encode(),
    headers={
        "content-type": "application/json",
        "authorization": f"Bearer {token}",
    },
    method="POST",
)
with urllib.request.urlopen(req, timeout=10) as resp:
    payload = json.loads(resp.read().decode())
tools = payload.get("result", {}).get("tools", [])
print(json.dumps(sorted(tool.get("name", "") for tool in tools)))
PY
}

mcp_call_tool() {
  local url="$1"
  local token="$2"
  local tool_name="$3"
  local args_json="$4"
  python3 - "$url" "$token" "$tool_name" "$args_json" <<'PY'
import json
import sys
import urllib.request

url, token, tool_name, args_json = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
req = urllib.request.Request(
    url,
    data=json.dumps({
        "jsonrpc": "2.0",
        "id": f"tool-{tool_name}",
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": json.loads(args_json),
        },
    }).encode(),
    headers={
        "content-type": "application/json",
        "authorization": f"Bearer {token}",
    },
    method="POST",
)
with urllib.request.urlopen(req, timeout=20) as resp:
    payload = json.loads(resp.read().decode())
print(json.dumps(payload, sort_keys=True))
PY
}

perplexity_tools() {
  if [[ -z "${PERPLEXITY_MCP_TOKEN:-}" ]]; then
    skip "perplexity tools/list; set PERPLEXITY_MCP_TOKEN for live UAT"
    return 0
  fi
  local names
  names="$(mcp_tools "${PERPLEXITY_MCP_URL}" "${PERPLEXITY_MCP_TOKEN}")"
  python3 - "$names" <<'PY'
import json
import sys

names = set(json.loads(sys.argv[1]))
assert "context.capture" in names, f"context.capture missing from {sorted(names)}"
for forbidden in ["life.recall", "life.observe", "life.commit", "life.resolve", "muninn_recall", "muninn_remember"]:
    assert forbidden not in names, f"{forbidden} must not be exposed on Perplexity capture surface"
PY
  pass "Perplexity/frontdoor tools/list exposes context.capture only from sensitive surfaces"
}

perplexity_capture() {
  if [[ -z "${PERPLEXITY_MCP_TOKEN:-}" ]]; then
    skip "perplexity context.capture call; set PERPLEXITY_MCP_TOKEN for live UAT"
    return 0
  fi
  local marker text args result
  marker="mcp-client-uat-$(date -u +%Y%m%dT%H%M%SZ)"
  text="${PERPLEXITY_CAPTURE_TEXT:-Philotic MCP client UAT marker ${marker}: verify context.capture remains Muninn continuity only.}"
  args="$(python3 - "$text" "$marker" <<'PY'
import json
import sys

text, marker = sys.argv[1], sys.argv[2]
print(json.dumps({
    "content": text,
    "category": "reference",
    "tags": ["uat", "mcp-client-uat", marker],
}))
PY
)"
  result="$(mcp_call_tool "${PERPLEXITY_MCP_URL}" "${PERPLEXITY_MCP_TOKEN}" "context.capture" "${args}")"
  python3 - "$result" <<'PY'
import json
import sys

payload = json.loads(sys.argv[1])
if "error" in payload:
    raise SystemExit(f"context.capture returned error: {payload['error']}")
result = payload.get("result", {})
content = result.get("content", [])
text = json.dumps(result)
if isinstance(content, list):
    text += " " + " ".join(json.dumps(item) for item in content)
lowered = text.lower()
assert "life." not in lowered, "context.capture response should not expose LifeGraph tool routing"
assert "muninn_recall" not in lowered, "context.capture response should not expose native Muninn recall"
PY
  pass "Perplexity/frontdoor context.capture call completed without exposing LifeGraph/native Muninn tools"
}

lifegraph_tools() {
  if [[ -z "${LIFEGRAPH_MCP_TOKEN:-}" ]]; then
    skip "lifegraph tools/list; set LIFEGRAPH_MCP_TOKEN for live UAT"
    return 0
  fi
  local names
  names="$(mcp_tools "${LIFEGRAPH_MCP_URL}" "${LIFEGRAPH_MCP_TOKEN}")"
  python3 - "$names" <<'PY'
import json
import sys

names = set(json.loads(sys.argv[1]))
assert "life.recall" in names, f"life.recall missing from {sorted(names)}"
for forbidden in ["context.capture", "life.commit", "life.resolve", "muninn_recall", "muninn_remember"]:
    assert forbidden not in names, f"{forbidden} must not be exposed on LifeGraph readonly surface"
PY
  pass "LifeGraph tools/list exposes governed recall without capture/native memory tools"
}

lifegraph_recall() {
  if [[ -z "${LIFEGRAPH_MCP_TOKEN:-}" ]]; then
    skip "lifegraph life.recall call; set LIFEGRAPH_MCP_TOKEN for live UAT"
    return 0
  fi
  local query args result
  query="${LIFEGRAPH_RECALL_QUERY:-What active goals or open loops are safe to recall for MCP client UAT?}"
  args="$(python3 - "$query" <<'PY'
import json
import sys

query = sys.argv[1]
print(json.dumps({
    "query_text": query,
    "operator_intent": "re_entry_context",
    "max_context_packets": 3,
}))
PY
)"
  result="$(mcp_call_tool "${LIFEGRAPH_MCP_URL}" "${LIFEGRAPH_MCP_TOKEN}" "life.recall" "${args}")"
  python3 - "$result" <<'PY'
import json
import sys

payload = json.loads(sys.argv[1])
if "error" in payload:
    raise SystemExit(f"life.recall returned error: {payload['error']}")
text = json.dumps(payload)
assert "cross_agent_context_packet" in text, "life.recall should return cross_agent_context_packet"
assert "context.capture" not in text, "LifeGraph recall response should not expose context.capture"
PY
  pass "LifeGraph life.recall call returned cross_agent_context_packet without capture surface"
}

remote_native() {
  just muninn-private-smoke
}

safe() {
  codex_config
  muninn_local
  if [[ "${RUN_REMOTE}" == "1" ]]; then
    remote_native
  else
    skip "remote native Muninn private smoke; set RUN_REMOTE=1 to include SSH tunnel UAT"
  fi
}

mode="${1:-safe}"
case "${mode}" in
  safe)
    safe
    ;;
  codex)
    codex_config
    ;;
  muninn-local)
    muninn_local
    ;;
  perplexity-tools)
    perplexity_tools
    ;;
  perplexity-capture)
    perplexity_capture
    ;;
  lifegraph-tools)
    lifegraph_tools
    ;;
  lifegraph-recall)
    lifegraph_recall
    ;;
  remote-native)
    remote_native
    ;;
  all)
    safe
    perplexity_tools
    lifegraph_tools
    ;;
  live)
    perplexity_tools
    perplexity_capture
    lifegraph_tools
    lifegraph_recall
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage >&2
    exit 64
    ;;
esac
