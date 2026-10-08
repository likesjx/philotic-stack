# Readiness delta — 2026-10-08, source/test only

Accepted baseline ad6de2b078e205754ab04b1ab7cd457bc318f89e is preserved in git.
These changes are additional local source work, not production deployment.
No push, service restart, live credentials/grants, production memory writes or
private recall occurred. Existing dirty identity/Beacon checkouts were preserved.

| Gap at inspection | Implemented locally | Remaining gate |
|---|---|---|
| Gateway could read world-readable hotel DB | deploy/ service template: dedicated user, namespace-hidden hotel data/config/general IPC, no capabilities, protected home | Select free UID/GID; verify actual root-key/backup/alternative paths; run actual systemd namespace denials. Template is not enforced isolation |
| Issuer permitted life:recall | Isolated identity patch enforces memory only for authorization and persisted code/refresh/access grants | Review/apply/publish patch; actual issuer image/config acceptance |
| Gateway read raw introspection env secret | Root-policy second pinned ref and fixed operation, strict UID/ACL; gateway fetches per call, rejects env/literal issuer secret; issuer receives verifier only | Review/integrate identity patch; separately approve random in-memory provisioning/encrypted vault writes and nonsecret verifier config |
| Architecture mismatch | Rebuilt/tested final aarch64 hotel; checked existing targets/tools | NO x86_64 artifact. New isolated toolchain/container approval requested; current install approval does not cover it |
| Installed Muninn compatibility unknown | Existing local v0.11.0 tag built offline; synthetic handler and real Pebble engine compatibility tests pass | Installed artifact/source equivalence and approved installed synthetic acceptance remain unverified; no evidence requires an upgrade |

## Credential contract

Policy retains secret_ref for Muninn and adds optional introspection_secret_ref
under secret://hotel/default/percival-issuer-introspection/<id>. Requests remain
fixed get_percival_credential or get_percival_introspection_credential; no caller
reference/role/guest. Both are available only to the pinned gateway UID and exact
singleton percival-personal-recall / percival-personal-gateway ACL. Four-worker
bounds, cancellation, same-record ACL/decryption and shutdown remain unchanged.
Missing second ref denies. Introspection value43–128 base64url characters from
>=32 random bytes, explicitly excluding mk_ to prevent swapped-key acceptance.

Both raw keys belong encrypted in Philote. The second key is NOT generated yet.
Identity introspectionVerifier derives SHA256 of the complete expected Basic
header in the same in-memory provisioning flow; issuer receives only
PERSONAL_OAUTH_INTROSPECTION_VERIFIER and pinned client ID. It independently
checks supplied username and compares verifier in constant time. A verifier is
not a usable Basic secret. Rotate encrypted value/verifier together. Never put
raw key in env/files/chat/argv/logs. Approved provisioning remains outstanding.

## Evidence

- Final Node gateway78/78 tests passed; fresh lookup, sanitized socket failure,
  cancellation and swapped long mk_ key denial included.
- Isolated identity32/32 policy/verifier tests and full TypeScript typecheck passed.
  Memory-only metadata/new grants and old broader persisted-family rejection
  included. Mongo end-to-end suite was adapted but NOT RUN: no disposable Mongo
  instance available, no install authorized.
- Native Rust18/18 harness tests and strict all-target Clippy passed.
- Final Linux ARM harness18/18 passed as synthetic UID1234; final full hotel
  rebuild/bootstrap passed against two real AES-GCM
  encrypted SQLite credentials, actual gateway UID, actual guest FD isolation,
  SIGTERM/recovery and startup rejection cases. Supervisor is synthetic, not
  production systemd. The narrow validators are also covered by native tests.
- Final ARM full hotel SHA256:
  2b1e470e863719aa43412bd0765751f401d3afa99766b920001fdbea48c15e74.
  Production x86_64 cannot run this artifact. Full native macOS hotel typecheck
  passed before the final validator-only change; final Linux hotel rebuild passed.
- Muninn local tag v0.11.0 resolves
  b30b9957b4621ea973e8af5ef87c30e507f4f7aa. Existing cached Go1.26.5 built it
  offline on darwin/arm64, without installation. Seven targeted tests passed:
  actual unary /mcp handler (synthetic key/spy engine), observe write denial,
  context propagation; separate real temporary Pebble engine tests verify
  suppression of Hebbian writes and recall events. This is source compatibility,
  not production daemon/binary equivalence or a cloud connector E2E.

Logs: /tmp/percival-readiness-node-tests.log,
/tmp/percival-readiness-identity-tests.log,
/tmp/percival-readiness-identity-typecheck.log,
/tmp/percival-readiness-rust-tests.log,
/tmp/percival-readiness-clippy.log,
/tmp/percival-readiness-linux-harness-tests.log,
/tmp/percival-readiness-full-linux-bootstrap.log,
/tmp/percival-v011-compat-tests.log.

## Identity deliverable and integration

Isolated source copy: /Users/jaredlikes/Documents/Codex/2026-10-05/task/identity-readiness.
identity-readiness.patch applies to the original checkout's existing baseline,
including its pre-existing dirty refresh-token regression; that regression is
preserved. Patch modifies source/tests/docs, not credentials or admissions.
Original checkout HEAD84d7870ce619b8f1fb876da899f9e8684fdb582f remains untouched.
Patch SHA256 c2280d778355148034a27b1ed77fc384db4396f072a215df4658b7f14ceecdf3;
git apply --check passed against that original dirty baseline.
Apply/review in a separate identity worktree before publishing. The patch is a
reviewable deliverable, not a claim that identity production has changed.

## Exact blockers

Deployment: approved x86_64 build/test artifact; reconciled source publications;
free UID/GID; actual systemd namespace/FD validation and precise unit/config diff.
UID991 is occupied. Upstream systemd documents Sockets= association but installed
behavior remains an acceptance gate. Rollback removes the additive scoped drop-in
rather than assigning Sockets= empty, which does not clear it. Preserve Beacon.

Connector activation additionally needs exact callback from actual UI, verified
provider/admin/operator subject, explicit synthetic-only privacy admission,
approved two-key encrypted vault provisioning, issuer verifier config and installed
issuer/Muninn acceptance. Private user_likesjx remains held; Claude still requires
separate reviewed client/key/endpoint design. No broad ACL or root broker.

Next x86 build needs a new disposable linux/amd64 Ubuntu24.04 container/image,
Rust1.94.0 x86_64, GCC/libc development toolchain, pkg-config and OpenSSL development
libraries from official sources; none is installed under the prior ARM-only
package approval. No production package changes are proposed.
