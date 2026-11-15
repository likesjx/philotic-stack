---
title: Deploy User Dependency Audit
doc_type: reference
domain: deployment-distribution
status: active
last_updated: 2026-11-14
tags:
- deploy
- vps
- audit
- least-privilege
related_docs:
- ../architecture/VPS_ACCOUNT_SEPARATION_PROPOSAL.md
- ../architecture/RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md
- ../architecture/MUNINN_VPS_REHARDEN_PROPOSAL.md
- VPS_ACCOUNT_CUTOVER_RUNBOOK.md
task_refs:
- docs/task.md
---

# Deploy User Dependency Audit

Audit date: 2026-11-14. Scope: this repository (`philotic-stack`), read from the tree. No host was contacted. "Rights it needs" is what the caller does in this repo, not a measurement of the live sudoers file.

Owner decision in force: keep `deploy` exactly as it is (sudo, docker group, current authorized keys) until each row below has been tried against a narrowed account on a staging host. The narrowing tasks in `ansible/roles/vps_account_separation` default off.

## What this repo actually logs in or runs as `deploy`

| Caller | Where | What it does as `deploy` | Rights it actually needs | Narrowing note |
| --- | --- | --- | --- | --- |
| Ansible inventory | `ansible/inventory/hosts.ini`, `ansible/ansible.cfg` (`remote_user = deploy`, `private_key_file = ~/.ssh/vps_deploy_key`) | SSH to jane-vps and `become` root | SSH as `deploy`, then passwordless sudo for the hotel playbook (packages, systemd, files under `/opt/philotic` and `/etc`) | This is the pipeline. A narrowed `deploy` still needs sudo for those playbooks, or the playbooks must stop using `become`. The stub entrypoint in the role refuses to run a deploy. It is not a substitute. |
| `just vps-push` | `justfile` | `rsync` and `ssh` to `/home/deploy/code/philotic-stack`, then `cargo build`, then `ansible-playbook deploy_hotel.yml` | SSH, write `/home/deploy/code` and `/home/deploy` build output, invoke cargo, sudo via Ansible | Needs a real shell and a writable home. Not an agent-ops restart request. |
| `just vps-deploy-ci` | `justfile` | SSH, `curl`/`unzip` into `/home/deploy/ci-artifacts`, `sha256sum`, then Ansible with `philotic_bin_layout=legacy` | SSH, write `/home/deploy/ci-artifacts`, sudo via Ansible | Same as the pipeline. Does not need docker group for the recipe itself. |
| `just vps-deploy-release` | `justfile` | SSH, download into `/home/deploy/release-cache/<tag>`, `sha256sum`, Ansible with `philotic_release_tag` | SSH, write `/home/deploy/release-cache`, sudo via Ansible | Release layout already installs binaries as root (`ansible/roles/philotic_hotel/tasks/release.yml`). |
| `just vps-config` | `justfile` | Ansible `deploy_hotel.yml --skip-tags binary` | SSH and sudo, no binary copy | Config render and service restart. |
| `just rollback` for `vps-jane` | `justfile` | Ansible `deploy_hotel.yml --tags rollback` | SSH and sudo | Flips `/opt/philotic/current` and restarts the hotel. |
| `just vps-logs` | `justfile` | `ssh deploy@jane-vps` and `tail` a log under `~/.philotic` | SSH and read that log directory. No sudo in the recipe | An agent-readonly account could do this if the log directory is group-readable. Today it is not granted. |
| `scripts/verify-release.sh` | default `PHILOTIC_VPS_SSH_TARGET=deploy@jane-vps` | SSH and inspect `/opt/philotic` | SSH and read the install tree | Read-only if the tree is world-executable. Confirm before dropping sudo. |
| `scripts/uat-philote-memory.sh` | `PHILOTIC_VPS_SSH` default `deploy@jane-vps` | SSH for a UAT | SSH. The script's remote commands were not re-executed for this audit | Treat as operator UAT, not an agent account, until someone reads the remote half against a narrowed user. |
| `scripts/sync-muninn-vault-tokens.py` | `PHILOTIC_CORTEX_SSH` default `deploy@jane-vps` | SSH to the Cortex host | SSH as whoever owns the Muninn home. Not general sudo by itself | Re-home with Muninn. Do not leave token sync on a shared admin account. |
| `scripts/muninn-cluster-ceremony.sh` | `ssh deploy@jane-vps` | `pkill` muninn, `muninn backup` into `~/muninn-backups`, `muninn start` or `sudo systemctl start muninn` | SSH, write `~/muninn-backups`, and sudo for systemd if the user unit is not enough | This is an operator ceremony. It needs more than agent-ops. |
| Hotel Ansible paths | `ansible/roles/philotic_hotel/defaults/main.yml` | Paths under `/home/deploy` (`release-cache`, `muninn-backups`, `.muninn/data`, and host_vars `ci-artifacts`, `apps/jane`) | The deploying user must be able to write those directories. The hotel process itself runs as `philotic`, not `deploy` | Moving these directories is a live cutover, not a default-role change. |
| Muninn role | `ansible/roles/muninn/defaults/main.yml` `muninn_user: deploy` | `muninn.service` `User=deploy`, state under `/home/deploy/.muninn` | A dedicated user that can read its env file and write its data dir. It does not need `NOPASSWD: ALL` or the docker group for the unit as written | Re-home is a live step. The unit template follows `muninn_user`; this slice does not change that default. |
| Muninn maintenance timer | `ansible/roles/philotic_hotel/templates/muninn-maintenance.service.j2` | `User={{ philotic_deploy_user \| default('deploy') }}` | Read/write the backup dir the script names | Same re-home. |
| Edge file notes | `ansible/files/vps-jane-edge/` | Documents Traefik dynamic config under `/home/deploy/traefik` and container `deploy-traefik-1` | Whoever edits that file needs write access to the Traefik dynamic directory. The container, not the `deploy` user, holds the docker socket | A3, not A1. Do not remove `deploy` from `docker` until that socket mount is reviewed. |
| Personal MCP notes | `services/personal-mcp/LIVE_APPROVAL_BUNDLE.md` | Records Muninn, the Node gateway, and Traefik as they were observed running under `deploy` / `/home/deploy` | Those services need their own users and read-only mounts. This slice does not change them | Overlaps Priti's beta/ops notes. Left untouched. |

