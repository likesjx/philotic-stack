# Local security remediation — production remains unchanged

Historical source: `25cb3d051aca502d6e88c3860e479e927312a0a7`.
Branch: `codex/muninn-security-remediation`. This is source-only work using
synthetic data. No publication, production access, secret handling, admission
query, installation, service action, or cutover replay is part of this phase.

## Confirmed defects and corrections

Two new regressions failed before correction: IPC Debug printed a synthetic
credential canary, and dropping a GetSecret future left its connection reusable.
No production disclosure was observed or investigated.

- IpcRequest and IpcResponse now format only enum discriminants, never fields.
  This protects stale/unexpected-response, timeout and enclosing Debug sites
  centrally. Wire serde is unchanged. Explicit serialization remains sensitive;
  this is not a claim that every unrelated logger is safe.
- GetSecret has an end-to-end drop guard covering writes and reads. Timeout,
  external cancellation, stale reply debt, malformed reply or reference mismatch
  closes that transport before audit/other reuse. The legacy no-deadline method
  now uses a five-second deadline for GetSecret only. Non-secret operations keep
  their existing semantics. A fresh registration is not attempted automatically.
- Ordinary egress no longer serves verifier requests. Supplying its old optional
  verifier profile causes startup refusal before any registration. Unsupported
  ordinary tasks are not logged as raw JSON.
- Explicit verifier-only mode is also **disabled before IPC or socket creation**.
  Its proposed isolation fields are declarations, not proof. No working dedicated
  authority transport has been implemented or claimed.
- Optional OAuth HTTP processing has non-queued global admission before JSON
  extraction: eight concurrent requests and eight admissions per fixed second,
  8192-byte body limit, fifteen-second whole-handler deadline. Dispatch has a
  separate four-concurrent/four-per-fixed-second gate before introspection.
  Fixed windows permit a burst at their boundary. Overload fails closed; a
  sustained attacker may still deny OAuth availability. Edge connection/header
  controls remain a separate deployment obligation. Default MCP/health routes
  do not share these gates.
- Cancelled dispatch removes pending replies and stream buffers even while
  enqueueing/waiting. Late stream tokens cannot recreate orphan buffers; live
  stream accumulation is capped at one MiB per turn. Cancellation removes local
  bookkeeping, not a promise to cancel an already dispatched upstream operation.

## Concrete authority incompatibility — not a routine configuration choice

In reviewed source `25cb3d0`, the hotel Unix socket accepts only the hotel UID.
Register automatically subscribes a guest, and RecordIntegrationAudit requires
the egress-http-runner role. GetSecret authorizes the registered role/guest.
A separate Unix account therefore cannot simply use the existing socket.
Another registration under the shared guest would risk stealing its inbox.
Claiming that role or widening socket permissions is not an accepted fix.

Documented B7 source `5a08d74d944ece80940513a197c5ffa9f6f5c255`, in
`crates/aiua/src/service/ipc.rs`, also automatically subscribes Register and
restricts integration audit to the egress role. Its socket implementation differs
from `25cb3d0`; do not project the newer same-UID implementation onto the live B7
binary. Source correspondence is not installed-binary equivalence. No installed
binary extraction, execution or new runtime probe was performed.

Recommended next design: an authenticated, fixed-action verifier capability,
reviewed as a narrow change against the documented B7-compatible source. It must
bind the kernel caller to an approved dedicated ingress account, accept only a
bounded token for the fixed issuer/client/resource, retain the issuer credential
inside its authority owner, return only typed claims/refusals, and acknowledge
content-free auditing. It must not expose generic GetSecret, arbitrary HTTP,
audit append, or an inbox subscription to that caller. It needs bounded load,
revocation/config checks, interruption and B7 compatibility tests before any
deployment proposal. No new account, ACL, capability or hotel change exists here.

Alternative: explicitly trust every process under the current shared UID and
retain shared runner availability coupling. This does not satisfy the requested
least-privilege boundary and is not recommended. A dedicated service with its own
credential delivery is another materially different authority design, not an
implementation shortcut or a route around the rejected protected inspection.

## Tests and limits

Final combined local run: **128 passed, zero failed, zero skipped**: SDK 62,
egress 19, ingress 44, MCP client 3. Normal offline locked builds of all three
guests passed. Existing membrane-mcp dead-code warnings remain. The two initial
regression failures were reproduced before the fixes; a subsequent health
fixture failure was fixed by supplying its required connection metadata.
The filesystem-socket tests required the supported local sandbox approval;
there was no production connection or external network in those runs.

Regressions cover actual synthetic WARN capture of a stale SecretData frame;
secret cancellation/deadline/reference mismatch; malformed/oversized/private
frames, peer UID refusal and stale socket refusal; audit refusal; independent
transport continuity; slow bodies, admission flooding, unrelated health;
HTTP/client cancellation with an unresponsive dispatched upstream; orphan and
oversized stream buffers; existing identity, expiry, revocation, configuration,
budget and token non-persistence checks. Independent review required the new
turn cleanup; that finding is addressed, not waived.

Local macOS tests/builds cannot establish Linux service/account isolation,
installed B7 compatibility, effective proxy policy, issuer/admission state,
upstream key association, live authentication, or Claude recall. The prepared
Linux workflow runs four crate suites and ordinary guest builds using synthetic
state. It retains only a JSON receipt for one day, no deployable executables.
It has not been published or dispatched.

## Administrator-cookie mitigation — design only

The runtime OAuth path must never require an administrator login. The separate
upstream association check remains rejected and unexecuted. No wording change,
new account or alternate executor is proposed to repeat it. If a supported
least-privilege native key metadata interface is later designed, qualify it as
a separate change; do not assume one exists. Until then, privileged inspection
has the previously disclosed twenty-four-hour copied-cookie risk. Logout and
client cleanup do not revoke a copied cookie. Server-side session revocation
would be a separate native-auth change, not performed in this remediation.

## Remaining production decisions

First choose and review the fixed-action authority contract; do not deploy this
disabled candidate as a functioning connection. Then qualify exact source on
Linux. Any eventual production package must separately bind approved callers,
credential recipients, installed source compatibility, artifact pins and full
guest/service rollback. Current issuer/client/admission state and upstream native
key association remain unresolved; the prior Mongo and protected-inspection
denials remain in force. OAuth activation and harmless Claude recall/denial/
revocation acceptance are still outstanding. Existing recovery journals and
working services stay untouched.
