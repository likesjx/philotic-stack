#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MUNINN_BASE_URL="${MUNINN_MCP_BASE_URL:-http://localhost:8750/mcp}"

FAILURES=0

pass() {
  printf 'PASS %s\n' "$1"
}

fail() {
  printf 'FAIL %s\n' "$1" >&2
  FAILURES=$((FAILURES + 1))
}

check_file() {
  local path="$1"
  local label="$2"

  if [[ -f "${ROOT_DIR}/${path}" ]]; then
    pass "${label}"
  else
    fail "${label} (missing ${path})"
  fi
}

run_check() {
  local label="$1"
  shift

  if "$@"; then
    pass "${label}"
  else
    fail "${label}"
  fi
}

run_in_repo() {
  local label="$1"
  shift
  local previous_dir="$PWD"

  cd "$ROOT_DIR"
  if "$@"; then
    pass "${label}"
  else
    fail "${label}"
  fi
  cd "$previous_dir"
}

printf 'Philotic Engine Check\n'
printf 'Root: %s\n' "$ROOT_DIR"
printf 'Muninn MCP: %s\n' "$MUNINN_BASE_URL"
printf '\n'

check_file "AGENTS.md" "repo protocol is present"
check_file "CLAUDE.md" "session bootstrap guide is present"
check_file "scripts/muninn_mcp.py" "shared Muninn helper is present"
check_file "scripts/codex-worktree.sh" "worktree helper is present"
check_file "scripts/codex-workstream.sh" "workstream helper is present"
check_file "skills/muninn-memory-habit/SKILL.md" "repo-local Muninn habit skill is present"
check_file "skills/proposal-maintainer/SKILL.md" "repo-local proposal maintainer skill is present"
check_file "skills/verification-ladder/SKILL.md" "repo-local verification ladder skill is present"

if command -v python3 >/dev/null 2>&1; then
  pass "python3 is available for helper scripts"
else
  fail "python3 is available for helper scripts"
fi

if python3 "${ROOT_DIR}/scripts/muninn_mcp.py" --base-url "$MUNINN_BASE_URL" require >/dev/null; then
  pass "Muninn MCP helper passes required bootstrap gate"
else
  fail "Muninn MCP helper passes required bootstrap gate"
fi

run_in_repo "cargo check workspace baseline" cargo check --workspace
run_in_repo "cargo test workspace baseline" cargo test --workspace

printf '\n'
if [[ "$FAILURES" -eq 0 ]]; then
  printf 'Engine check complete: all checks passed.\n'
else
  printf 'Engine check complete: %d check(s) failed.\n' "$FAILURES" >&2
  exit 1
fi
