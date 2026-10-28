# Existing ingress and issuer: local recall candidate

**Historical candidate: deployment held.** Source `25cb3d0` passed its original
synthetic suite but has unresolved secret logging, cancellation and shared
authority/availability findings. See [SECURITY-REMEDIATION.md](SECURITY-REMEDIATION.md).
The sections below describe the historical design, not deployment approval.

This reconstructs the proposed integration on MBP from source
`13c90b9f07af1bf74ea2de8a0d2c2603f24538f1`. It is not a transfer of mac-jane's
unpublished candidate. No production operation, credential read, publication,
grant, activation or cutover replay occurred.

The local Intel-Graph REST service was consulted: last scan
2026-10-27T20:54:07.530107Z, project documents `doc:mcp-membrane-gateway` and
`doc:muninn-memory-protocol`. Project guidance favors configured membrane
routing and separate endpoints for different authority levels. No personal
memory content or mac-jane source was accessed.

## Implemented path

Existing membrane-mcp listener -> optional `/personal/mcp` recall resource ->
private local Unix socket -> actual existing egress-http-runner identity ->
unchanged hotel GetSecret + bounded HTTPS issuer introspection -> acknowledged
content-free integration audit. Only structured claims return to ingress.
The existing `/mcp` bearer path is unchanged. With no new profile configured,
the optional recall and protected-resource metadata routes return 404.

After verification, ingress maps only the explicitly named existing token
grant into the existing MCP upstream dispatch. It preserves that grant's
principal and budget, permits only `muninn_recall`, rejects overrides/writes,
forces `default` vault, and bounds query/limit. It pins issuer, audience,
Claude client, subject, scope and 15-minute token lifetime. Before releasing
results it rechecks revocation, expiry, grant expiry, endpoint and perimeter.

No shared IPC type, hotel implementation, native key or scoped B7 broker rule
was changed. The aiua change is only a harmless two-guest test. The candidate
uses generic existing GetSecret; this does not establish that the live egress
guest is authorized for the selected issuer credential.

## Why a private call was necessary

The ordinary RunnerHttpExecutor puts HTTP bodies inside session-bearing
EmitTask payloads, which the hotel records. Same-hotel ledger entries are
skipped, but session recording still persists the token. A sessionless variant
loses integration auditing and has no server-enforced local-only delivery:
the hotel may reroute an unavailable guest remotely. It was not accepted.

In-process execution in ingress would require ingress secret-recipient and
audit authority changes. The selected private call retains the real egress
identity and audit authority, without a hotel upgrade or role impersonation.

The socket requires a private 0700 directory and 0600 socket, trusted ancestors,
and matching peer/server UID. This explicitly trusts other processes under
that deployment UID; it is not executable/process attestation. It has bounded
4096-byte frames, one request per connection and a ten-second deadline. It does
not replace/unlink existing socket files. A stale socket refuses startup and
needs an explicitly reconciled cleanup or a fresh reviewed path.

Profiles are optional, non-secret, bounded descriptor reads with no-follow,
owner/ancestor and stable identity checks. The egress profile pins the exact
binding revision, fixed HTTPS issuer path, actual local execution node, agent
grant, limits and Basic credential injection. It rejects approval-required
bindings: automatic use requires an explicitly approved binding with
`requires_approval=false`. It checks binding stability after HTTP execution.

Any private authority failure immediately discards the hotel connection;
otherwise a late SecretData could reach the SDK's stale-response logging.
Because this is the existing runner's connection, that failure can terminate
the runner and interrupt other HTTP integrations it serves. This is an
intentional fail-closed behavior, not isolated-availability qualification.
Production approval must account for that shared guest impact and stale-socket
recovery. A second connection claiming the same guest is not substituted:
Register automatically subscribes it, which could steal existing inbox work.
Injected credential headers are marked sensitive. Successful parsed
introspection, including inactive claims, requires durable audit acceptance.
Pre-authority/HTTP/parse failures return fixed refusals; this is not an
all-attempt durable integration audit. No memory-zeroization claim is made.

