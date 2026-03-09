# Parallel Worktree Runbook

Use this runbook whenever multiple implementation conversations are active at the same time.

## Goal

Keep each active implementation thread isolated in its own sibling worktree, and surface hot-file overlap before it turns into a quota-burning merge archaeology project.

## Core Recommendation

Treat a worktree as the unit of an implementation conversation.

- one implementation thread -> one `codex/<slug>` branch
- one `codex/<slug>` branch -> one sibling worktree
- one PR per coherent slice

When a thread needs `crates/ansible/src/main.rs`, `crates/ansible/src/service/ipc.rs`, `crates/agent-core/src/runtime.rs`, `crates/hegemon/src/main.rs`, or `docs/task.md`, sync it from `origin/main` first and check overlap before continuing.

## Standard Flow

1. Start a thread worktree:

```bash
just workstream-start model-controller-abstraction
```

2. Before touching runtime hot files, inspect the worktree:

```bash
just workstream-status model-controller-abstraction
```

3. Before opening a PR, check for hot-file overlap explicitly:

```bash
just workstream-overlap model-controller-abstraction
```

4. If overlap exists:

- merge or rebase from `origin/main` first
- keep only one thread owning the architectural boundary
- let the other thread implement inside that boundary instead of re-arguing it in code

## Hot Files

These are the paths where parallel Philotic runtime work most often collides:

- `crates/ansible/src/main.rs`
- `crates/ansible/src/service/ipc.rs`
- `crates/agent-core/src/runtime.rs`
- `crates/hegemon/src/main.rs`
- `crates/model-router/src/main.rs`
- `crates/model-router/src/runtime.rs`
- `crates/model-router/src/controller.rs`
- `crates/model-router/src/providers/gemini.rs`
- `crates/model-router/src/providers/elevenlabs.rs`
- `crates/philotic-client/src/lib.rs`
- `crates/ansible/README.md`
- `docs/task.md`
- `docs/architecture/MODEL_CONTROLLER_PROPOSAL.md`

## Reality Check

`git worktree create` alone is not enough.

The expensive part is not making the sibling checkout. The expensive part is letting two active threads drift through the same runtime boundary without an overlap check and then discovering the contradiction inside a merge conflict.
