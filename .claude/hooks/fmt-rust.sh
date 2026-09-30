#!/usr/bin/env bash
# PostToolUse(Edit|Write) — run rustfmt on the file that was just written.
#
# Why: `cargo fmt --all --check` is a hard CI gate as of pr-check.yml. Without
# this hook, agents discover formatting drift only after pushing, which costs a
# round trip per PR. With it, the tree simply never drifts.
#
# The workspace mixes edition 2021 and 2024 crates, so the edition is resolved
# from the file's nearest Cargo.toml rather than assumed.
#
# Always exits 0: a formatter is a convenience, never a reason to fail a tool
# call the agent has already completed.
set -uo pipefail

input=$(cat)

file=$(printf '%s' "$input" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    print(d.get("tool_input", {}).get("file_path", ""))
except Exception:
    pass
' 2>/dev/null) || exit 0

case "$file" in
    *.rs) ;;
    *) exit 0 ;;
esac

[ -f "$file" ] || exit 0
command -v rustfmt >/dev/null 2>&1 || exit 0

dir=$(dirname "$file")
while [ "$dir" != "/" ] && [ "$dir" != "." ] && [ ! -f "$dir/Cargo.toml" ]; do
    dir=$(dirname "$dir")
done

edition=2021
if [ -f "$dir/Cargo.toml" ]; then
    found=$(grep -m1 '^edition' "$dir/Cargo.toml" 2>/dev/null | sed 's/[^"]*"\([^"]*\)".*/\1/')
    [ -n "$found" ] && edition="$found"
fi

rustfmt --edition "$edition" "$file" >/dev/null 2>&1 || true
exit 0
