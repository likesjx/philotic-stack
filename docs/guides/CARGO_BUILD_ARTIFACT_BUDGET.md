---
title: "Cargo Build Artifact Budget"
doc_type: guide
domain: build
status: active
last_updated: 2026-08-30
tags:
  - build
  - worktrees
  - disk
  - cargo
---

# Cargo Build Artifact Budget

← [Guides index](README.md) · [docs/README.md](../README.md)

**Problem:** ~20 parallel worktrees, each cold-building 605 dependencies into its
own `target/`. A single `target/` for this workspace measured **11 GB**. The Air
has hit 0 bytes free more than once.

**Short answer:** do **not** share one `target/` directory between worktrees — it
silently produces wrong binaries. Share the *compiler cache* instead, cut
debuginfo, and garbage-collect merged worktrees.

---

## ⛔ Why a single shared `target/` is unsafe here

The intuitive fix is to point every worktree at one directory:

```bash
export CARGO_TARGET_DIR=~/.cache/cargo-target/philotic-stack   # DO NOT DO THIS
```

Cargo fingerprints a compilation unit by **package name, version, profile, and
features — not by workspace path**. Two worktrees of the same repo therefore
produce *colliding* units, and the final binary is uplifted to a single
`target/debug/<name>` path shared by both.

Measured directly, two checkouts of one package sharing a target dir:

```
--- build worktree A ---
shared/debug/demoapp prints: I am worktree A

--- build worktree B into the SAME shared target dir ---
shared/debug/demoapp now prints: I am worktree B v2 CHANGED

=== now rebuild A, which was NOT changed ===
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.00s
binary prints: I am worktree B v2 CHANGED          <-- A's build, B's binary

=== ping-pong ===
after B build: I am worktree B v2 CHANGED
after A build: I am worktree B v2 CHANGED          <-- A can never win
```

`cargo build` in worktree A reports **`Finished`** — success, no error, no
rebuild — while `target/debug/` holds worktree B's code. Cargo considers A's unit
fresh because the fingerprint file is shared and its recorded dep-info points at
B's (unchanged) sources.

This is disqualifying for this repo specifically. `scripts/` contains **98**
`${ROOT_DIR}/target/{debug,release}/...` call sites — every smoke script, plus
`push-homebrew-remote.sh`, which `scp`s `target/release/*` to live hotels. Under
a shared target dir those would verify and *ship* the wrong branch's build while
reporting green. That is precisely the failure class
[`$runtime-rollout-watch`](../../skills/runtime-rollout-watch/SKILL.md) exists to
catch.

> Note also that stable cargo (1.94) has no `cargo clean --gc`, so a shared dir
> would additionally grow without bound as fingerprints from 20 branches
> accumulate.

## ✅ What to do instead

### 1. Bounded debuginfo (checked in, applies everywhere)

The workspace root `Cargo.toml` sets:

```toml
[profile.dev]
debug = "line-tables-only"
```

Panics and backtraces keep file and line numbers — which is how this stack is
actually debugged (runtime logs, `sample`, smoke scripts). Only debugger variable
inspection is lost. Need lldb locals for one session:

```bash
CARGO_PROFILE_DEV_DEBUG=2 cargo build
```

There is deliberately **no** `[profile.release]` block: release already defaults
to `debug = false`, and `target/release` is dominated by dependency rlibs, which
a profile knob cannot shrink.

### 2. sccache — share compiler work, not artifacts

```bash
just build-cache-setup      # install + enable, bounded cache
just build-cache-status     # hit rate + per-worktree target/ sizes
just build-cache-disable    # revert
```

sccache is keyed on preprocessed compiler input, so the 605 dependencies are
compiled **once per machine** and every later worktree gets cache hits. Because
each worktree still owns its `target/`, there is no fingerprint collision and no
shared lock — worktrees keep building in parallel.

It is set up as machine configuration (`~/.cargo/config.toml`), **not** in the
repo's checked-in `.cargo/config.toml`, which must stay portable for
self-hosters.

**Honest limit:** sccache makes a fresh worktree's build *fast*, not *small*.
Disk is still N× worktrees. Steps 1 and 3 are what handle size.

### 3. Sweep `target/` from live-but-idle worktrees

`worktree-gc.sh` removes whole worktrees, but only ones already merged to
`origin/develop` — and in practice that reclaims **nothing**. A real dry run:

```
preserved: 19
would remove: 0
reclaimed: -0.05 GB
```

All 19 worktrees were preserved as unmerged, dirty, or detached. They are
legitimately alive. The disk is still gone, because each live worktree owns its
own multi-GB `target/`.

So sweep the *artifacts*, not the worktrees. `target/` is gitignored, regenerable
build output — deleting it costs a rebuild and loses no work:

```bash
just target-sweep          # dry run — what could be reclaimed
just target-sweep-apply    # delete target/ from worktrees idle >14 days
```

Safety invariants (see the script header): only ever removes `<worktree>/target`;
never a worktree, branch, or tracked file; refuses entirely while any
`cargo`/`rustc` is running; skips any `target/` whose cargo lock is held; skips
anything built within `--idle-days` (default 14); and never touches the main
checkout unless `--include-main` is passed, since `push-homebrew-remote.sh` reads
its `target/release`.

Still use `worktree-gc` for genuinely finished work:

```bash
just worktree-gc            # dry run
just worktree-gc-apply      # remove merged+clean worktrees
just worktree-gc-schedule   # launchd, every 2h
```

## Rules of thumb

| Do | Don't |
|---|---|
| `just build-cache-setup` once per machine | set a shared `CARGO_TARGET_DIR` |
| `just target-sweep-apply` when disk tightens | symlink one worktree's `target/` into another |
| `CARGO_PROFILE_DEV_DEBUG=2` for a debugger session | add `[profile.release] debug` (already the default) |
| keep one `target/` per worktree | assume `Finished` means your code was built |

## Reproducing the collision

The experiment above is two throwaway packages sharing one `CARGO_TARGET_DIR`;
it takes under a minute and is worth re-running if anyone proposes the shared-dir
fix again. Build A, build B with different source, then rebuild A and read
`target/debug/<bin>`.

---

← [Guides index](README.md) · [Architecture](../architecture/ARCHITECTURE.md) · [Workflow](../process/WORKFLOW.md)
