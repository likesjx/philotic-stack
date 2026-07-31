# Claude Code harness review (`proposal:claude-code-harness-review`)

Review of how Claude Code is used in this repo, 2026-07-27. Implementation
landed in PR #371; the follow-ups below are the part that was deliberately not
implemented.

> **Why this is a doc and not only graph nodes.** The follow-ups below were
> filed with `graph_create_node`, per the standing rule that proposals live in
> the intel-graph rather than in markdown. But DEF-072 records that
> `graph_create_node` proposals are wiped by the 6h rescan — only `doc:`-backed
> proposals persist. This file is the durable backing until that defect is
> fixed. Delete it once graph-native proposals survive a rescan.

## Verdict

The repo has an unusually developed *agent operating discipline* — harness
charters, a project graph, Muninn memory, SVE verification ladders,
worktree-per-workstream, 32 authored skills. Almost none of it was **enforced by
machinery**. Every rule was carried by prose in `CLAUDE.md`, `AGENTS.md` and
skill documents, which is the weakest available enforcement for a fleet that
auto-merges its own PRs and deploys to three hosts.

The through-line: **nothing mechanically stopped a bad change from reaching
`develop` or the fleet.**

## Measured baseline (2026-07-27)

| Check | Result |
|---|---|
| `cargo test --workspace --no-fail-fast` | 2078 passed, 0 failed, 89 binaries (on `fe8bf77b`, **on a Mac with an unlocked Keychain** — see below) |
| `cargo clippy --workspace --all-targets` | exit 0, hundreds of warnings (aiua alone: 313) |
| `cargo fmt --all --check` | dirty |
| `just test` honesty | **honest** — runs `cargo test` unconditionally, propagates exit code |
| Skills discoverable by Claude Code | **0 of 32** |
| CI gate on pull requests | **none** |
| `.githooks/pre-push` secret guard | **inert** |

Two results that contradicted prior notes and are worth keeping:

1. **`just test` is not vacuous.** The "graceful no-op" in
   `scripts/test-and-record.sh` only skips *graph recording* when :8900 is down.
   `cargo test --workspace` runs unconditionally and `exit "$TEST_EXIT_CODE"`
   propagates real failures. The "honest green" policy rests on a sound signal.
2. **The `aiua` test hang is environment-dependent, not stale.** `MEMORY.md`
   said `cargo test -p aiua` stalls at `desktop_membrane` lease tests. It runs
   clean on a developer Mac — 2078 passed, no skips — so it initially looked
   retired. CI then reproduced a hang, and the cause turned out to be the macOS
   Keychain shell-out (below), which blocks wherever the login keychain is not
   unlocked. Correct the memory rather than deleting it: the suite is green on
   an interactive Mac and hangs everywhere else.

## What PR #371 changed

1. **`pr-check.yml`** — the missing gate. `build-linux.yml` runs on push to
   develop (after merge, no tests); `release.yml` runs on tags. Nothing verified
   a PR, while the standing policy is to auto-merge agent PRs "at honest green"
   — a green self-reported by the agent that wrote the code.

   **What actually landed:** `cargo fmt --all --check` is a hard gate on PRs and
   runs in ~16s. `cargo test --workspace` runs but is **advisory
   (`continue-on-error`), not blocking** — because it does not terminate.

   **Why, and this is the most valuable thing the gate found.** The first two
   runs timed out during compilation (90 min, then 180), which suggested build
   cost. That diagnosis was wrong. On the third run, with a longer limit, the
   **build step succeeded** and the cargo cache saved at **2.03 GiB** — well
   under GitHub's 10 GB limit, so caching is viable. It was `cargo test`
   *execution* that ran 5h50m to the limit.

   The orphan processes GitHub terminated at teardown name the culprit:
   `aiua-2f3393f6dd` and three **`security`** processes — the macOS Keychain
   CLI. `crates/aiua/src/vault.rs` shells out to `security
   find-generic-password` and `add-generic-password` for the vault root key.
   On a host with no unlocked login keychain those calls **block indefinitely**.
   `load_keychain_root_key` tolerates the non-interactive *error* case (exit 36
   / "User interaction is not allowed" → `Ok(None)`), but
   `store_keychain_root_key` has no equivalent, and neither survives the call
   simply hanging. `crates/philotic-web` (`serve.rs`, `doctor.rs`) does the same.

   So the workspace test suite cannot complete on **any** machine without an
   unlocked login Keychain — a fresh Mac, a CI runner, a headless box. This is
   almost certainly the real explanation for the long-standing "cargo test -p
   aiua hangs at desktop_membrane" reports, which were assumed to be a tokio
   deadlock. Fix (`proposal:aiua-tests-hang-on-keychain`): an escape hatch that
   skips the Keychain backend for the existing file backend, plus a timeout
   around every `security` invocation. That unblocks
   `proposal:pr-test-gate-viability`.

   Not fixed in this PR: it is a change to the vault security path and deserves
   its own review, not a fold-in to a harness PR.
