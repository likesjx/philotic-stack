# Source review fixes atop e3af423e — 2026-10-08

Worktree recovered clean after the Mac restart; no interrupted edits existed.

## Shared SQLite shutdown lock

New service/scoped_vault_shutdown.rs arms an independent native thread before
normal drain (35 seconds) and startup-test cleanup (2 seconds), only when the
scoped endpoint is enabled. It forces process exit on expiry even if synchronous
GraphDomain.set_hotel_pid blocks before hotel_main can return. Existing bounded
runtime teardown remains; disabled startup retains prior shutdown behavior.

Actual aiua regression holds SqliteGraphStorage.raw_conn mutex on a separate
thread, calls the real GraphDomain.set_hotel_pid cleanup, and asserts forced
subprocess exit under a shortened deadline. This tests the real shared storage
lock, not an unrelated parked worker. Targeted tests passed; full Mac aiua built.

## Scoped descriptor inheritance

Disabled/unset mode rejects all LISTEN_* metadata and kernel-identified scoped
socket FDs (including unadvertised FDs) before guest subprocess creation. This
fails closed if /proc/self/fd cannot be inspected, and deliberately makes unrelated
socket activation incompatible with disabled mode. Enabled adoption rejects any
additional inherited scoped alias before duplicating the selected descriptor.
The original and adopted duplicate both use FD_CLOEXEC.

Actual optional() Linux subprocess/exec tests passed for enabled clean adoption,
extra unadvertised alias rejection, disabled/unset advertised/wrong/missing
metadata rejection, and clean disabled exec. The child probe detects any scoped
FD through the actual disabled startup check. Actual Linux listener acceptance
also passed again. Independent source review found no remaining blocker in these
fixes. Seventeen harness tests and strict Clippy all-targets passed.

Logs (local temporary artifacts):
- /tmp/percival-storage-lock-stop-tests.log
- /tmp/percival-p2-linux-exec-acceptance.log
- /tmp/percival-p2-linux-listener-acceptance.log
- /tmp/percival-p2-hotel-build.log
- /tmp/percival-p2-clippy.log

No package installation, push, deployment, live service restart, credential/grant
or private production recall occurred. Only the existing isolated test container
was restarted after the Mac restart. Full Linux aiua still awaits approval for
pkg-config/OpenSSL development dependencies. Actual hotel bootstrap lifecycle,
supervisor descriptor delivery and installed encrypted-vault acceptance remain
onboarding gates; callback, Claude cloud product, separate client/key policies and
vault-backed introspection credential delivery are unresolved. user_likesjx held.

## Followthrough after approved Linux dependency installation

The Linux full build and actual-hotel synthetic bootstrap gates now pass; see
LINUX_FULL_HOTEL_ACCEPTANCE.md. Native stop signal observation now begins before
runtime/bootstrap using an atomic signal flag and independent thread, so even
blocked async polling cannot delay the intended deadline. A real SIGTERM while
the real SQLite mutex blocks cleanup exits under the shortened regression bound.
Early process UID/socket validation precedes database/guest bootstrap.
