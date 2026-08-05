# Contributing

Thanks for looking at this. The Philotic Stack is a distributed AI agent OS in
Rust — a **hotel** (node) materializes AI **guests** (supervised subprocesses),
with all state in a SQLite context graph owned by the hotel daemon.

This document covers how to build it, how changes get verified, and the one
convention that is genuinely unusual here: **how we talk about confidence**.

---

## Building

```bash
just check       # cargo check --workspace — fastest feedback
just build       # cargo build --workspace
just test        # cargo test --workspace
just format      # cargo fmt --all
```

`cargo test -p <crate>` runs a single crate.

Running a hotel needs `mesh-config.json` in the repository root — copy
`mesh-config.example.json` and fill in your own identity and provider keys.

### Platform note

The workspace contains Apple-only crates, so the full test suite is macOS-only.
CI builds a selective package set on Linux (`build-linux.yml`) and runs the
tests on macOS. If you are on Linux, expect `just test` to fail on those crates;
`just check` and the Linux package set should both pass.

---

## Branch model

- **`main`** — stable. Only merged from `develop` when the edge is ready to ship.
- **`develop`** — the integration edge. **All pull requests target `develop`,
  not `main`.**
- **`codex/<slug>`** — one branch per active implementation thread.

Treat a worktree as the unit of an implementation conversation: one thread, one
branch, one worktree. `just workstream-start <slug>` creates a sibling worktree,
and `just workstream-overlap <slug>` shows risky overlap with `origin/main`
before you open a PR.

---

## Verification ladder

This is the part worth reading. Claims about whether something works are graded,
and the grade is expected to appear in your PR description:

| Rung | Means |
|---|---|
| **test-green** | The test suite passes. The code does what its tests say. |
| **smoke-green** | An end-to-end path was exercised against a running system. |
| **watched-live-green** | The change was observed working in a real deployment, on the installed binary, over a real window. |

These are not synonyms and the difference has repeatedly mattered. This project
has a long history of changes that were test-green and still wrong in
production — a watchdog whose timeout ceiling never held, a batch parser that
rejected every real payload, a deploy path that no-op'd and reported success.

**Say which rung you reached.** "test-green, not watched-live" is a completely
acceptable thing to write, and far more useful than an unqualified "works".

### A test that cannot fail is not evidence

When fixing a bug, verify your test **fails without your fix**. Revert the fix,
watch the test go red, restore it. A test written after the fact that passes
against both the broken and fixed code has told you nothing.

---

## Failure must be loud

The single largest class of defect in this project's history is **silent
failure** — the system broken while reporting nothing. A full disk wedging a
hotel with zero running guests and no error. An agent deaf for 31 hours with no
detector firing. Replication logs growing 10 GB/day unnoticed.

So, when contributing:

- Prefer making a bad state **impossible** over detecting it after the fact.
- If a component can be absent, make its absence **named** in the logs, along
  with what is consequently disabled.
- Never let a failed operation return a success-shaped result.
- If you add a probe or a health signal, make sure a recovery clears whatever
  the failure wrote — stale state that reads as current is its own bug.

---

## Pull requests

Four **hard** CI gates run on every PR (`pr-check.yml`): `rustfmt`, `clippy`,
a Linux build, and the macOS test suite. All four must pass.

In your description, please include:

- What changed and why — the reasoning, not just the diff.
- Which verification rung you reached.
- Evidence where you have it. Measurements, before/after numbers, and log lines
  are all more persuasive than assertions.

Commit messages follow `type(scope): summary` — e.g.
`fix(aiua): resolve the MuninnDB heal-queue row when Muninn comes back`.

---

## Reporting a bug

Operational reports are genuinely valuable, especially from multi-node
deployments — that path is the least exercised. What helps most:

- What you observed, and what you expected.
- Evidence: relevant log lines, `phil status` output, measurements.
- Whether it reproduces, and what you have already ruled out.

For security issues, see [SECURITY.md](SECURITY.md) — please do not open a
public issue.
