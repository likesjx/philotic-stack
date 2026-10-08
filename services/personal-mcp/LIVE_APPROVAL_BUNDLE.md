# Production approval bundle — prerequisites unresolved

UPDATE: READINESS_DELTA.md supersedes implementation blockers below. Local
filesystem templates, memory-only issuer and narrow introspection delivery now
exist with source tests. x86_64 and installed-systemd acceptance still block
deployment; the historical production inspection below remains valid.

Read-only VPS inspection 2026-10-08; no live changes, credential values, admissions or memories accessed. Local candidate ad6de2b078e205754ab04b1ab7cd457bc318f89e is unpublished; old PR616 does not contain it.

## Decision

Muninn already runs in production: health HTTP200, v0.11.0. No upgrade is ready for approval: replacement commit/artifact is unspecified and installed observe/no-learning/unary compatibility remains unverified. Test compatibility first; retain installed Muninn if it passes. A failing result requires a separately specified Muninn upgrade, rollback and shared-memory downtime decision.

Production is x86_64. Accepted full-hotel test binary is aarch64 and CANNOT deploy. Build/test the same candidate for x86_64 and approve its hash. Existing full acceptance is documented in LINUX_FULL_HOTEL_ACCEPTANCE.md.

## Verified installation

- Hotel: philotic-hotel.service, host systemd, philotic UID999/GID987. /opt/philotic/bin/aiua installed SHA256 c58c54b76489c19a70a0f930784f534d71f3cfd368d9dc33f5e7be63a043f003. Installed source revision/running executable hash unverified. TimeoutStopSec30; NoNewPrivileges=yes, ProtectSystem=strict, ProtectHome=yes. Preserve drop-ins beacon-b64, debug-model-requests, heal-escalation, lifegraph-observer, log-dir, memory-maintenance, memory-sleep.
- Muninn: muninn.service, deploy; running executable /home/deploy/.local/bin/muninn, symlink /usr/local/bin/muninn. Installed SHA256 6a862d2fa39d48fe20b849b6ced66f2673822d1677e8bfe52300f8c16dc032d4; installed 2026-08-06. Health v0.11.0 does not establish installed API/auth semantics.
- Node gateway: percival-personal-mcp.service, deploy, /usr/bin/node, working directory /home/deploy/percival-personal-mcp/release-f12b1b46. Current ProtectHome=read-only, ProtectSystem=strict, NoNewPrivileges=yes; no InaccessiblePaths.
- Discovery bridge: Docker percival-personal-discovery-proxy, nginx UID101, host network, read-only root, dropped capabilities; /home/deploy/percival-personal-mcp/nginx.conf read-only mount. Exact personal paths forward 172.18.0.1:8913 to Node 127.0.0.1:8913. Preserve bridge.
- Edge: deploy-traefik-1, traefik:v2.11; personal dynamic route /home/deploy/traefik/dynamic/percival-personal-discovery.yml. Preserve routing.
- Identity: philotic-native-identity-20261001, image philotic-native-identity:mount-20261005, ID sha256:54f983366ae6df9b4e9a7dab517072eec8b0aad6fed4fd1f80da6cb505301d2d, loopback3102. Candidate source/image correlation unverified. Loopback OAuth discovery returned404; public edge behavior not established.
- Personal discovery HTTP200 advertises resource https://mcp.jaredlikes.com/personal/mcp, issuer https://www.jaredlikes.com, memory:recall.

## Proposed ordered actions — require later approval

