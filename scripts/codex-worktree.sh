#!/usr/bin/env bash

set -euo pipefail

usage() {
    cat <<'EOF'
Usage:
  scripts/codex-worktree.sh create <slug> [base-ref]
  scripts/codex-worktree.sh add <slug> [base-ref]
  scripts/codex-worktree.sh list
  scripts/codex-worktree.sh path <slug>
  scripts/codex-worktree.sh remove <slug> [--delete-branch]
  scripts/codex-worktree.sh prune

Conventions:
  - Branches are created as codex/<slug>
  - Worktrees live as siblings of the repo:
      ../philotic-stack-<slug>
EOF
}

die() {
    echo "error: $*" >&2
    exit 1
}

require_repo_root() {
    git rev-parse --show-toplevel >/dev/null 2>&1 || die "not inside a git repository"
}

repo_root() {
    git rev-parse --show-toplevel
}

repo_name() {
    basename "$(repo_root)"
}

branch_name() {
    local slug=$1
    echo "codex/${slug}"
}

worktree_path() {
    local slug=$1
    local root parent
    root=$(repo_root)
    parent=$(cd "${root}/.." && pwd)
    echo "${parent}/$(repo_name)-${slug}"
}

branch_exists() {
    local branch=$1
    git show-ref --verify --quiet "refs/heads/${branch}"
}

worktree_exists() {
    local path=$1
    git worktree list --porcelain | awk '/^worktree / {print $2}' | grep -Fxq "$path"
}

cmd_create() {
    local slug=${1:-}
    local base_ref=${2:-main}
    [ -n "${slug}" ] || die "missing slug"

    local branch path
    branch=$(branch_name "${slug}")
    path=$(worktree_path "${slug}")

    if [ -e "${path}" ] || worktree_exists "${path}"; then
        die "worktree path already exists: ${path}"
    fi

    if branch_exists "${branch}"; then
        git worktree add "${path}" "${branch}"
    else
        git worktree add -b "${branch}" "${path}" "${base_ref}"
    fi

    cat <<EOF
Created:
  branch:   ${branch}
  worktree: ${path}

Next:
  cd ${path}
  git status --short
EOF
}

cmd_list() {
    git worktree list
}

cmd_path() {
    local slug=${1:-}
    [ -n "${slug}" ] || die "missing slug"
    worktree_path "${slug}"
}

cmd_remove() {
    local slug=${1:-}
    local delete_branch=${2:-}
    [ -n "${slug}" ] || die "missing slug"

    local branch path
    branch=$(branch_name "${slug}")
    path=$(worktree_path "${slug}")

    if [ -d "${path}" ] || worktree_exists "${path}"; then
        git worktree remove "${path}"
    else
        die "worktree does not exist: ${path}"
    fi

    if [ "${delete_branch}" = "--delete-branch" ]; then
        git branch -d "${branch}"
    fi

    echo "Removed worktree: ${path}"
}

cmd_prune() {
    git worktree prune
}

main() {
    require_repo_root

    local command=${1:-}
    shift || true

    case "${command}" in
        create|add)
            cmd_create "$@"
            ;;
        list)
            cmd_list
            ;;
        path)
            cmd_path "$@"
            ;;
        remove)
            cmd_remove "$@"
            ;;
        prune)
            cmd_prune
            ;;
        ""|-h|--help|help)
            usage
            ;;
        *)
            usage
            die "unknown command: ${command}"
            ;;
    esac
}

main "$@"
