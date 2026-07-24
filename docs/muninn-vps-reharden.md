# MuninnDB VPS hardened baseline (`proposal:muninn-vps-reharden`)

Closed out 2026-07-21. Follow-up to the 2026-07-20 incident where a cluster
rebuild left the fleet-primary Muninn on jane-vps **open**: default admin
creds (`root`/`password`) inlined in the systemd unit, unauthenticated data
plane on the `default` vault, open MCP port, and no muninn footprint in the
deploy source — so every rebuild regressed to that state.

## What the baseline enforces

The `muninn` ansible role (`ansible/roles/muninn/`, playbook
`ansible/deploy_muninn.yml`, also chained into `deploy_hotel.yml` under the
`muninn` tag) enforces, idempotently:

1. **Admin auth** — `MUNINN_ADMIN_PASSWORD` comes from
   `vault_muninn_admin_password` (ansible-vault, `vault/jane-vps.yml`),
   rendered into `~deploy/.muninn/muninn.env` (0600). Muninn's bootstrap
   re-hashes the root password from that env on every daemon start, so the
   vaulted value is authoritative even on a freshly wiped data dir. The role
   asserts the shipped default password is rejected post-deploy.
2. **No inline secrets** — the unit uses `EnvironmentFile=`, never
   `Environment=` for secrets.
3. **MCP token** — `MUNINN_MCP_TOKEN` (from `vault_muninn_mcp_token`) gates
   the MCP port 8750, which is otherwise unauthenticated by design.
4. **Locked `default` vault** — the role PUTs
   `{"name":"default","public":false}` so all REST data-plane access requires
   an `mk_` API key, and verifies an unauthenticated read 401s. (Agent/user
   vaults are created locked by aiua provisioning already.)
5. **Automated backups** — `MUNINN_BACKUP_INTERVAL=24h` into
   `~deploy/.muninn/backups` (retain 7). Muninn backups include the Pebble
   store **and** `auth_secret`, so a wiped instance can be restored with all
   API keys and admin users intact.

## Invariants (do not break these)

- **Never touch `~deploy/.muninn/data/`** during deploys. It holds the
  Pebble store (engrams + hashed `mk_` API keys + vault configs + admin
  users) and `auth_secret`. Wiping it invalidates every token the hotel has
  stored (the 2026-07-20 failure). The role only writes `muninn.env`, the
  unit file, and the backup dir.
- **`auth_secret` must not be rotated casually.** It only signs admin session
  cookies (API keys are independent SHA-256 hashes in Pebble), but it lives
  in the data dir and is regenerated if missing — treat its presence as a
  canary that the data dir survived.
- **Password rotation is safe for tokens.** Changing
  `vault_muninn_admin_password` + redeploy does not affect `mk_` keys; it
  only invalidates admin browser sessions and any automation using the old
  password.

## Tandem credential surfaces

The hotel reads muninn admin creds from `context_graph.muninn` in
`mesh-config.json` (used by `aiua --load-config` vault provisioning, which
mints per-vault tokens). The mesh-config template now renders that block from
the same `vault_muninn_admin_password`, so hotel and muninn cannot drift.
Runtime hotel traffic uses stored `mk_` tokens (vault_registry →
encrypted secrets in the context graph), not the admin password.

## Rotation runbook

```sh
# 1. Change the secret
ansible-vault edit ansible/vault/jane-vps.yml   # vault_muninn_admin_password
# 2. Re-apply baseline (restarts muninn only because env changed)
cd ansible && ansible-playbook deploy_muninn.yml --limit jane-vps
# 3. Re-render hotel config so future --load-config runs use the new password
ansible-playbook deploy_hotel.yml --limit jane-vps --tags config
```

## Known consumers of the admin password

- `aiua --load-config` vault provisioning (`crates/aiua/src/muninn_provision.rs`).
- Operator ceremonies (`scripts/muninn-cluster-ceremony.sh`).
- An unidentified host-local job triggered `POST /api/admin/backup` at
  2026-07-21 06:30Z using the default password; after rotation it will show
  up in `~deploy/.muninn/data/audit.log` as `auth.login_failed` — trace and
  update it if it recurs. (Internal `MUNINN_BACKUP_INTERVAL` backups now
  cover the same need without HTTP auth.)

## mbp-jane / mac-air

The Macs are not ansible-managed; their muninn daemons still accept the
shipped default admin password (noted 2026-07-21). Rotate them manually with
`MUNINN_ADMIN_PASSWORD` in `~/.muninn/muninn.env` + daemon restart, or fold
them into a launchd equivalent of this baseline. Their REST/UI ports are
loopback-bound, so exposure is local-only, but the default must still die.
