---
title: VPS Account Separation
doc_type: proposal
domain: deployment-distribution
status: accepted-current-slice
last_updated: 2026-11-14
tags:
- vps
- accounts
- sudo
- least-privilege
- test-only
related_docs:
- ARCHITECTURE_STATUS.md
- RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md
- OWNER_APPROVAL_VERIFIER_PROPOSAL.md
- ../operations/DEPLOY_USER_DEPENDENCY_AUDIT.md
- ../operations/VPS_ACCOUNT_CUTOVER_RUNBOOK.md
task_refs:
- docs/task.md
proposal_id: bill-vps-account-separation-20261114
active_seams:
- secret-handling-hardening
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# VPS Account Separation

## Goal

Stop external agents from logging in as `deploy`. Give Jared a personal admin account, give approval state its own service user, and give each external agent a named account whose rights come from a group.

## Core Recommendation

Ansible role `vps_account_separation` creates the accounts and a sudoers allowlist. It does not change `deploy` or sshd unless the matching flag is set. Both flags default off. The role is a separate playbook from `deploy_hotel.yml`, and that playbook refuses `jane-vps` and `31.97.130.98`.

Account layout:

- `jared-admin`: personal account, `NOPASSWD: ALL`, only the public keys supplied in inventory, each key restricted with `from="100.64.0.0/10"`. No automation identity.
- `philotic-approval`: system user, uid 970 (hypothesis: free on the VPS; not 999 or 1001), shell `nologin`, no sudo, not in `deploy`, `docker`, or the agent groups. Owns `/var/lib/philotic-approval`.
- `agent-<name>`: one account per external agent. The roster is pepper (muse), priti (ChatGPT), bill (Grok Bot), and claude, all in `agent-ops`. Public keys stay out of git and are supplied at apply time. A later entry with no `group` fails the play. `agent-ops` may read journals (when `systemd-journal` exists) and may run only `/usr/local/sbin/philotic-request-restart`. `agent-readonly` gets the journal group and no sudo. The helper records a request file. It does not restart anything.

`deploy` stays as it is today: sudo, docker group, and its current authorized keys. Narrowing tasks exist and run only when `vps_accounts_narrow_deploy=true`. sshd `PermitRootLogin no` and `PasswordAuthentication no` are installed only when `vps_accounts_harden_sshd=true`, and the role never reloads sshd.

## Disposition

`accepted for current slice`

Accepted by Jared in chat on 2026-11-14. This slice is the test-only role, sudoers check, and dependency audit. It does not create users on the VPS.

## Current Slice

A1 is the role, the allowlist, the audit, and the runbook. A2 (create `jared-admin` on the VPS, prove login, then narrow `deploy`) and A3 (Docker socket review) are not started. Every live step is in [the runbook](../operations/VPS_ACCOUNT_CUTOVER_RUNBOOK.md) and needs a separate approval.

Hypothesis, not a measured fact: `jared-admin` uses `NOPASSWD` because the account is key-only and has no password. Tailscale restriction is `from=` on the authorized key, not an sshd `Match`, so the default role does not edit `sshd_config`.

## Verification

`test-green` on a local ansible run. Not `smoke-green`. Nothing was applied to jane-vps.

## Active Work Links

- [docs/task.md](../task.md)
- [Deploy user dependency audit](../operations/DEPLOY_USER_DEPENDENCY_AUDIT.md)
- [Cutover runbook](../operations/VPS_ACCOUNT_CUTOVER_RUNBOOK.md)