2. **`rust-toolchain.toml`** pinned to 1.94.0, with `build-linux.yml` and
   `release.yml` passing the version explicitly alongside their `targets:`
   input. Caveat: rustup is not installed on the dev Macs, so the pin governs
   CI only until it is.
3. **Skills made discoverable.** Claude Code reads `.claude/skills/`, never a
   top-level `skills/`. All 32 lived in `skills/`, so none were loadable —
   including 6 of the 7 the `claude-local` charter declares Active. 12 also had
   malformed or absent frontmatter. Frontmatter repaired and validated 32/32;
   per-skill symlinks added.
4. **Shared `.claude/settings.json`.** Everything had lived in
   `settings.local.json`, which is globally gitignored, so 872 grants and the
   only hook could not be shared or reviewed across 14 worktrees.
5. **Enforcement hooks.** `guard-destructive-git.sh` (PreToolUse) makes "never
   push to main / never force-push / never merge to main" mechanical;
   `fmt-rust.sh` (PostToolUse) keeps Rust formatted.

   **Trap avoided, worth recording:** project `settings.json` is validated
   strictly — *"a file that fails validation is rejected as a whole and
   reported"*; only *managed* settings parse tolerantly. A `"//"` comment key
   does not get ignored, it can discard the entire file and silently disable
   every deny rule and both hooks. Rationale therefore lives in
   `.claude/README.md`, not inline. Confirm the file actually loaded with
   `/status` → **Setting sources**.



## Follow-ups — ALL NINE IMPLEMENTED

