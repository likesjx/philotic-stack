# Guest Binary Resolution Proposal

## Goal

Replace hardcoded `target/debug/<name>` paths in `guest_seed_for_profile` with a resolution strategy that works correctly in both dev and deployed environments without shims.

## Problem

`guest_seed_for_profile` in `crates/ansible/src/main.rs` seeds every guest with a hardcoded relative binary path:

```rust
"command": "target/debug/hegemon"
"command": "target/debug/agent-core"
"command": "target/debug/model-controller-gemini"   // no such binary
"command": "target/debug/model-controller-elevenlabs" // no such binary
"command": "target/debug/tool-runner"               // no such binary
```

This has two distinct problems:

**1. Wrong binary names.** `model-controller-gemini` and `model-controller-elevenlabs` do not exist as separate binaries — both map to `model-router`. `tool-runner` has no binary yet. The names were written as aspirational crate names rather than current compiled artifacts.

**2. Relative paths only work from the repo root.** In a deployed hotel, `WorkingDirectory` is `/opt/philotic/data`, not the repo checkout. The relative `target/debug/...` path never resolves. The first VPS smoke worked around this with `target/debug/` symlinks in the data directory — an explicit hack noted for removal.

## Core Recommendation

### Binary name alignment

Seed guest commands using the actual compiled binary name, not the aspirational future crate name:

| Guest role | Current seeded command | Correct command |
|---|---|---|
| `hegemon` | `target/debug/hegemon` | `hegemon` |
| `agent` | `target/debug/agent-core` | `agent-core` |
| `model.gemini` | `target/debug/model-controller-gemini` | `model-router` |
| `model.elevenlabs` | `target/debug/model-controller-elevenlabs` | `model-router` |
| `tool` | `target/debug/tool-runner` | _(not yet built — see below)_ |

When `model-router` splits into separate provider binaries, update the seed at that point — not speculatively before.

### Path resolution

Use just the binary name (no path prefix) in the seeded command. Resolution order:

1. `PHILOTIC_BIN_DIR` env var — absolute path to the directory containing hotel guest binaries. Set by systemd in deployed environments.
2. System `PATH` — fallback for dev mode where `cargo run` or symlinks handle resolution.

The hotel daemon prepends `PHILOTIC_BIN_DIR` to the seeded command when spawning if the command is not already absolute.

### Dev mode

In dev mode (`just start-ansible` from the repo root), `PHILOTIC_BIN_DIR` is unset. Binary names resolve via `PATH` — which means either:
- `cargo build` has been run and binaries are in `target/debug/` with that dir on PATH, or
- symlinks exist in a directory already on PATH.

The `justfile` should ensure the relevant `target/debug/` dir is on PATH when running locally, so the seeded binary names work without any path prefix.

### Unimplemented guests

Guests with no binary (e.g. `tool-runner`) should produce a warning and be skipped, not a hard spawn failure. The hotel should remain operational for the guests that do materialize.

## Disposition

`proposed`

## Current Slice

The `target/debug/` Ansible shim tasks in `philotic_hotel` role are the transitional workaround. They must be removed once `guest_seed_for_profile` is fixed.

What is deferred:
- Splitting `model-router` into separate provider binaries
- `tool-runner` binary and crate
- Full env-var-driven binary resolution implementation

## Active Work Links

- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
