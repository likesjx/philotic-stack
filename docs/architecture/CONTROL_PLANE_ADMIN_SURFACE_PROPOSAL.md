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

It should also own high-trust operator workflows that must not be reduced to ordinary chat commands or raw key handling:

- secret add/rotate/revoke initiation
- vault status and audit inspection
- transport/perimeter trust changes
- break-glass or recovery flows with stronger ceremony

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

## Relationship To Membrane

`membrane` may expose admin/operator entry points, but it should not become the owner of secret or policy truth.

Recommended boundary:

- `membrane` may start an authenticated operator control session
- `membrane` may launch a Mini App or secure action link
- `membrane` may collect approval intents and control-plane requests
- `ansible` / hotel control plane validates, authorizes, persists, audits, and executes the requested admin action

This keeps the outside-world interface useful without letting the transport boundary quietly become the admin database with better emojis.

## Secret Administration Recommendation

Adding or rotating secrets should be treated as an admin/control-plane workflow, not as ordinary conversational tool use.

Recommended shape:

1. operator issues a high-trust action such as `/vault add gemini` or `/vault rotate telegram`
2. `membrane` verifies operator posture and opens a secure admin flow
3. a CLI/TUI or Mini App collects the action under explicit auth/approval
4. the hotel control plane performs the vault mutation
5. the result returns as structured admin output without surfacing raw secret material

The key rule is:

- `membrane` starts and brokers the flow
- the hotel control plane owns the mutation
- the vault owns the secret
- model-facing components never receive the admin key material

## First Slice Recommendation

Start with a deterministic graph manager that can:

- inspect agent profile/config
- inspect hotel manifests and live guests
- show routing/materialization state
- patch a bounded set of records with audit output
- initiate one high-trust vault admin flow without exposing raw secret material

Then wrap that same management plane in a TUI.
