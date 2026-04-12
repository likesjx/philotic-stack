---
title: Philotic Deployment and Environment Model
doc_type: proposal
domain: deployment-distribution
status: proposed
last_updated: 2026-04-11
tags:
- deployment
- environments
- launchd
- profile
- mbp-jane
- active-seam
related_docs:
- ARCHITECTURE_STATUS.md
- HOMEBREW_DISTRIBUTION_PROPOSAL.md
- RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: philotic-deployment
implements: []
implemented_by: []
active_seams:
- philotic-profile-namespacing
- mbp-jane-launchd-hardening
- phil-service-subcommands
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# PROPOSAL: Philotic Deployment and Environment Model

## Goal

Define the three-environment model for Philotic, establish `PHILOTIC_PROFILE` as the isolation primitive, root MBP-Jane as a permanent always-on edge production node via launchd, and give `phil` ownership of service lifecycle management.

## Core Recommendation

Treat environment isolation as a path-namespacing problem solved once by `PHILOTIC_PROFILE`. Every other concern — launchd hardening, dev protection, ground-zero testing — flows from that single primitive being in place.

## Disposition

`proposed` — decisions settled in planning; implementation not yet started.

## Current Slice

`PHILOTIC_PROFILE` path namespacing is implemented, and the current slice is the `phil service` lifecycle surface plus onboarding handoff:

- `phil service install`, `start`, `stop`, `restart`, `uninstall`, and `status`
- interactive onboarding optionally hands off to `phil service install` on macOS so first-run setup can root the daemon immediately

---

## 1. Three-Environment Model

Three environments, three separate concerns. Do not conflate them.

| Environment | Purpose | Lifecycle | Owner |
|---|---|---|---|
| **MBP-Jane** | Permanent edge production, always-on | launchd-rooted, 24/7 | `PHILOTIC_PROFILE=jane` |
| **Ground-zero** | Homebrew raze+rebuild install validation | Throwaway, separate machine | Fresh Mac or GitHub Actions |
| **VPS / Ansible** | Internet-facing deployment, Red Hat Ansible pipeline | Separate workstream | `RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md` |

MBP-Jane is never razed. Ground-zero testing is never done on MBP-Jane. VPS is a separate pipeline and out of scope here.

---

## 2. `PHILOTIC_PROFILE` — The Isolation Primitive

`PHILOTIC_PROFILE` is an environment variable that drives all path derivation in `aiua`, `phil`, and related tooling. Every runtime path is namespaced under `~/.philotic/<profile>/`.

### 2.1 Directory Layout

```
~/.philotic/<profile>/
  config.json          # mesh config (replaces mesh-config.json from repo root)
  context.db           # SQLite context graph
  aiua.sock            # IPC socket
  vault/               # encrypted vault
  logs/                # runtime logs (or symlink to ~/Library/Logs/philotic/<profile>/)
```

### 2.2 Rules

- `PHILOTIC_PROFILE=jane` — Jane's permanent production home on MBP
- `PHILOTIC_PROFILE=dev` — default for development; never conflicts with jane
- No `PHILOTIC_PROFILE` set — defaults to `dev`
- Two profiles cannot collide by construction: every path is independently namespaced

### 2.3 Dev Protection

`just start-aiua` in the dev workflow always runs with `PHILOTIC_PROFILE=dev` (set in the justfile recipe). It is structurally impossible to accidentally stomp Jane's context graph or vault from a dev run.

---

## 3. MBP-Jane — Permanent Edge Production Node

MBP-Jane (M1, 16GB, 1TB) is the primary always-on Philotic node. Jane is the resident agent. The machine may be progressively dedicated to this role.

### 3.1 Data Home

```
~/.philotic/jane/
```

All of Jane's state lives here: context graph, vault, OAuth tokens, socket. Not in the code repository. Single rsync target for backup.

### 3.2 launchd

`aiua` runs as a launchd LaunchAgent (`PHILOTIC_PROFILE=jane`). launchd owns:

- startup on login
- crash restart (keep-alive)
- log routing to `~/Library/Logs/philotic/jane/`

`aiua` supervises its own guests (philote, membrane, model-router). launchd only needs to own the one `aiua` process — guests are aiua's responsibility.

