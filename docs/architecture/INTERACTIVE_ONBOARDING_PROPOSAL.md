---
title: Interactive Onboarding
doc_type: proposal
domain: operator-experience
status: proposed
last_updated: 2026-03-31
tags:
- onboarding
- setup
- philotic-web
- config
- operator-experience
related_docs:
- DESKTOP_MEMBRANE_PROPOSAL.md
- PHILOTIC_DEPLOYMENT_PROPOSAL.md
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md
proposal_id: interactive-onboarding
implements: []
implemented_by: []
active_seams:
- onboarding-tui-flow
- onboarding-web-wizard
- agent-preset-library
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Interactive Onboarding

## Goal

Replace the "edit a 150-line JSON file" setup experience with a guided
interactive flow that produces a valid `mesh-config.json` from a series of
questions. The flow lives inside `philotic-web` and runs in two modes:

1. **TUI mode** — `phil init --interactive` (or just `phil init` when no
   config exists). Terminal prompts via `inquire`.
2. **Web mode** — `phil serve` shows a `/setup` wizard when no valid config
   is detected. Same questions, browser UI.

Both modes produce the same output: a valid `mesh-config.json` + identity
keypair + Muninn initialization.

## Disposition

`proposed`

---

## The Problem

Today's setup requires the operator to:

1. Run `phil init` (generates keypair + template JSON)
2. Open `mesh-config.json` in an editor
3. Replace 10+ placeholder values (`REPLACE_WITH_*`) across a nested JSON
   structure
4. Understand model configuration, approval policies, media routing, and
   Telegram bot setup without guidance
5. Get everything right on the first try — a missing comma breaks the whole
   file

This is the biggest barrier to anyone other than the author running the
stack.

---

## Design

### Tiered Question Flow

Questions are organized into three tiers. Tier 1 is mandatory. Tiers 2 and
3 can be skipped and configured later via `phil serve`.

#### Tier 1 — Get One Agent Running (required)

| # | Question | Type | Default |
|---|---|---|---|
| 1 | Hotel name | text | `default` |
| 2 | Gemini API key | password | — (required) |
| 3 | First agent name | text | `jane` |
| 4 | First agent personality (one-liner) | text | `A warm and capable assistant` |
| 5 | Connect Telegram? | confirm | no |
| 5a | Telegram bot token | password | — |
| 5b | Telegram allowed username | text | — |

After Tier 1, the operator has a working single-agent config. `phil start`
will materialize the hotel with one agent reachable via Telegram (if
configured) or the desktop membrane chat.

#### Tier 2 — Agent Fleet (optional, skippable)

| # | Question | Type | Default |
|---|---|---|---|
| 6 | Agent fleet preset | select | `solo` |
| | Options: `solo` (1 agent), `team` (3: assistant + architect + chief-of-staff), `full` (5: Jane/Aria/Beacon/Hermes/Astrid) | | |
| 7 | Per-agent Telegram tokens | password × N | skip |
| 8 | Tool approval policy | select | `ask-first` |
| | Options: `ask-first` (require approval), `preapprove-safe` (auto-approve utility+session), `trust-all` (no approval) | | |

#### Tier 3 — Advanced (optional, skippable)

| # | Question | Type | Default |
|---|---|---|---|
| 9 | ElevenLabs API key (voice) | password | skip |
| 10 | Muninn password | password | auto-generated |
| 11 | Profile name (PHILOTIC_PROFILE) | text | none |
| 12 | Mesh enrollment | confirm | skip |
| 12a | Peer hotel address | text | — |

### Agent Presets

Presets are static definitions compiled into the binary. Each preset
defines agent_id, persona_name, system_prompt, default toolset_tags, and
default approval_policy. The operator only needs to add API keys and
Telegram tokens.

```
presets/
  solo.json       →  jane only
  team.json       →  jane + aria + beacon
  full.json       →  jane + aria + beacon + hermes + astrid
```

