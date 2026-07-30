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
check_file "scripts/docs-metadata-check.py" "docs metadata helper is present"
check_file "scripts/codex-worktree.sh" "worktree helper is present"
check_file "scripts/codex-workstream.sh" "workstream helper is present"
check_file "skills/muninn-memory-habit/SKILL.md" "repo-local Muninn habit skill is present"
check_file "skills/proposal-maintainer/SKILL.md" "repo-local proposal maintainer skill is present"
check_file "skills/architecture-docs-maintainer/SKILL.md" "repo-local architecture docs maintainer skill is present"
check_file "skills/verification-ladder/SKILL.md" "repo-local verification ladder skill is present"
check_file "docs/architecture/README.md" "architecture docs hub is present"
check_file "docs/architecture/DOMAIN_MAP.md" "architecture domain map is present"
check_file "docs/architecture/SEAM_REGISTRY.md" "architecture seam registry is present"
check_file "docs/architecture/ARCHITECTURE_STATUS.md" "architecture status source of truth is present"
check_file "docs/architecture/DOC_TAGGING_FRONTMATTER_PROPOSAL.md" "docs metadata proposal is present"

if command -v python3 >/dev/null 2>&1; then
  pass "python3 is available for helper scripts"
else
  fail "python3 is available for helper scripts"
fi

if python3 "${ROOT_DIR}/scripts/muninn_mcp.py" --base-url "$MUNINN_BASE_URL" bootstrap >/dev/null; then
  pass "Muninn MCP helper passes recoverable bootstrap gate"
else
  fail "Muninn MCP helper passes recoverable bootstrap gate"
fi

if python3 "${ROOT_DIR}/scripts/docs-metadata-check.py" >/dev/null; then
  pass "docs metadata anchor set passes frontmatter checks"
else
  fail "docs metadata anchor set passes frontmatter checks"
fi

# proposal:secret-push-guard-activation — assert the guard is actually wired.
#
# .githooks/pre-push has existed (and invoked scripts/secret-push-check.py)
# since March, but core.hooksPath pointed at an empty .git/hooks, so it had
# NEVER FIRED — despite backup-pre-secret-rewrite-20260313 in this repo's
# history showing the incident it exists to prevent already happened. A guard
# nobody checks is indistinguishable from no guard, so check it.
HOOKS_PATH="$(git -C "${ROOT_DIR}" config core.hooksPath 2>/dev/null || true)"
if [[ "${HOOKS_PATH}" == ".githooks" ]]; then
  pass "git core.hooksPath points at .githooks"
else
  fail "git core.hooksPath is '${HOOKS_PATH:-<unset>}', not .githooks — the secret-push and rustfmt hooks are INERT. Fix: just install-git-hooks"
fi

for hook in pre-push pre-commit; do
  if [[ -x "${ROOT_DIR}/.githooks/${hook}" ]]; then
    pass ".githooks/${hook} is present and executable"
  else
    fail ".githooks/${hook} is present and executable"
  fi
done

run_in_repo "cargo check workspace baseline" cargo check --workspace
run_in_repo "cargo test workspace baseline" cargo test --workspace

printf '\n'
if [[ "$FAILURES" -eq 0 ]]; then
  printf 'Engine check complete: all checks passed.\n'
else
  printf 'Engine check complete: %d check(s) failed.\n' "$FAILURES" >&2
  exit 1
fi
