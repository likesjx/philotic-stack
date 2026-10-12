# Linux full hotel acceptance — 2026-10-08

## Authorized environment and installation

Existing isolated container percival-linux-rust-acceptance, Ubuntu24.04.2 aarch64,
Rust1.94.0. User approved pkg-config and OpenSSL development libraries here only.
Installed official Ubuntu packages: pkg-config/pkgconf1.8.1-2build1,
libssl-dev/openssl3.0.13-0ubuntu3.16 and required package dependencies. Build used
cached public Cargo.lock archives/index offline; container network maps are {}.
A2GB linker kill was corrected by raising only this test container to4GB. No Mac,
production package, credential, service, grant or permission change occurred.

## Exact build and evidence

Full cargo build -p aiua --offline passed (existing warnings).
Linux aarch64 binary: /hotel-target/debug/aiua within test container.
SHA256: a5e2a9128476bbb9bae2fd2478f6c97e244cc477300d4678bc4889ecc4b0beb3.

Actual hotel acceptance passed:
- Root synthetic supervisor prebinds gateway UID1234 mode0600 socket and passes
  a named descriptor through exec into actual hotel UID999.
- Real ephemeral SQLite stores an AES-GCM synthetic credential with singleton
  role/guest ACLs. The actual hotel resolver returns it only to admitted UID1234.
  Public fixed fixture key/data only; no production values/files were inspected.
- Actual GuestManager materializes a Python guest that reports no inherited scoped
  socket descriptor. The probe records only UID and a boolean, no environment data.
- Actual SIGTERM yields successful process shutdown; a fresh hotel reuses the
  retained supervisor FD and restores credential acceptance.
- scoped-vault-bootstrap startup-test completes and shuts down. This CLI mode
  validates configuration/teardown only; concurrent admitted-UID/guest probes
  separately prove delivery/isolation in the synthetic acceptance.
- Invalid PID, extra alias, disabled/unset/hidden descriptors, incompatible smoke
  mode and root/wrong process UIDs reject before guest materialization.

Five targeted actual-aiua Linux lifecycle entries passed:
real shared SQLite lock blocks production set_hotel_pid; native signal handling
forces stop without async polling; injected endpoint panic through the production
supervision helper terminates its subprocess. Short test limits avoid35s waits.
Seventeen current Linux listener tests passed as UID1234. Native harness17 and
strict Clippy passed; current macOS full aiua type-check passed. Prior75 Node
results remain applicable to unchanged gateway source.

## Source corrections found while climbing the test ladder

Native SIGTERM/SIGINT observer installs before runtime/bootstrap: atomic signal
flag plus independent thread enforces intended35s stop despite blocked async work,
allowing watcher polling and OS scheduling latency. Startup-test cleanup has2s
independent deadline. Main also validates actual hotel UID, listener pathname/
owner/mode before database/guest bootstrap. IPC-only smoke mode fails closed when
scoped activation is enabled. Endpoint supervision is shared with panic regression.
Actual bootstrap readiness is logged after shutdown signal handlers are installed;
the acceptance waits for that rather than racing a new process's startup.

## Logs

/tmp/percival-approved-linux-deps.log
/tmp/percival-full-linux-hotel-build.log
/tmp/percival-full-linux-hotel-bootstrap.log
/tmp/percival-full-linux-stop-tests.log
/tmp/percival-final-linux-harness-tests.log
/tmp/percival-final-native-hotel-check.log

## Remaining onboarding/deployment gates

This is source/test acceptance, not installed production readiness. Production
supervisor descriptor delivery, installed binary/vault/Muninn, exact OAuth callback,
operator subject admission, vault-backed issuer introspection credentials and
privacy approval remain. Claude product/callback and separate revocable client/key
policy remain unknown. No verified Claude session messaging channel; no handoff
sent. user_likesjx/private data remain held. Future integration keys must be stored
encrypted in the Philote vault. No live key was created and nothing was pushed.
