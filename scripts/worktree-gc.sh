#!/usr/bin/env bash
# Read-only by default; retirement requires a current owner release.
set -euo pipefail
exec python3 "$(cd "$(dirname "$0")" && pwd)/worktree_gc.py" "$@"
