---
name: command-center-context
description: Use this skill when working in Jane Command Center to run session bootstrap, structured handoff, Aria ingestion, context validation, and inventory refresh. Trigger when users ask about handoffs, living context, startup context, cross-agent continuity, or command-center operational context hygiene.
---

# Command Center Context

Run this skill for any task that touches coding-agent session continuity in Jane Command Center.

## Goals

- Keep every session start/end structured.
- Preserve continuity between external coding agents and internal agents (especially Aria/architect).
- Keep command-center context docs aligned with executable workflows.

## Required Workflow

0. Run `just --list` first, then read the repo's `justfile` and treat it as the executable source of truth for local automation.
   - This is the default discovery step before attempting any Jane Command Center lifecycle command.
   - If the repo exposes Jane Command Center lifecycle recipes, use the repo's exact recipe names and arguments.
   - If those recipes are absent, do not invent them. Fall back to Jane Command Center's canonical `justfile` at `$(command -v just >/dev/null && just --evaluate JANE_COMMAND_CENTER_JUSTFILE || echo "/path/to/fallback")`, note the gap, and continue with the closest safe equivalent.
1. Validate context system first when the repo provides a validation recipe.
   - Prefer the repo's `justfile` entry over assumed defaults such as `just context-check`.
   - When falling back to Jane Command Center, use `just -f $(command -v just >/dev/null && just --evaluate JANE_COMMAND_CENTER_JUSTFILE || echo "/path/to/fallback") context-check`.
2. Start (or confirm existing) handoff scaffold when the repo provides a handoff bootstrap recipe.
   - Prefer the repo's `justfile` entry over assumed defaults such as `just session-start AGENT=<agent> GOAL="<goal>" TO=<receiver>`.
   - When falling back to Jane Command Center, use `just -f $(command -v just >/dev/null && just --evaluate JANE_COMMAND_CENTER_JUSTFILE || echo "/path/to/fallback") session-start AGENT=<agent> GOAL="<goal>" TO=<receiver>`.
3. Make requested changes.
4. If behavior/policy changed, update the repo's documented command-center context files when they exist.
   - Common examples include `AGENTS.md`, `docs/JANE-REFERENCE-MAP.md`, and `docs/DECISIONS.md`, but verify the actual file set in the repo before assuming those exact paths.
5. Complete and submit handoff when the repo provides a handoff completion recipe.
   - Prefer the repo's `justfile` entry over assumed defaults such as `just session-handoff FILE=<path> TO=<receiver>`.
   - When falling back to Jane Command Center, use `just -f $(command -v just >/dev/null && just --evaluate JANE_COMMAND_CENTER_JUSTFILE || echo "/path/to/fallback") session-handoff FILE=<path> TO=<receiver>`.
6. If receiver is Aria/architect and the repo provides ingestion automation, run it.
   - Prefer the repo's `justfile` entry over assumed defaults such as `just aria-ingest-handoffs`.
   - When falling back to Jane Command Center, use `just -f $(command -v just >/dev/null && just --evaluate JANE_COMMAND_CENTER_JUSTFILE || echo "/path/to/fallback") aria-ingest-handoffs`.
7. Refresh inventory surface when the repo provides a refresh recipe.
   - Prefer the repo's `justfile` entry over assumed defaults such as `just inventory-refresh`.
   - When falling back to Jane Command Center, use `just -f $(command -v just >/dev/null && just --evaluate JANE_COMMAND_CENTER_JUSTFILE || echo "/path/to/fallback") inventory-refresh`.
8. Run final validation when the repo provides it.
   - Prefer the repo's `justfile` entry over assumed defaults such as `just context-check`
   - When falling back to Jane Command Center, use `just -f $(command -v just >/dev/null && just --evaluate JANE_COMMAND_CENTER_JUSTFILE || echo "/path/to/fallback") context-check`

## Handoff Requirements

Submitted handoff must include non-placeholder content for:

- Goal
- What changed
- Decisions made
- Risks
- Open loops
- Exact next commands
- Files touched

## Aria First-Class Rule

Treat Aria (`architect`) as first-class operational authority:

- Intake Aria-targeted handoffs promptly.
- Preserve architecture-impact decisions in `docs/DECISIONS.md`.
- Escalate architecture conflicts to Aria instead of silently overriding.

## References To Load As Needed

- `references/quick-commands.md`
- `references/context-files.md`