## Local qualification

Normal offline builds of membrane-mcp, membrane-mcp-client and
egress-http-runner passed. Relevant suites passed 59 tests: 40 ingress,
16 egress and 3 MCP client. The separate actual LocalProcessMaterializer
two-sleep-guest fixture passed: absolute command despite an invalid shared
binary directory, targeted reclaim and the other guest remaining alive.
Count that fixture once, despite its isolated child invocation.

Coverage includes real private sockets and peer refusal, framing and deadlines,
real SDK/fake hotel frames excluding the token, mandatory audit acknowledgement,
late-secret timeout log canary, profile source/FIFO/symlink/mode rejection,
HTTP form/Basic wire contract, token-free audit projection, actual ingress
correlation and upstream dispatch, existing budgets, revocation/config/fence
changes, time-only grant expiry and missing-endpoint legacy fallback refusal.
Independent review found and closed profile-source, late-secret logging,
approval-gate and endpoint/expiry findings. Existing compiler warnings remain.

These are synthetic fixtures, not a real website Mongo/admission test or live
B7 test. Documented B7 source `5a08d74d944ece80940513a197c5ffa9f6f5c255`
contains the existing Register/GetSecret/GetIntegrationBindings/
RecordIntegrationAudit requests and binding-entry metadata used here; that is
source correspondence, not installed-binary equivalence proof. Do not extract
or execute the previously blocked B7 executable investigation.

The included workflow `Isolated governed Muninn ingress` runs only on the
dedicated branch, in likesjx/philotic-stack. It builds the three guests, runs
synthetic contracts and the harmless materializer fixture under a 20-minute
job limit. It builds the two changed guest executables from this source and
retains them, SOURCE/SHA256SUMS and an exact-source JSON receipt for one day.
No installed-server binary, real credentials, SSH, deployment or production
database is involved.
It has not been published or run.

## Minimum remaining composition inputs and owner decision

First qualify this exact source on Linux. Do not interpret that as activation.
Then bind the following finite inputs, using non-secret project metadata where
authorized, with no retry of the denied Mongo admission read:

1. Current ingress and egress guest records, complete commands/configuration,
   Unix UID and live source/artifact identities. Canonical membrane-mcp serves
   three guests: do not overwrite it globally or replace a full guest record
   with an incomplete RegisterComponent payload.
2. The exact existing issuer credential reference/verifier association and
   whether the genuine egress role/guest is already an allowed recipient.
   Also the exact approved local automatic integration binding and revision.
   If absent/denied, choose an explicit narrow authority adoption or stop;
   never silently widen ACLs, impersonate a role, rotate or create a credential.
3. Existing endpoint ID, Claude grant ID/budget and local Muninn upstream ID.
   Its separate upstream credential validity/default-vault association remains
   unproven; no new native key is included in this candidate.
4. Website issuer remains the existing jaredlikes.com OAuth implementation.
   Desired client is `claude_muninn_recall_v1`, callback
   `https://claude.ai/api/mcp/auth_callback`, resource
   `https://mcp.jaredlikes.com/personal/mcp`, scope `memory:recall`.
   Enabled admissions/version and promotion persistence are not verified here.

The production proposal should use versioned absolute executables for only
the intended ingress guest and the existing egress guest, with optional
profile configuration, one private socket directory, and existing ingress
route/metadata forwarding. Stage proposed release paths only after bindings
are known; no live UID, reference, guest-record digest or install path is
invented in this document. Restart only those approved guests, not the hotel.
Preserve their complete prior records and paths for rollback; on failed
acceptance restore the two records and keep OAuth disabled. Do not touch
unrelated membrane guests, web/ONNX/Memgraph or old uncertain cutover journals.

Separate exact authorization must cover any required credential-recipient or
automatic binding adoption, issuer admission/enablement, route and guest
changes. No such action is executed by local tests or publication. Final
acceptance remains Claude consent, expected recall-only tools, one expressly
authorized harmless fixture recall, write/unauthorized denial and revocation
with results withheld. Personal memories are not this fixture.
