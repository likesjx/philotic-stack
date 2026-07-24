---
title: Muninn VPS Reharden
doc_type: proposal
domain: deployment-distribution
status: implemented
last_updated: 2026-07-21
related_docs:
- docs/muninn-vps-reharden.md
tags:
- muninn
- security
- ansible
- vps
---

# proposal:muninn-vps-reharden — IMPLEMENTED

Filed 2026-07-21 after the muninn 401 incident (handoff PR #339, follow-up 5).
**Shipped the same day**: PR #346, applied live on jane-vps and verified.

## Problem

The 2026-07-20 cluster rebuild left the fleet-primary Muninn on jane-vps
**open**: `root`/`password` inlined in a hand-placed systemd unit,
unauthenticated data plane on the `default` vault, open MCP port (8750), and
zero muninn footprint in the deploy source — every rebuild regressed to this
state ("rebuilds keep dropping Muninn's auth + keys").

## Decision

Restore admin + token auth **without touching the data dir** (preserves
`auth_secret` and the resynced `mk_` keys), and bake the hardened baseline
into ansible so rebuilds restore it. Key model correction discovered during
implementation: `auth_secret` only signs admin session cookies; `mk_` API
keys are independent SHA-256 hashes in the Pebble store — the Jul-20 token
loss was the Pebble wipe, not secret rotation. Hardening was therefore
provably safe for existing tokens.

## Shipped (PR #346)

- `ansible/roles/muninn/` + `ansible/deploy_muninn.yml`, chained into
  `deploy_hotel.yml` (tag `muninn`): vaulted admin password + MCP token →
  `~deploy/.muninn/muninn.env` (0600, `EnvironmentFile=`), `default` vault
  locked, daily internal backups (include `auth_secret` + Pebble key store),
  post-deploy assertions (default password rejected, unauthenticated data
  plane 401s). Never writes under the data dir.
- `mesh-config.json.j2` renders `context_graph.muninn` from the same vault
  secret — hotel provisioning and muninn admin creds cannot drift.
- Runbook + invariants: `docs/muninn-vps-reharden.md`.

Applied live 2026-07-21: `auth_secret` sha unchanged, resync keys intact,
old creds 401, hotel journal clean, second playbook run `changed=0`.

## Open follow-ups

- Unidentified nightly-backup client used the default password; will surface
  as `auth.login_failed` in `~deploy/.muninn/data/audit.log` — trace and fix.
- mbp-jane / mac-air muninn daemons still accept the default admin password
  (manual rotation documented in the runbook).
