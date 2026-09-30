# `.claude/` — Claude Code configuration for philotic-stack

Rationale for `settings.json`, which cannot carry it inline.

**`settings.json` must contain nothing but schema-valid keys.** Project settings
are validated strictly: *"a file that fails validation is rejected as a whole and
reported."* Only *managed* settings parse tolerantly. So a stray `"//"` comment
key does not get ignored — it can discard the entire file, silently disabling
every deny rule and both hooks. That is exactly the failure this configuration
exists to prevent, so keep comments here instead.

To confirm the file actually loaded, run `/status` in a session and look for
`.claude/settings.json` under **Setting sources**. A file that fails to parse
does not appear there. `/doctor` shows resolved settings.

## Why a shared `settings.json` exists

Everything used to live in `.claude/settings.local.json`, which is globally
gitignored (`~/.config/git/ignore`). That meant 872 permission grants and the
only configured hook could not be shared, reviewed, or propagated to the sibling
worktrees or the other machines in the fleet.

`settings.local.json` still works and still wins for personal overrides. What
belongs here is anything that is a *project rule* rather than a personal
preference: the deny rails, the hooks, and an allowlist of commands safe for any
agent in any worktree.

## What is deliberately NOT allowlisted

Even though the local file grants them today:

- `ansible-playbook ... --vault-password-file ansible/vault/...` — full fleet deploy
- `ssh -i ~/.ssh/vps_deploy_key deploy@jane-vps "sudo ..."` — root on the VPS
- `cp` / `ln -sf` into `/opt/homebrew/Cellar/...` — mutating installed binaries

These deploy to the live fleet and should cost one confirmation. See
`proposal:agent-grant-blast-radius`.

## Hooks

| Hook | Event | Purpose |
|---|---|---|
| `hooks/guard-destructive-git.sh` | `PreToolUse(Bash)` | Blocks force-push, pushes targeting main/master, bare `git push` while on main, and `git merge` while on main. Exit 2 blocks; malformed input fails **open** so a broken guard can never wedge a session. |
| `hooks/fmt-rust.sh` | `PostToolUse(Edit\|Write)` | Runs `rustfmt` on written `.rs` files, resolving the edition from the nearest `Cargo.toml` (the workspace mixes 2021 and 2024). Always exits 0. |

Known false positive in the guard: a branch whose name contains `main` as a word
(`codex/main-thing`) is blocked by the main/master refspec check. Erring toward
over-blocking was deliberate.

The fmt hook only covers Claude Code. Seven other harnesses are registered
(codex, windsurf, four gemini/antigravity roles) and will keep introducing
rustfmt drift. The tool-agnostic fix is a git `pre-commit` hook — see
`proposal:tool-agnostic-fmt-hook`.

## `skills/`

Symlinks into the repo's top-level `skills/`. Claude Code discovers skills from
`.claude/skills/`, `~/.claude/skills/` and plugins — **never** a top-level
`skills/` — so before these existed none of the 32 authored skills were
loadable. The docs specify that a skill *entry* may be a symlink and Claude Code
follows it, which keeps the skills single-sourced where `phil graph harness`
already scans them.

Note `check-engine` still resolves to the personal
`~/.claude/commands/check-engine.md`, since personal overrides project.

## Verifying after a change

A top-level skills directory created mid-session is not watched until restart,
and settings/hook changes need a fresh session too. After changing anything
here:

1. Start a new session in this worktree.
2. `/status` → confirm `.claude/settings.json` is listed under Setting sources.
3. Confirm project skills (e.g. `graph-intelligence`) appear in the skill list.
4. Ask for `git push --force` on a throwaway ref and confirm the guard blocks it.

Full context: `docs/claude-code-harness-review.md`.
