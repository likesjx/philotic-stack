# Scoped broker helper

Build and test with Cargo using --offline --locked after fetching the pinned dependencies. A native macOS build is not a Linux deployment artifact.

The helper uses vendored vault encryption and GraphDomain ACL APIs. Fixture labels and comments are generic. SqliteGraphStorage::open_existing opens an existing database without schema migration.

Create requires --apply-scoped-synthetic --root-private-start. The private adapter execs as root; the shared Rust bootstrap requires Linux fs.suid_dumpable=0, disables dumps and privilege gain, drops groups and all UID/GID identities to the runtime account, verifies zero capabilities, and never execs again before reading the private tmpfs key. Delete remains key-free. Real-key use requires a hash-bound passing Linux adversarial acceptance receipt and separate action authorization.

Only the two scoped kinds with singleton role/guest ACL are accepted in the fixed existing database. Input is private stdin JSON; create outputs only the generated reference and errors are fixed messages. Unknown outcomes require reconciliation, never an automatic create retry. No fallback key is generated.