Every follow-up below was implemented on `codex/claude-harness-hardening`
(PR #371), plus `proposal:aiua-tests-hang-on-keychain`, which the test gate
depended on. Status per item is inline.



1. ✅ `proposal:secret-push-guard-activation` — **DONE.** `just install-git-hooks`
   run; `core.hooksPath` now `.githooks`, verified to apply to the main checkout
   too (shared `.git/config`), so all 18 worktrees are covered. The guard fired
   on a real push for the first time ever: *"secret-push-check: scanned 6
   commit(s); no forbidden secrets found"*. `engine-check.sh` now asserts it
   stays wired.

   Original finding: **highest value, one command.**
   `.githooks/pre-push` invokes `scripts/secret-push-check.py`, but
   `core.hooksPath` points at an empty `.git/hooks`. The guard has never fired,
   despite `backup-pre-secret-rewrite-20260313` in history. `just
   install-git-hooks` fixes all worktrees at once (`.git/config` is shared).
2. ✅ `proposal:harness-verify-resolves-skills` — **DONE.** verify now resolves each declared skill (exists / frontmatter valid / reachable from the runtime's discovery path) and reports `drifted` with the specific failure. Proven: it caught `implementation`, then `planning` and `verification`, where the old one said `clean`.

   Original finding: — `phil graph harness verify`
   reported `claude-local: clean` while all 7 declared skills were unreachable
   and one (`implementation`) does not exist anywhere on disk. It checks the
   managed CLAUDE.md block and never resolves the skill list against the
   filesystem. It attests that a broken harness is healthy.
3. ✅ `proposal:agent-grant-blast-radius` — **DONE.** 923 → 728 entries; 185 dangerous grants demoted to prompt. Details above.

   Original finding: — of the 872 grants, ~a third are parser
   garbage, but the list also contains blanket grants for `ansible-playbook
   --vault-password-file`, `ssh deploy@jane-vps "sudo …"`, and `cp`/`ln` into
   `/opt/homebrew/Cellar`. Any agent can run those unattended.
4. ✅ `proposal:declare-system-dependencies` — **DONE.** `just preflight` + README table + `engine-check.sh` assertion. Verified it fails correctly when Opus is absent.

   Original finding: — found by the new gate on its first
   run. `membrane-discord` depends on `opus`; its `-sys` crate asks pkg-config
   first and only builds from source on failure. The dev Macs have opus from
   Homebrew so the source path never ran locally and the dependency was
   invisible. A bare runner took it and died on CMake 4. Audit the other
   `-sys`/`build.rs` crates and document the required system packages.
5. ✅ `proposal:pr-test-gate-viability` — **DONE.** The test job is now a hard gate. Three things had to be fixed first: the Keychain hang, a test-isolation bug where two tests `remove_var`'d the vault key out from under three others, and a TOCTOU port race in graph-intelligence's server tests. Verified under CI conditions: 2340 passed / 0 failed.

   Original finding: — **make the test gate blocking.** Cold
   `cargo test --workspace` on macos-14 measured 2h59m and did not finish;
   cancelled runs never saved the cargo cache, so nothing ever got warm. Work
   needed: warm the cache from a develop push (now wired), confirm `target/`
   fits under GitHub's 10 GB cache limit, shard the workspace across a matrix,
   evaluate `cargo-nextest`, and keep `CARGO_PROFILE_TEST_DEBUG=0`. Flip
   `continue-on-error` off only after a cached run finishes well inside the
   timeout.
6. ✅ `proposal:tool-agnostic-fmt-hook` — **DONE.** `.githooks/pre-commit` checks staged Rust files only. Verified both ways: unformatted → exit 1, formatted → exit 0.

   Original finding: — the PostToolUse rustfmt hook covers Claude
   Code only, but eight harnesses are registered (codex, windsurf, four
   gemini/antigravity roles) and any of them can drift develop. This is not
   theoretical: PR #371 went red on its own merge commit within a day, because
   develop picked up unformatted code from another harness after the branch was
   cut. Move the enforcement to a git `pre-commit` hook, which applies whoever
   commits — and which rides along with the `just install-git-hooks` step
   already needed for (1).
7. ✅ `proposal:clippy-ratchet-workspace-lints` — **DONE.** `[workspace.lints.clippy]` + per-crate opt-in; **17 of 32 crates gated at deny**, and a hard clippy job in CI (passes in ~3m). Five crates were deliberately backed out and named as the next steps.

   Original finding: — clippy is not in the PR gate
   because hundreds of existing warnings would make it red on arrival. Clean
   crates one at a time behind `[workspace.lints]`, then add it blocking.
8. ✅ `proposal:claude-charter-in-repo` — **DONE.** Charter projects to `.claude/harness/claude-local.md` with a relative import; hook moved to the version-controlled `settings.json`. Also fixed the phantom skills at source across nine canonical profiles.

   Original finding: — `.claude/CLAUDE.md` is checked in and
   contains only `@/Users/jaredlikes/.claude/philotic/harnesses/claude-local/CLAUDE.md`.
   On any other machine or in CI that resolves to nothing and the charter
   silently vanishes. The charter should live in-repo with the harness tool
   syncing into it.
9. ✅ `proposal:pr-linux-compile-check` — **DONE.** `cargo check` job over exactly build-linux.yml's package set (parity asserted), passing in ~2m.

   Original finding: — the PR gate is macOS-only, so a PR can
   break the vps-jane build and not find out until it lands on develop. Add a
   `cargo check` job on ubuntu over the package set `build-linux.yml` builds.

## Follow-up outcomes

All nine were implemented on `codex/claude-harness-hardening` (PR #371).
Notes on the two whose effect is not visible in the diff:

- **`proposal:agent-grant-blast-radius`** — `.claude/settings.local.json` is
  globally gitignored, so the prune leaves no commit. Recorded here instead:
  **923 → 728 entries.** Removed 10 shell fragments produced by the permission
  prompt splitting multi-line commands (`Bash(done)`, `Bash(do echo ...)`), and
  demoted **185** grants back to prompting — 89 that mutate installed binaries
  under `/opt/homebrew/Cellar`, 31 `ssh … sudo` (remote root on jane-vps), 20
  `ansible-playbook` fleet deploys, 18 other remote shells, 11 launchctl/
  systemctl, plus rm -rf, pkill, brew mutations, git push and rsync. Read-only
  `Read(/opt/homebrew/...)` grants were kept. A backup of the original is at
  `$CLAUDE_JOB_DIR/tmp/settings.local.json.backup`.
- **`proposal:claude-charter-in-repo`** — also fixed the phantom skills at their
  source. **Four** skills (`planning`, `verification`, `implementation`,
  `review`) were declared across **nine** canonical profiles and exist nowhere
  on disk; all nine were re-registered against real skills. `implementation` on
  the claude-local charter was a symptom, not the disease.

## Not filed, but noted

- **MCP surface.** `muninn` and `muninn-local` are both connected and expose
  ~45 near-identical tools each. `enableAllProjectMcpServers: true` plus the
  claude.ai connectors pulls personal-finance and fitness tools into a Rust
  infrastructure session.
- **Instruction load** is ~43 KB before any work begins (`AGENTS.md` 24.7 KB,
  global `CLAUDE.md` 10 KB, project `CLAUDE.md` 7.6 KB). `AGENTS.md` at 616
  lines is the first place to look for consolidation.
- **Worktree sprawl** — 18 worktrees, 14 under `.claude/worktrees/`, several for
  work already shipped. Reaping is destructive; check `status --porcelain` and
  merged-ness first.

## What is working and should be kept

- `release.yml`'s `validate-branch` job, enforcing that stable tags come from
  `main` and pre-release tags from `develop`. It is the machinery-not-prose
  pattern the rest of the setup lacked, and the model for everything above.
- `scripts/test-and-record.sh` — records to the graph while never letting graph
  downtime mask a test failure.
- Worktree-per-workstream with `workstream-status` / `workstream-overlap`
  hot-file overlap detection.
- Muninn and the intel-graph kept as separate concerns: learned context versus
  structural truth.
