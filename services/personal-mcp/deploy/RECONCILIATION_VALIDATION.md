# Reconciled source validation — 2026-10-08

Base: `78eb2f32303823379d6b8c089000e19bf1f69d62`, last verified remote develop.
Candidate: scoped runtime/gateway files selectively applied from `55f0136c`.
Parent verified the remote via GitHub at 19:31 UTC: identical to this base,
ahead zero/behind zero, closing the local DNS uncertainty at that timestamp.
Refresh again if publication occurs after further develop changes.

Preserved: split IPC modules, Beacon worktree, release/packages.toml, existing
PR Check/Build Linux/release workflows, release Ansible tasks and current version
stamping. Egress inventory replaces the retired IPC path with the two actual
direct callers; the inventory checker itself is unchanged.

Implemented source additions:

- Dedicated disabled-by-default Ansible play/role: fixed two-reference policy,
  separate numeric gateway identity, account/group collision preflight, installed
  effective ExecStart and running `/proc/MainPID/exe` path/identity/SHA256 checks,
  four gateway file SHA256 checks, root-owned runtime/config,
  additive hotel drop-in, ordered socket/hotel/gateway handlers.
- Explicit operator legacy rollback procedure, preserving unrelated drop-ins,
  encrypted state and Muninn. No backup or live rollback was performed.
- Native Ubuntu x86_64 disposable-container CI coverage for the actual listener,
  numeric peers, FD inheritance, bounded stuck jobs, full hotel bootstrap with
  two synthetic encrypted SQLite credentials, startup failure and lifecycle.
  No live keys or host service mounts. CI has not been dispatched or run here.
  Fixture FD handoff and DAC checks do not prove real systemd Sockets/dependencies,
  restart ordering, InaccessiblePaths or rollback; those remain operator checks.

Local checks on this reconciliation:

- Gateway syntax and Node tests: 78 passed, zero failed.
- Rust source harness: 18 passed, zero failed, offline locked build.
- Harness Clippy all targets with `-D warnings`: passed.
- Full `cargo check --offline --locked -p aiua`: passed; eight existing warnings.
- Rust formatting and source whitespace checks: passed, excluding the unchanged
  embedded identity patch's two required blank context lines.
- Ansible syntax check, synthetic policy-template JSON rendering and workflow
  YAML parsing: passed using existing tooling, without credential config files.
- Dedicated play, local connection, enabled=false: zero changed, 24 skipped.
- Eight synthetic provenance tests: approved/versioned path accepted; legacy,
  stale running executable, bad hash, deleted process, reload and ambiguity denied.
- Actual Ansible role gate tested locally using a temporary systemctl stub:
  legacy ExecStart rejected, approved symlink admitted, restart failure stops the
  stub gateway and fails rollout. Only a temporary stop marker changes; no live
  systemd calls. Log: `/tmp/percival-role-provenance-tests.log`.
- Egress inventory: 34 classified callers and three migration guards, passed.

Logs: `/tmp/percival-reconciled-{node-tests,harness-tests,clippy,aiua-check,ansible-disabled}.log`.
No new software installation or large fresh hotel build was required.

Limitations/approvals: native x86_64 CI acceptance of this exact reconciliation
is pending; the previous cloud candidate is older. Refresh/review concurrent
owners before publishing. Publication, workflow dispatch, tags/releases,
production Ansible, account creation, credential creation/vault storage, issuer
admission and service restart remain operator actions requiring authorization.
The new role never creates or transports a key: both credentials must already
exist in Philote with the required singleton ACLs before active rollout.
