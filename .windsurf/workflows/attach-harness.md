---
description: Attach or refresh a harness outside any workstream. Updates desired/rendered/observed harness state without creating a seam, workstream, or session.
---

# Attach Harness

Use this workflow when you want to attach a harness before any workstream exists.

This workflow:

1. Inspects current harness state
2. Plans the desired harness attachment
3. Applies the attachment to desired/rendered state
4. Verifies the local projection and observed state
5. Checks drift

It does **not** create a seam, workstream, or session.

## Prerequisites

- `just phil` must be available from the repo root
- The target harness must already exist in the graph

## Example: attach the windsurf harness

```bash
# Inspect current harness state
just phil harness status harness:windsurf-native

# Plan the desired attachment
just phil harness plan harness:windsurf-native --profile orchestrator

# Apply the desired attachment and render the local projection
just phil harness apply harness:windsurf-native --profile orchestrator

# Verify the local projection and refresh observed state
just phil harness verify harness:windsurf-native

# Optional: check for drift
just phil harness drift harness:windsurf-native
```

## Bundle-based attachment

If you want to attach a named bundle instead of a profile, swap `--profile orchestrator` for `--bundle <bundle-name>` in the `plan` and `apply` commands.

## When to use this

Use this workflow when you are:

- preparing a harness before starting work
- refreshing the harness's desired state after a config change
- verifying whether a local projection matches the graph

## When not to use this

Do not use this workflow to start execution on a workstream.

For that, use the separate `start-workstream` workflow after the harness has been attached.

## Related

- `@skills/windsurf-harness-setup` — explains the attach/refresh boundary and the later workstream-start flow
- `@skills/multi-agent-orchestration` — coordinates multiple agents after the harness is attached
