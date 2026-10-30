# Packaged response-boundary acceptance

The legacy LifeGraph gateway has optional acceptance instrumentation immediately
following its normal permit validation and before HTTP response emission. This is
a new reviewed gateway package, not evidence retroactively attributed to older
unchanged packages. Native Rust memory does not use this hook.

Production defaults remain synchronous and unchanged. Startup must explicitly
supply both `PHILOTIC_ACCEPTANCE_HANDOFF=response-boundary-v1` and an
`acceptanceHandoff` object containing exactly `mode: response-boundary-v1` and a
canonical absolute Unix `socket` path. Only LifeGraph accepts the hook. The
production configuration compiler does not emit these settings. The endpoint
must be owned by the process UID, mode 0600, in its canonical mode-0700 directory.
Identity, permissions and canonical paths are rechecked after resume.

Each pause has a fixed one-second monotonic deadline and a fresh 256-bit nonce.
Only one matching resume frame terminated by EOF succeeds. Missing, malformed,
duplicate, stale, late or disconnected responses destroy the HTTP response.
Cancellation and response disconnect also fail closed. After resume the gateway
synchronously revalidates storage and identity authority before emitting bytes.
The existing finish/close handlers retain sole responsibility for lease release;
the hook never acquires, replaces, extends or releases those leases.

An observation-only notification records the nonce and synchronously sampled
HTTP `finish` monotonic timestamp. The harness compares it to the first actual
Mongo mutation timestamp on the same Linux clock. Notification arrival order is
not proof. No token, result, memory or credential is sent to the controller.
HTTP finish means handoff to the local transport, not acknowledgement by a remote
human or client.

Qualification must run the actual installed package under its existing supervisor,
retain the real SQLite competing-write and issuer owner-fence assertions, and
verify all twelve cases through actual rollback. The instrumented handoff case
must be disclosed separately; the other eleven cases keep the hook disabled.
A missing notification or unsuccessful assertion fails the gate. Historical
blocked evidence remains blocked and is not overwritten.
