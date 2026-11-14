---
title: VPS Account Cutover Runbook
doc_type: workflow
domain: deployment-distribution
status: active
last_updated: 2026-11-14
tags:
- vps
- runbook
- approval
- live-steps
related_docs:
- ../architecture/VPS_ACCOUNT_SEPARATION_PROPOSAL.md
- ../architecture/OWNER_APPROVAL_VERIFIER_PROPOSAL.md
- DEPLOY_USER_DEPENDENCY_AUDIT.md
task_refs:
- docs/task.md
---

# VPS Account Cutover Runbook

Nothing in this file has been run. Each step needs Jared's explicit approval in chat for that step, on that host. The playbook `ansible/vps_account_separation.yml` refuses `jane-vps`, `vps-jane`, and `31.97.130.98` even if `vps_accounts_live_approved` is set. Pointing it at the VPS is itself a code change that belongs in the approved window.

Do not run it against `ansible/inventory/hosts.ini`. Tests use `ansible/tests/localhost.ini`.

## Still needs owner approval

1. Confirm uid 970 is free on the VPS (`getent passwd 970`) and that uid 999 is still `philotic`. The role refuses 999, the configured hotel uid, and 1001. Beta mongo was observed running as `999:999`. Do not create `philotic-approval` on 999.
2. Confirm the agent roster: which external agents exist, which group each is in (`agent-ops` or `agent-readonly`), and the public key for each. Private keys stay on the agent's machine. This repo has no real keys.
3. Create `jared-admin` and install only Jared's public keys, with the Tailscale `from=` restriction. Prove login and sudo from a Tailscale address before any other account loses access.
4. Create `philotic-approval`, `/var/lib/philotic-approval` mode `0700`, and the broker user. Install the unit and socket. Do not start them until the binary on the host is the one that contains `aiua approval-broker`.
5. Restart the hotel and philotic-web in the reboot window. Re-rendering `philotic-hotel.service` from this branch changes `InaccessiblePaths` and will restart the hotel on the next `deploy_hotel.yml` run. That overlaps Priti's beta/reboot work. Do not deploy the hotel unit from this branch during beta unless that restart is the approved window. Until `philotic_vault_key_via_load_credential` is turned on, the hotel still reads `PHILOTIC_VAULT_MASTER_KEY` from `secrets.env` and, for the `philotic-web` guest only, writes that key to a mode-0400 file and passes `PHILOTIC_VAULT_MASTER_KEY_FILE`. The key is not placed in any guest environment. Set `PHILOTIC_WEB_VAULT_KEY_FILE_HANDOFF=0` only if that file must not be written; web then has no key until the credential cutover.
6. Set `philotic_vault_key_via_load_credential=true` in that same window when ready to move the key out of `secrets.env`. That renders `LoadCredential=philotic-vault-master-key:/etc/philotic/philotic-vault-master-key`, installs the file as root mode `0400`, and omits `PHILOTIC_VAULT_MASTER_KEY` from `secrets.env`. The hotel and philotic-web read `$CREDENTIALS_DIRECTORY/philotic-vault-master-key` once. Same-uid guests can still open that file if they guess the path, because they share the hotel mount namespace. A separate web unit would close that gap and is not this slice.
7. Set `philotic_bin_owner_root=true` in that same window if `philotic-web` should stop being owned by `philotic`. The default leaves the owner alone. The next legacy binary copy with the flag on also notifies a hotel restart.
8. Remove world-read from the old context-DB backups under the hotel data dir. The graph counted about twenty root-owned `644`/`664` backups. This slice does not chmod them.
9. Move agents off `deploy` onto their named accounts. Remove their keys from `deploy` only after each agent has logged in as itself.
10. Re-home Muninn (`muninn_user`), the maintenance timer, `/home/deploy/.muninn`, `/home/deploy/muninn-backups`, and the Claude remote server. The server is not defined in this repo; confirm where it runs before changing `deploy`.
11. Set `vps_accounts_narrow_deploy=true` only after the dependency audit's pipeline rows have been repeated on staging with the narrowed sudo. The entrypoint installed by that flag is a stub that refuses to deploy. Wiring the real Ansible entrypoint is part of this step, not something the default role does.
12. Set `vps_accounts_harden_sshd=true` only after `jared-admin` login and sudo are proven. Then reload sshd as a separate approved command. The role does not reload sshd.
13. Decide the open key questions: whether Hostinger root SSH is still required, whether to delete the failing rotate-secrets and VPS-Management workflows in the other repos, whether to remove the `github_actions_deploy` private key from the Air, and what owns the `srv875272` load-check cron.

## Overlap with Priti's beta and reboot work

- The hotel unit template change restarts `philotic-hotel` the next time `deploy_hotel.yml` renders it.
- `philotic_vault_key_via_load_credential` is off, so a rendered unit does not gain `LoadCredential=` and `secrets.env` still has the vault key line when the vault var is set.
- `deploy_hotel.yml` does not import the account role. A normal hotel deploy does not create users.
- Beta mongo's `999:999` mapping is untouched.
- Traefik's `docker.sock` mount is untouched (slice A3).
- Personal MCP units under `/home/deploy` are untouched.

## What a default local run changes

On a machine where the playbook is allowed (localhost in the test inventory): the new users, groups, sudoers allowlist, request helper, approval directory, and broker unit files. It does not enable or start those units. It does not edit `deploy` or sshd while the flags are false.
