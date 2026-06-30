#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MUNINN_BASE_URL="${MUNINN_BASE_URL:-http://127.0.0.1:8750/mcp}"
PERPLEXITY_MCP_URL="${PERPLEXITY_MCP_URL:-https://mcp.jaredlikes.com/mcp}"
LIFEGRAPH_MCP_URL="${LIFEGRAPH_MCP_URL:-https://mcp.jaredlikes.com/lifegraph/mcp}"
RUN_REMOTE="${RUN_REMOTE:-0}"

usage() {
  cat <<EOF
Usage: $0 [safe|codex|muninn-local|perplexity-tools|lifegraph-tools|remote-native|all]

Safe modes do not require live bearer tokens and never print secrets.

Environment:
  MUNINN_BASE_URL          Local Muninn MCP URL (default: http://127.0.0.1:8750/mcp)
  PERPLEXITY_MCP_URL       Perplexity/frontdoor MCP URL (default: https://mcp.jaredlikes.com/mcp)
  PERPLEXITY_MCP_TOKEN     Bearer token for frontdoor tools/list UAT
  LIFEGRAPH_MCP_URL        LifeGraph MCP URL (default: https://mcp.jaredlikes.com/lifegraph/mcp)
  LIFEGRAPH_MCP_TOKEN      Bearer token for LifeGraph tools/list UAT
  RUN_REMOTE=1             Include remote native Muninn private smoke in all/safe mode

Modes:
  safe                     codex + muninn-local, plus remote-native when RUN_REMOTE=1
  codex                    Validate repo .mcp.json has muninn-local stdio proxy
  muninn-local             Validate local Muninn MCP health
  perplexity-tools         Validate external frontdoor only exposes context.capture
  lifegraph-tools          Validate LifeGraph endpoint exposes life.recall without commit/resolve
  remote-native            Run just muninn-private-smoke
  all                      Run safe checks plus token-backed checks when tokens are present
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
  lifegraph-tools)
    lifegraph_tools
    ;;
  remote-native)
    remote_native
    ;;
  all)
    safe
    perplexity_tools
    lifegraph_tools
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage >&2
    exit 64
    ;;
esac

