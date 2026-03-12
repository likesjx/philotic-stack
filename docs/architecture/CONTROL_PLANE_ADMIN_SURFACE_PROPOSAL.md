# Control Plane Admin Surface Proposal

## Goal

Define the first deterministic management surface for the context graph and hotel/agent runtime so operators stop relying on file edits, restart rituals, and interpretive debugging as the main admin interface.

## Disposition

`proposed`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Core Recommendation

Philotic should have a **deterministic context graph manager** and **admin surface** as part of the main CLI/TUI story.

The first recommended shape is:

1. CLI-backed control plane
2. TUI as the first serious admin app
3. web/app surfaces later if justified

## What This Surface Owns

- inspect graph-backed agent and hotel state
- inspect live materialization/routing state
- mutate allowed admin-controlled records through validated commands
- show drift between startup overlays and graph truth
- expose audit-friendly diffs and repair actions

## TUI Recommendation

Yes, the TUI should be the first admin app.

Recommended reasons:

- lower implementation cost than web first
- works well for operator workflows
- fits the existing CLI/runtime culture
- can define the control model before future GUI layers add gloss

## Relationship To The Main CLI

The TUI should live under the main Philotic CLI rather than as a disconnected side tool.

That keeps:

- auth and operator posture consistent
- shared config discovery in one place
- admin workflows visible and scriptable

## Future Surfaces

Possible later layers:

- web GUI for richer inspection
- full desktop/mobile app if operator use justifies it

But the architecture should be proven in the CLI/TUI first.

## First Slice Recommendation

Start with a deterministic graph manager that can:

- inspect agent profile/config
- inspect hotel manifests and live guests
- show routing/materialization state
- patch a bounded set of records with audit output

Then wrap that same management plane in a TUI.
