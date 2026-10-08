# Scoped rollout and legacy rollback

Source preparation only. No production change is authorized by this document.
The dedicated play `ansible/deploy_personal_mcp.yml` defaults to disabled and
does not import hotel, Muninn, identity, database seed or vaulted-value tasks.
Current observed VPS installation is `/opt/philotic/bin/aiua`, with no
`/opt/philotic/current` release link; release-link rollback alone cannot restore it.

Before an operator-approved rollout:

1. Refresh `develop`, review concurrent IPC/Beacon/release changes and obtain green
   PR Check, Build Linux and Percival Linux acceptance for the exact source SHA.
   Install the approved native x86_64 hotel artifact through the release procedure,
   preserving the existing Beacon drop-ins. Record binary path and SHA256; the
   scoped role asserts that exact checksum before configuring activation.
2. Before switching away from legacy layout, preserve the prior aiua binary and
   base `philotic-hotel.service` in a root-owned 0700 rollback directory; record
   original paths, ownership, modes and SHA256. Record the names/hashes of existing
   drop-ins, the absence/presence and target of the release link, and prior gateway
   unit/config paths. Do not print or copy credential environment values. Do not
   overwrite existing snapshots. This backup is an operator action, not performed
   during source validation.
3. Verify a free numeric UID and GID for `percival-gateway` (991 is occupied).
   Confirm `/usr/bin/node` is >=22, hotel UID is 999 and systemd can bind the fixed
   socket. Verify gateway isolation hides the hotel DB, root-key location, any
   alternative key backups and general IPC in its mount namespace. The supplied
   unit hides `/opt/philotic/data`, `/opt/philotic/etc`, `/run/philotic` and all
   homes. Stop if the live paths differ; do not relax global filesystem modes.
4. Create the observe-only Muninn test-vault credential and distinct issuer
   introspection credential directly in the Philote vault through an approved
   operator path. Never pass raw keys through Ansible, chat, argv, gateway JSON,
   plaintext files or environment variables. Both records require singleton ACLs
   for role `percival-personal-recall`, guest `percival-personal-gateway`.
   Supply only `percival_secret_ref` and `percival_introspection_secret_ref`.
5. Review issuer admission, exact client and actual connector callback URI before
   activating personal recall. Keep private `user_likesjx` memories held. Supply
   public config fixed to `percival_connection_test`, `muninn_recall`, broker socket,
   loopback Muninn unary JSON endpoint and approved issuer/client/subject.
6. Supply the reviewed gateway source directory, exact commit and the four runtime
   files' SHA256 manifest (`percival_source_sha256`), numeric UID,
   installed hotel hash/path and public config as explicit play inputs. First
   review the dedicated play's check/diff output; only operator approval permits
   a real run with `percival_enabled=true`. Preserve public proxy routes.
   Handlers reload systemd, start the socket, restart hotel, then restart gateway.
   Validate installed/running executable hashes, peer UID checks, guest FD absence,
   discovery and synthetic recall before considering any broader memory access.

Explicit legacy rollback (operator-approved, never automatic from this branch):

1. Stop and disable only `percival-personal-mcp.service`. Stop hotel before stopping
   the socket so no activation job can race the rollback. Preserve runtime logs.
2. Remove only `/etc/systemd/system/philotic-hotel.service.d/percival-scoped-vault.conf`.
   Leave all Beacon and other pre-existing/concurrently added drop-ins intact.
   Disable/stop `percival-personal-vault.socket`; remove its owned unit after stop.
3. Verify the immutable snapshot hashes. Restore the prior aiua to its original
   legacy path `/opt/philotic/bin/aiua` with recorded ownership/mode. Compare the
   current base unit with the snapshot before restoring: retain any separately
   authorized changes made since the snapshot; stop for owner review if they
   overlap. Restore the approved base-unit result. If migrating created `/opt/philotic/current`, undo
   that new link only after verifying it was absent before rollout. If it existed
   before, restore its recorded target. Do not prune retained release directories.
4. Reload systemd and restart `philotic-hotel.service`; verify running `/proc/PID/exe`
   and binary SHA against the recorded baseline and verify Beacon operation.
   Confirm no scoped activation environment/socket dependency remains.
5. Leave encrypted vault records, hotel DB, Muninn binary/data and gateway account
   untouched. Revoke the two new grants separately through their authorities if
   approved. A prior gateway configuration may be restored only from the recorded
   baseline, without exposing keys. Do not execute broad `deploy_hotel.yml` as a
   rollback shortcut: it can seed DB state, update config and restart Muninn.

New source candidate differs from `55f0136c`: cloud acceptance of the old candidate
does not validate this reconciliation. Native x86_64 acceptance and publication
approval are still required for the exact reconciled commit.
