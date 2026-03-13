---
title: "Red Hat Ansible And VPS Deployment Proposal"
doc_type: proposal
domain: deployment-distribution
status: accepted-current-slice
last_updated: 2026-03-12
tags:
  - deployment
  - vps
  - ansible
  - secrets
  - transitional
related_docs:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
  - GUEST_BINARY_RESOLUTION_PROPOSAL.md
  - NATIVE_OVERLAY_VPN_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: rh-ansible-vps-deployment
implements: []
implemented_by:
  - vps-boundary-contract-slice
active_seams:
  - secret-handling-hardening
  - watched-live-vps-smoke
  - artifact-distribution-rollout
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# Red Hat Ansible And VPS Deployment Proposal

## Goal

Define the authority boundary between Philotic's hotel runtime and Red Hat Ansible as the external infrastructure orchestrator, with VPS deployment as the first concrete target.

## Core Recommendation

- Red Hat Ansible should own machine provisioning, package/runtime installation, service units, secrets/bootstrap placement, peer inventory, and rollout operations.
- Philotic `ansible` should own hotel runtime state, guest materialization, session/context authority, and inter-hotel runtime behavior once the process is running.
- Peer topology should be rendered explicitly for deployed hotels instead of inferred from local loopback assumptions used in development.

## Disposition

`accepted for current slice`

## Current Slice

This slice defines the first concrete VPS deployment contract.

What is accepted:
- one hotel per deployed machine is the default deployment model
- Red Hat Ansible is the outer control plane for VPS/bootstrap operations
- Philotic hotel config remains the inner runtime authority
- Tailscale is a named transitional scaffold for inter-hotel peer addressing (see Transitional Network Constraint)
- peer inventory schema carries explicit `host`/`beacon_port` fields now so Tailscale can be swapped without a schema migration
- build-on-host strategy is explicitly transitional; pre-built artifact distribution replaces it when CI cross-compilation is ready
- `philotic` (non-root) system user runs the hotel daemon with progressive systemd hardening

What remains deferred:
- watched live VPS deployment smoke (`jane-vps` first hotel)
- multi-host or local-to-VPS two-hotel roundtrip
- pre-built artifact fetch replacing on-host compilation
- Tailscale installation/auth automation in the role (currently flagged as a manual prerequisite)
- firewall hardening beyond the Tailscale CGNAT range restriction on the blob port

## Transitional Network Constraint

Inter-hotel mesh peer addressing currently depends on Tailscale MagicDNS. This is a **named transitional scaffold**, not a permanent architectural commitment.

Long-term intent is a native VPN built through the hotels themselves. That work is deferred behind hardening requirements and higher-priority functionality. Until then:

- Tailscale is a **host prerequisite** for any deployed hotel participating in the mesh
- `backbonePeers` entries must resolve via Tailscale MagicDNS (e.g. `jane-vps`)
- The peer inventory/rendering contract should carry explicit host authority fields now so the Tailscale dependency can be dropped without a schema change when the native VPN lands

## Proposed Ownership Split

### Red Hat Ansible owns

- host inventory
- Linux package/runtime prerequisites
- binary placement or build artifact deployment
- service lifecycle (`systemd`, restart, rollback)
- Tailscale installation and auth key provisioning (transitional; will be replaced by native hotel VPN)
- initial config and secret material placement
- peer address rendering for hotels

### Philotic owns

- context graph contents
- hotel identity and guest manifests
- guest materialization and supervision
- session state and routing
- mesh event dispatch/ack behavior
- blob/event/cursor persistence
- agent identity import and runtime config projection

## VPS Deployment Contract

### Host Prerequisites

- Ubuntu 22.04+ (Debian family)
- Tailscale installed and authenticated (**transitional** — required for inter-hotel mesh until native hotel VPN lands)
- SSH access: `deploy` user with sudo capability
- Inbound UDP 8999 open for inter-hotel beacon traffic
- Inbound TCP 9001 restricted to Tailscale CGNAT range (`100.64.0.0/10`) for blob transport

### Filesystem Layout

