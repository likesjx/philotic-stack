# LifeGraph editor and relationships

Status: implemented on `codex/lifegraph-editor`; production rollout pending.
Parent: [Native Apple program](../../docs/architecture/NATIVE_APPLE_APP_PROPOSAL.md).
Work: [LifeGraph editor](../../docs/task.md#native-lifegraph-editor).

## Operator decision

Save directly changes canonical node text, with an audit record; it does not
submit an agent-review proposal. This first bounded editor supports `title`,
`claim_summary`, and `description`. IDs, labels, trust, provenance, arbitrary
typed properties, deletion, and edge mutation are not editable here.

## Contract

`PATCH /api/edge/lifegraph/node/:id` accepts only `before` and `changes` maps.
Every changed field needs its original string or explicit null. The gateway
requires a currently enrolled per-device bearer and derives `actor=edge:<id>`
from the registry; shared bearer tokens and client-supplied actor fields are
rejected. This is the existing single-operator enrolled-device trust boundary,
not a new human identity proof or a multi-user permission system. Do not grant
this capability to untrusted sensor-only devices. Fine-grained device grants
remain a prerequisite before widening enrollment beyond operator devices.

The runner's dispatch-only `life.node.edit` is not in the model tool catalog.
It permits canonical `life:` IDs and three text fields, bounds values to 16 KiB,
and does not accept raw Cypher. An existing unique node must match every supplied
original value. Missing, ambiguous, or stale targets return conflict, without
changing the node or creating an audit entry.

One Cypher statement both changes the fields and creates `LifeNodeEdit` with
audit ID, node ID, authenticated device actor, server UTC timestamp, and JSON
before/after maps. No identity, validation state, confidence, or original source
provenance is rewritten. Audit content is sensitive LifeGraph data, stored only
in its graph, never emitted in application logs. No audit deletion endpoint or
automatic retention policy is introduced. The audit is append-only through this
surface, not cryptographically tamper-proof against graph administrators.

The app keeps failed drafts, refuses a hotel switch underneath an edit, and
requires a matching node ID and nonempty audit ID in the save receipt. It reloads
canonical state after acknowledgment. A failed refresh is shown alongside the
receipt; a lost network response is uncertain, not a successful save. Reload
before retrying an uncertain write. Saving does not refresh semantic embeddings
in this first version; embedding refresh and audit-history/undo UI are follow-ups.

## Relationships

The existing bounded node-detail response supplies typed neighbors and original
edge endpoints. A one-hop diagram shows up to eight returned relationships;
arrowheads follow the stored direction, labels show relationship type, and
neighbor buttons open their detail. The relationship list remains available
below, including incoming/outgoing labels. Missing endpoint direction is never
invented. The view states its returned-edge limit; it is not a whole-graph map.

## Verification and rollout

- Shared library: 100 tests, one existing skip, no failures.
- Mac app build and test pass; iPhone Simulator build passes (unsigned validation builds; installed apps unchanged).
- LifeGraph library: 213 tests pass; gateway edge and edit-authority tests pass.
- Gateway and runner compile. Isolated Memgraph test passes for real Cypher save,
  stale-write rejection, null originals, unchanged confirmation state, audit
  count, and persisted actor/timestamp/before/after content. Runner unit suite:
  55 passed; edge suite: 23 passed; edit authorization: two passed.
- Baseline build repair: removed duplicate `deserialize_properties_leniently`
  definition while retaining the newer implementation and compatible error wording.
- Still required: install updated `philotic-web` and `life-graph-runner` together,
  integrate the Apple changes without losing the existing notch/desktop work,
  and verify a disposable node from the installed app through the selected hotel.
  No real operator nodes have been edited for validation.

Reproduce database validation with a disposable local Memgraph at port 17687:
`cargo test -p data-memorygraphrag --test node_edit_memgraph -- --ignored`.
The test only touches uniquely named synthetic nodes.