## GitHub Actions in this repository

`.github/workflows/` has no `ssh`, no `ansible-playbook`, and no `deploy@` target. `build-linux.yml` builds the linux artifact. `just vps-deploy-ci` is what downloads it and SSHes as `deploy`, and a person or a runner outside this workflow file invokes that recipe.

The graph export (2026-11-14) says other repositories still SSH as `deploy`: `jaredlikes-com` and `jaredlikes-monorepo` secret `VPS_SSH_KEY`, `jaredlikes-desktop` secret `VPS_SSH_PRIVATE_KEY`, a monthly `rotate-secrets.yml` that has been failing, and VPS-Management workflows whose secrets do not exist. Those YAML files are not in this repo. This audit does not confirm their current contents. Do not narrow `deploy` until someone opens those repos and lists the remote commands.

## systemd, docker, backups, Claude, Muninn

- Hotel, ONNX sidecar, and philotic-web-the-binary run as `philotic` (the graph's read-only note says uid 999). They are not `deploy`. `philotic-web` is a binary in `philotic_binaries`, not its own unit.
- Legacy binary copies are owned by `philotic` unless `philotic_bin_owner_root=true`. Release copies are already root. The live "philotic-web binary owned by philotic" finding matches the legacy copy task.
- `philotic-memgraph.service` runs a container as `philotic_memgraph_uid` default 101. The graph note that beta mongo runs as `999:999` is a different container and is not changed here. That uid collides with the hotel user if both exist on the host.
- No compose file in this repo mounts `docker.sock`. The graph and `ansible/files/vps-jane-edge/traefik-mcp-membrane.yml` say the live Traefik container mounts it read-write. That mount is root-equivalent. It is not modified.
- Backups: `muninn_agent_backup_dir` is `/home/deploy/muninn-backups`. `scripts/muninn-cluster-ceremony.sh` writes there over SSH. A fleet script `~/.philotic/bin/muninn-fleet-backup.sh` was named in the graph export and is not in this repo.
- Claude remote server: not defined in this repo. The graph says it currently runs as `deploy` and must be re-homed. There is no unit, playbook, or script here that starts it. Treat that as an external fact to re-check on the host, not as something this change moved.
- Claude Code harness docs (`.claude/README.md`, `docs/claude-code-harness-review.md`) show operators running `ssh -i ~/.ssh/vps_deploy_key deploy@jane-vps`. That is Jared or an agent on a laptop using the shared `deploy` key. It needs whatever sudo the operator types. It is the thing `jared-admin` is meant to replace for Jared, and a named `agent-*` account is meant to replace for agents.

## Rights the new accounts are for

| Account | Rights in the default role |
| --- | --- |
| `jared-admin` | Full sudo. SSH only from `100.64.0.0/10` via `authorized_keys` `from=`. |
| `deploy` | Unchanged. |
| `philotic-approval` | No shell, no sudo, owns `/var/lib/philotic-approval`. |
| `agent-ops` | `systemd-journal` when that group exists. sudo only for the restart-request helper. |
| `agent-readonly` | `systemd-journal` when that group exists. No sudo. |

## What is not proven

- Which of deploy's sudoers rules on the live host are `NOPASSWD: ALL` versus something narrower. The graph says `NOPASSWD ALL`. This repo's playbooks assume `become: true` works.
- Which authorized keys are on the live `deploy` account. The role does not read or write them when the flag is off.
- Whether anything outside this repo (cron on the VPS, the Air load-check script, Hostinger root login) still needs `deploy` or root. The graph lists those as open questions. They are still open.
