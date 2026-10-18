# Shared memory release integration

Gateway adapter `createMemoryReleaseAdapter` exposes the agreed
`withMemoryRelease({token,binding,policyRevision},prepare,handoff)` shape plus an
optional cancellation signal. It uses the existing native transport's pinRelease,
validate and acknowledged release operations. It never uses graph reads. The
owner-supplied transport must authenticate the enrolled gateway, serialize issuer
mutations, and preserve uncertainty on channel loss. Native fence-only profile
qualification remains a separate required test.

Binding and callbacks are trusted internal integration inputs, never HTTP caller
arguments. `currentPolicyRevision(binding)` must verify the entire binding against
the canonical trusted mapping and return its revision only on an exact match.
`inspect` is confidential canonical introspection, including admission_revision.
Token digest stays in memory and is not logged. The4.5-second gateway bound is
below the native5-second lease deadline. Abort or unknown completion abandons the
channel; only successful local finish permits acknowledged end_release.

`prepare` must validate records and provenance before deriving the exact JSON-RPC
response, including ID. Returned serialized bytes are immutable, size-bounded and
hashed. `handoffMemoryResponse` installs listeners before writing, verifies exact
length/hash, sends once, and completes only at local finish. This does not claim
physical receipt or retraction. `serveFencedMemory` combines the durable lifecycle
abort signal and withdrawal gate with the shared release interval. Normal finish
releases lifecycle tracking without aborting the subsequent native acknowledgment;
early close still aborts. Production HTTP route/record-owner wiring remains absent.

The8 public fixture tests exercise the gateway writer with an EventEmitter
transport and simulated native owner. The record rejection fixture models a
trusted callback; it is NOT an implemented production record authority. Real HTTP
writer plus native owner race/worker-loss/dirty-restart qualification is still
required before production. No production route is enabled by this module.

## Concrete backend gap

Inspected Muninn source fc16e786970c7ef31a6e5244cfdca7980a341551 Engram has ID,
CreatedBy, UpdatedAt, Tags, Classification (concept cluster), Trust (provenance
confidence), summaries and associations. These are not a demonstrated canonical
owner-subject, explicit nonprivate disclosure decision, complete provenance or
release-stable content revision. UpdatedAt alone does not prevent concurrent
changes. Do not map those fields heuristically into authorization.

Minimum candidate path requiring owner/user approval: one immutable curated
collection snapshot, with explicit approved record IDs, content digests/revisions,
owner-subject, nonprivate label and complete source dependencies. The record owner
must create/attest it; gateway cannot infer or self-grant metadata. Snapshot bytes
and metadata stay immutable while selected; replacing the mapping/snapshot must
coordinate with the same owner or drain serving first. Existing mutable live
collection requires every content/metadata/policy writer to participate in that
exclusion. Choosing immutable approved scope versus coordinating live writers is
a concrete remaining architecture/data-scope decision, not a gateway test fix.
