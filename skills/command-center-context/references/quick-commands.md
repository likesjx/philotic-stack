# Quick Commands

## Lifecycle

Read the repo's `justfile` first. Use its exact recipe names when Jane Command Center lifecycle commands are present; if they are missing, do not invent them.

```bash
just --list
# then run the repo's actual context/handoff recipes, if any
```

If the current repo does not expose the lifecycle recipes, fall back to Jane Command Center's canonical `justfile`:

```bash
just -f /Users/jaredlikes/code/jane-command-center/justfile context-check
just -f /Users/jaredlikes/code/jane-command-center/justfile session-start AGENT="codex" GOAL="..." TO="architect"
just -f /Users/jaredlikes/code/jane-command-center/justfile session-handoff FILE=/abs/path/to/handoff.md TO="architect"
just -f /Users/jaredlikes/code/jane-command-center/justfile aria-ingest-handoffs
just -f /Users/jaredlikes/code/jane-command-center/justfile inventory-refresh
```

## Typical Pattern

```bash
just --list
# confirm whether the repo exposes command-center recipes
# if yes, use those exact recipes
# if no, note the gap and use:
# just -f /Users/jaredlikes/code/jane-command-center/justfile <recipe> ...
```
