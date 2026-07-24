#!/usr/bin/env bash

set -euo pipefail

usage() {
    cat <<'EOF'
Usage:
  scripts/codex-workstream.sh start <slug> [base-ref]
  scripts/codex-workstream.sh status <slug> [compare-ref]
  scripts/codex-workstream.sh overlap <slug> [compare-ref]

Conventions:
  - Worktree bootstrap delegates to scripts/codex-worktree.sh
  - compare-ref defaults to origin/develop
  - hot files are the runtime/docs paths most likely to cause expensive merge drift
EOF
}

die() {
    echo "error: $*" >&2
    exit 1
}

repo_root() {
    dirname "$(git rev-parse --path-format=absolute --git-common-dir)"
}

script_dir() {
    cd "$(dirname "${BASH_SOURCE[0]}")" && pwd
}

worktree_path() {
    local slug=$1
    "$(script_dir)/codex-worktree.sh" path "${slug}"
}

branch_name() {
    local slug=$1
    echo "codex/${slug}"
}

require_worktree() {
    local path=$1
    [ -d "${path}" ] || die "worktree does not exist: ${path}"
}

hot_file_patterns() {
    cat <<'EOF'
crates/aiua/src/main.rs
crates/aiua/src/service/ipc.rs
crates/philote/src/runtime.rs
crates/membrane-telegram/src/main.rs
crates/model-router/src/main.rs
crates/model-router/src/runtime.rs
crates/model-router/src/controller.rs
crates/model-router/src/providers/gemini.rs
crates/model-router/src/providers/elevenlabs.rs
crates/philotic-client/src/lib.rs
crates/aiua/README.md
docs/task.md
docs/architecture/MODEL_CONTROLLER_PROPOSAL.md
EOF
}

print_header() {
    local title=$1
    printf '\n== %s ==\n' "${title}"
}

cmd_start() {
    local slug=${1:-}
    local base_ref=${2:-develop}
    [ -n "${slug}" ] || die "missing slug"

    "$(script_dir)/codex-worktree.sh" create "${slug}" "${base_ref}"

    local path
    path=$(worktree_path "${slug}")

    cat <<EOF

Workstream bootstrap:
  slug:        ${slug}
  branch:      $(branch_name "${slug}")
  worktree:    ${path}
  compare-ref: origin/develop

Recommended next steps:
  cd ${path}
  git status --short
  just workstream-status ${slug}

Rules:
  - Keep one active implementation thread per worktree.
  - Merge or rebase from origin/develop before touching hot runtime files.
  - PRs target develop, not main. main is releases only.
  - Run just workstream-overlap ${slug} before opening a PR.
EOF
}

cmd_status() {
    local slug=${1:-}
    local compare_ref=${2:-origin/develop}
    [ -n "${slug}" ] || die "missing slug"

    local path
    path=$(worktree_path "${slug}")
    require_worktree "${path}"

    print_header "Workstream"
    echo "slug:        ${slug}"
    echo "branch:      $(branch_name "${slug}")"
    echo "worktree:    ${path}"
    echo "compare-ref: ${compare_ref}"

    print_header "Git Status"
    git -C "${path}" status --short

    print_header "Changed Files vs ${compare_ref}"
    git -C "${path}" diff --name-only "${compare_ref}..."

    print_header "Hot File Overlap"
    cmd_overlap "${slug}" "${compare_ref}"
}

cmd_overlap() {
    local slug=${1:-}
    local compare_ref=${2:-origin/develop}
    [ -n "${slug}" ] || die "missing slug"

    local path changed overlap_file hot_file_list
    path=$(worktree_path "${slug}")
    require_worktree "${path}"

    changed=$(mktemp)
    overlap_file=$(mktemp)
    hot_file_list=$(mktemp)

    git -C "${path}" diff --name-only "${compare_ref}..." | sort -u > "${changed}"
    hot_file_patterns | sort -u > "${hot_file_list}"

    if comm -12 "${changed}" "${hot_file_list}" > "${overlap_file}"; then
        :
    fi

    if [ -s "${overlap_file}" ]; then
        cat "${overlap_file}"
    else
        echo "No hot-file overlap detected."
    fi

    rm -f "${changed}" "${overlap_file}" "${hot_file_list}"
}

main() {
    git rev-parse --show-toplevel >/dev/null 2>&1 || die "not inside a git repository"

    local command=${1:-}
    shift || true

    case "${command}" in
        start)
            cmd_start "$@"
            ;;
        status)
            cmd_status "$@"
            ;;
        overlap)
            cmd_overlap "$@"
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