1. Publish/reconcile candidate preserving Beacon work; integrate identity separately. Identity checkout /Users/jaredlikes/code/philotic-native-identity-percival-oauth HEAD84d7870ce619b8f1fb876da899f9e8684fdb582f has an existing dirty tests/personal-oauth.test.ts regression to preserve/review. Produce approved x86_64 hotel and immutable gateway/identity releases.
2. Select verified-free percival-gateway UID/GID with nologin, no home/supplementary hotel/docker/sudo groups. UID/GID991 is OCCUPIED by systemd-resolve; do not use it. Exact free identity remains an approval input; abort on collision.
3. New gateway mount namespace must hide /opt/philotic/data, /opt/philotic/etc and /run/philotic. Hotel database aiua_context.db is0664 under0755 directory: UID separation alone does not prevent reading it. Do not globally chmod database. Move gateway release outside /home to /opt/percival-personal-mcp/releases/<approved-release>; ProtectHome=yes, ProtectSystem=strict, PrivateTmp=yes, NoNewPrivileges=yes, empty capability bounding set, AF_UNIX/AF_INET/AF_INET6 only. Prove actual service namespace denies DB/general IPC/other credentials.
4. Root-owned nonwritable policy /etc/percival-personal-mcp/scoped-vault-policy.json pins actual caller UID, hotel UID999 and exact new secret reference. Supervisor-prebind /run/percival-personal-vault-broker.sock gateway-owned0600, descriptor name percival-scoped-vault, targeting actual hotel PID. Proposed percival-personal-vault.socket targets philotic-hotel.service; service Sockets= association is documented upstream; actual descriptor delivery requires installed-systemd validation before final unit approval. Synthetic supervisor tests are insufficient proof.
5. Hotel PHILOTIC_SCOPED_VAULT_ENABLED=1; proposed TimeoutStopSec40 accommodates intended35s native watchdog plus teardown/scheduling. Start socket, replace/restart hotel, verify scoped delivery and existing guests, then restart hardened gateway in discovery-only mode. Hotel restart interrupts ALL hosted guests/frontdoors; allow up to40s stop plus bootstrap of unmeasured duration. Muninn/edge/bridge need no restart for this phase.
6. Once activation blockers below are resolved, replace/restart only identity container with approved image/config; verify issuer, then enable/restart gateway and conduct synthetic consent/recall. Identity login/OAuth downtime is possible; duration/parallel cutover not established.

## Exact test grant and encrypted key handling

After explicit live approval, Muninn key body must specify all fields:

```json
{"vault":"percival_connection_test","label":"percival-personal-readonly","mode":"observe","expires":"90d"}
```

Capture one-time returned token only in memory, save encrypted in Philote vault under secret://hotel/default/percival-muninn-observe/<id>, singleton role percival-personal-recall and guest percival-personal-gateway. Record only ID, expiry, ref. No token in logs/chat/argv/plaintext staging/env files/Docker environment. Defaults can mean default vault/full access/no expiry; never rely on them. No key has been created/saved. Do not rerun broad legacy provisioning helpers.

Issuer introspection delivery is now implemented locally: second pinned ref secret://hotel/default/percival-issuer-introspection/<id>, fixed operation, same exact gateway ACL, fresh per-call retrieval; server rejects raw/environment issuer-secret config. Isolated identity source accepts only a hashed full Basic-header verifier. Source integration and approved in-memory provisioning remain gates; both raw credentials must be encrypted in Philote. See READINESS_DELTA.md for test coverage. Do not reuse session/provider/Muninn secrets or expose general hotel IPC.

## Activation gates and admission

Isolated identity candidate now restricts grants to memory:recall and rejects old broader persisted grants during exchange/refresh/introspection. Apply/review its patch before publication; installed enforcement is unverified. Exact connector callback must come from actual UI. Current identity supports one public client/exact callback, PKCES256,15min access,7day absolute refresh families; these are candidate, not verified installed capabilities.

Operator must confirm exact internal subject UUID mapped from verified Google/GitHub provider tuple, current admin role, signed session <=10min; no email linking. Initial trusted personal admission {subject,enabled:true,version:0}; subsequent disable/re-enable via changePersonalAdmission atomic version increments, not direct enabled toggles. No existing user/admission records were inspected.

Privacy admission covers at most three explicitly approved synthetic records in percival_connection_test and their cloud processing. user_likesjx, private/unclassified records and backfill remain HELD; gateway lacks record-level egress filtering. Claude requires separate client/key and reviewed endpoint/policy; single fixed-reference candidate supports Percival only. Claude product/callback/session unknown.

Source/build/isolation preparation does not depend on exact callback. Live identity activation/consent does. None of these phases is authorized by this document.

## Acceptance and rollback

Installed-systemd named FD/actual UID checks, no guest inheritance, wrong UID/ref rejection, scoped failure shutdown and recovery; actual gateway namespace DB/general IPC denial; installed Muninn observe denies mutation and read_only recall avoids learning, all with synthetic data. Then exact client/resource/scope rejection, expiry, logout/admission-version revocation, refresh replay and key revocation. No private recall.

Before deployment record exact prior unit/drop-in/public config diffs and immutable artifact hashes, without exposing secret values. Rollback gateway to discovery-only; revoke only new grants through supported admission-version/key paths; restore prior gateway release, identity image, hotel hash above and prior service settings preserving concurrent Beacon/drop-ins; restart only affected services. Remove service dependency before stopping only new scoped socket. Never reset unrelated branches, delete accounts/vault data or alter private grants. Suspected exposed token must be revoked, not merely hidden by config rollback.

Outstanding approval inputs: x86_64 artifact/source integration; installed Muninn compatibility; free UID/GID/final tested unit diff; identity patch integration; approved two-key/verifier provisioning; exact callback and verified operator/privacy admission.