`philotic-web serve` (the mesh dashboard) is **not** always-on via launchd. It is on-demand, invoked by the operator when needed. It connects to aiua via IPC but is external to the mesh.

### 3.3 launchd Plist Shape

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.philotic.aiua.jane</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/aiua</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PHILOTIC_PROFILE</key>
    <string>jane</string>
  </dict>
  <key>KeepAlive</key>
  <true/>
  <key>RunAtLoad</key>
  <true/>
  <key>StandardOutPath</key>
  <string>/Users/jaredlikes/Library/Logs/philotic/jane/aiua.log</string>
  <key>StandardErrorPath</key>
  <string>/Users/jaredlikes/Library/Logs/philotic/jane/aiua-error.log</string>
</dict>
</plist>
```

---

## 4. `phil service` — Service Lifecycle Ownership

`phil` (the `philotic-web` CLI, operator control plane for the Philotic Web mesh) owns launchd service management via a `service` subcommand family.

### 4.1 Subcommands

| Command | Action |
|---|---|
| `phil service install [--profile <name>]` | Generate and load the launchd plist for the given profile |
| `phil service uninstall [--profile <name>]` | Unload and remove the plist |
| `phil service start [--profile <name>]` | `launchctl start` |
| `phil service stop [--profile <name>]` | `launchctl stop` |
| `phil service restart [--profile <name>]` | stop + start |
| `phil service status [--profile <name>]` | Show launchd state, PID, last exit |

`--profile` defaults to `PHILOTIC_PROFILE` from the environment if not specified.

### 4.2 Homebrew Integration

When `phil` is distributed via Homebrew, the formula declares a `service` block so `brew services start phil` works out of the box. `phil service` and `brew services` coexist cleanly — both wrap `launchctl`.

```ruby
service do
  run [opt_bin/"aiua"]
  environment_variables PHILOTIC_PROFILE: "default"
  keep_alive true
  log_path var/"log/philotic/aiua.log"
  error_log_path var/"log/philotic/aiua-error.log"
end
```

---

## 5. `phil serve` — Mesh Control Plane Dashboard

`phil serve` is the web UI/API surface for inspecting and managing the Philotic Web mesh. It is:

- **External** to the mesh — it talks to aiua via IPC, it is not a guest or membrane inside the hotel
- **On-demand** — not always-on, invoked by the operator when needed
- **Degradation-aware** — should surface partial status even when aiua is unreachable (the management surface is most valuable precisely when aiua is misbehaving)
- **Auth-required** — as a web-facing surface it needs its own auth model, independent of agent/session auth (day-one requirement per `project_desktop_aiua_integration`)

The question of whether `phil serve` should evolve toward a persistent supervised service (its own launchd plist) is deferred until the auth model and desktop embedding story are resolved.

---

## 6. Ground-Zero Testing

Ground-zero = complete raze + `brew install` rebuild, proving the install experience from scratch.

- **Never on MBP-Jane** — she is a permanent node, not a test subject
- **Primary test surface**: a friend with a fresh Mac — genuine first-run, no dev environment contamination
- **Formula CI**: GitHub Actions macOS runners (`macos-latest`, M1) for automated Homebrew formula validation — push formula change, runner installs, runs smoke, reports
- **Bottles**: pre-compiled targets (`darwin-arm64`, `darwin-x86_64`, `linux-x86_64`) — relevant when the formula moves toward bottled distribution; retest ground-zero at that point

---

## 7. Implementation Sequence

1. **`PHILOTIC_PROFILE` path namespacing** — thread through `aiua` and `phil` so all paths derive from `~/.philotic/<profile>/`. This is the blocking prerequisite.
2. **Migrate Jane to `~/.philotic/jane/`** — one-time migration of her existing context graph, vault, and config.
3. **`phil service` subcommands** — `install`, `uninstall`, `start`, `stop`, `restart`, `status`.
4. **Write and load Jane's launchd plist** — `phil service install --profile jane`. Engrain her into the machine.
5. **Dev protection in justfile** — `PHILOTIC_PROFILE=dev` hardcoded in `just start-aiua`.
6. **Build Jane's memory, skills, roles** — now that her home is stable and permanent, begin the actual agent identity work.
7. **`phil serve` auth + desktop embedding** — deferred until after Jane is rooted. See `project_desktop_aiua_integration`.