Presets can also be loaded from `~/.philotic/presets/` for user-defined
fleet templates.

### Config Generation

The onboarding flow builds a `serde_json::Value` tree, merges operator
answers with preset defaults, and writes the final `mesh-config.json`.
Validation runs before write — the same validation that `aiua --load-config`
performs on startup.

### Web Wizard Integration

When `phil serve` starts and detects no valid config (or a config with
`REPLACE_WITH_*` placeholders remaining), the embedded UI redirects to
`/setup` instead of the dashboard. The wizard:

1. Renders the same tiered questions as the TUI but in a step-by-step
   web form
2. Validates each step before advancing
3. Writes `mesh-config.json` on completion
4. Offers "Start hotel now?" which triggers `phil start` internally
5. Redirects to the dashboard

After initial setup, config changes go through the existing
`PUT /api/config/:key` endpoint and the desktop UI's settings panels.

---

## Implementation

### Seam 1 — TUI onboarding (`phil init --interactive`)

**Files:** `crates/philotic-web/src/onboard.rs` (new),
`crates/philotic-web/src/init.rs` (extend)

**Dependencies:** `inquire` crate for terminal prompts

**Steps:**
1. Add `inquire` to `philotic-web/Cargo.toml`
2. Create `onboard.rs` with the tiered question flow
3. Create preset JSON files (embedded via `include_str!` or a `presets/`
   module)
4. Wire `phil init` to call the interactive flow when no `--config` flag
   is provided and no config exists
5. Add `phil init --interactive` flag to force the wizard even when a
   config exists
6. Validate generated config against `aiua`'s config schema before write

**Effort:** M (2-3 sessions)

### Seam 2 — Web setup wizard

**Files:** `crates/philotic-web/src/serve.rs` (extend),
`jaredlikes-desktop` (new `/setup` route)

**Steps:**
1. Add config-health check on `phil serve` startup — detect missing or
   placeholder config
2. Add `GET /api/onboard/status` endpoint — returns which tiers are
   complete
3. Add `POST /api/onboard/tier1`, `POST /api/onboard/tier2`,
   `POST /api/onboard/tier3` endpoints — accept answers, generate config
4. Embedded UI adds a `/setup` route with step-by-step wizard
5. Auto-redirect to `/setup` when onboard status shows tier 1 incomplete

**Effort:** M (2-3 sessions, depends on desktop UI changes)

### Seam 3 — Agent preset library

**Files:** `crates/philotic-web/src/presets.rs` (new)

**Steps:**
1. Define `AgentPreset` struct with all fields from mesh-config agent
   entries
2. Ship `solo`, `team`, `full` presets as compiled-in defaults
3. Support loading custom presets from `~/.philotic/presets/*.json`
4. `phil presets` subcommand to list available presets
5. Desktop UI shows preset picker in the setup wizard

**Effort:** S (1 session)

---

## What This Replaces

- The hardcoded `CONFIG_TEMPLATE` string in `init.rs` (236 lines of raw JSON)
- The expectation that operators manually edit JSON
- The implicit knowledge of what fields are required vs optional

## What This Does NOT Replace

- `mesh-config.json` as the config format — it remains the source of truth
- `aiua --load-config` — it still reads the same file
- The desktop UI config panels — they still work for post-setup changes
- Manual config editing — advanced users can still edit the JSON directly

---

## Open Questions

1. Should `phil init` default to interactive mode (always ask questions)
   or only when it detects no existing config? Recommendation: interactive
   by default, `--non-interactive` flag for scripted/CI use.

2. Should the TUI support editing an existing config (re-running the
   wizard to change API keys, add agents), or only initial setup?
   Recommendation: initial setup only for v1; editing goes through
   `phil serve` UI.

3. Should presets include Telegram bot tokens as placeholders, or should
   Telegram be a separate add-on step? Recommendation: separate step —
   not everyone uses Telegram, and bot token creation requires external
   BotFather interaction.
