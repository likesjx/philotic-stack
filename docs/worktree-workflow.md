# Worktree Workflow

This repo should use one git worktree per active Codex thread.

The goal is simple:

- one branch per thread
- one worktree per branch
- one coherent slice per worktree

Do not run multiple active implementation threads from the same filesystem checkout.

## Why

Without separate worktrees, parallel threads collide in exactly the files we most want to keep clean:

- hot runtime files
- active proposals
- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)

The result is mixed commits, confused diffs, and architectural boundary drift masquerading as productivity.

## Naming Convention

- branch: `codex/<slug>`
- worktree path: sibling directory `../philotic-stack-<slug>`

Examples:

- branch: `codex/telegram-controller`
- worktree: `../philotic-stack-telegram-controller`

- branch: `codex/hegemon-membrane`
- worktree: `../philotic-stack-hegemon-membrane`

## Commands

Create a new worktree from `main`:

```bash
just worktree-create telegram-controller
```

Create a new worktree from another base ref:

```bash
just worktree-create telegram-controller codex/hegemon-membrane-slice
```

List active worktrees:

```bash
just worktree-list
```

Show the expected path for a slug:

```bash
just worktree-path telegram-controller
```

Remove a finished worktree:

```bash
just worktree-remove telegram-controller
```

Remove a finished worktree and delete its local branch:

```bash
just worktree-remove telegram-controller --delete-branch
```

Prune stale metadata:

```bash
just worktree-prune
```

## Starting A New Thread

1. Choose a short slug for the thread.
2. Create the worktree with `just worktree-create <slug>`.
3. Open that sibling directory in the new conversation or terminal.
4. Keep that worktree scoped to one coherent seam.

## Migrating An Existing Active Thread

If a thread is currently using the shared checkout:

1. Commit or stash its current work in the shared checkout if needed.
2. Create a dedicated worktree for that thread.
3. Open the new worktree path in the corresponding conversation.
4. Continue that thread there, not in the shared root checkout.

If a thread already has a branch:

```bash
./scripts/codex-worktree.sh create <slug> <existing-branch>
```

The helper will reuse the existing branch if it already exists.

## Operating Rules

- One active implementation conversation should map to one worktree.
- Do not mix unrelated slices in one worktree.
- Run tests in the same worktree that owns the code changes.
- Commit early on hot shared files.
- If two threads need the same files and the same decision surface at once, stop and resolve the seam before both continue.

## What Other Threads Need To Do

Each other active thread should move to its own worktree.

Concretely, for every active conversation:

1. Pick a slug.
2. Run `just worktree-create <slug>` from the repo root.
3. Open the resulting sibling directory for that conversation.
4. Keep future edits for that conversation in that worktree only.

If a thread is already mid-change in the shared checkout, move it after its next clean commit boundary.