```
/opt/philotic/
  bin/
    ansible          # hotel daemon
    membrane          # Telegram gateway guest
    agent-core       # persona/cognitive loop guest
    model-router     # LLM routing guest
  etc/               # mode 0700 — philotic user only
    mesh-config.json # hotel runtime config (rendered by Ansible, no secrets)
    vault-bootstrap.json.enc # encrypted bootstrap material only when platform-native vault init is required
  data/
    context.db       # SQLite context graph (owned by hotel daemon)
    blob/            # blob store (HTTP content-addressed artifacts)
  src/               # build checkout (transitional; remove when artifact fetch lands)
```

### Runtime Ports

| Port | Proto | Purpose |
|---|---|---|
| 8999 | UDP | Inter-hotel mesh beacon (BeaconMessage envelope) |
| 9001 | TCP | Blob HTTP store (content-addressed artifact transport) |
| 9002 | TCP | Inter-hotel execution plane (point-to-point routed task transport) |
| `/tmp/philotic-ansible.sock` | UDS | Intra-hotel IPC (guests ↔ hotel daemon) |

### Service Manager Shape

- systemd service: `philotic-hotel.service`
- Runs as `philotic` system user (non-root)
- avoid plaintext `EnvironmentFile` secrets on disk
- if runtime bootstrap material is needed, it should be encrypted at rest and used only to initialize or unlock the hotel-owned vault / platform secret store
- Progressive systemd hardening: `NoNewPrivileges`, `PrivateTmp`, `ProtectSystem=strict`
- `After=tailscaled.service` (transitional; remove when native hotel VPN lands)

### Config and Secrets Inputs

| Input | Source | Location on host |
|---|---|---|
| `mesh-config.json` | Rendered by Ansible from `host_vars/<host>.yml` | `/opt/philotic/etc/mesh-config.json` |
| encrypted vault bootstrap (optional) | Rendered from Ansible vault | `/opt/philotic/etc/vault-bootstrap.json.enc` |
| `PHILOTIC_MESH_PSK` | Ansible vault / platform secret store | Loaded into the hotel-owned vault or injected through a non-persistent secret-store path |
| Telegram bot tokens | Ansible vault per-agent | Stored in the hotel vault or platform secret store, never rendered into plaintext `mesh-config.json` |

### Secret Handling Rule

Raw secrets should not be written to disk unencrypted.

That means this deployment contract should converge toward:

- plaintext secrets do not live in `mesh-config.json`
- plaintext secrets do not live in `secrets.env`
- RH Ansible may render encrypted bootstrap material when absolutely necessary
- the hotel should then import or unlock those secrets into the local vault / platform secret store
- steady-state runtime should use secret references and hotel-owned vault access, not flat secret files

Transitional convenience is not a valid reason to leave money lying around on disk.

### Binary / Artifact Placement

**Transitional (current):** Ansible clones the repo to `/opt/philotic/src`, builds with `cargo build --release`, and installs binaries to `/opt/philotic/bin/`.

**Intended:** CI pipeline cross-compiles for `x86_64-unknown-linux-gnu`, signs artifacts, and Ansible fetches and installs pre-built binaries. Eliminates Rust toolchain dependency on VPS nodes.

### Peer Inventory Rendering

Backbone peers are rendered explicitly per hotel in `mesh-config.json`. Each peer carries:
- `name` — logical hotel name
- `host` — Tailscale MagicDNS name today; explicit IP or hostname when native VPN lands
- `beacon_port` — defaults to 8999

Schema is stable across the Tailscale → native VPN transition. Only `host` values change.

### Playbook

```bash
# Full deploy
ansible-playbook -i ansible/inventory/hosts.ini ansible/deploy_hotel.yml

# Single host
ansible-playbook -i ansible/inventory/hosts.ini ansible/deploy_hotel.yml --limit jane-vps

# Config-only (no rebuild)
ansible-playbook -i ansible/inventory/hosts.ini ansible/deploy_hotel.yml --tags config
```

## VPS Target

The first deployment target is `jane-vps` — one Philotic hotel with a materialized guest stack (`membrane`, `agent-core`, `model-router`). Multi-hotel and mixed local/VPS deployments build on the same contract.

## Active Work Links

- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
