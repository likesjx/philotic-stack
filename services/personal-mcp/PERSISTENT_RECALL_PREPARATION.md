# Persistent two-client recall preparation

This is a local, pure policy implementation, not an activated service or permission
to read real memories. The existing gateway's synthetic-only multi-client restriction
remains unchanged. No shared website issuer or LifeGraph source is modified.

`persistent-recall.mjs` adds:

- A preparation manifest requiring two distinct clients, exact nonshared HTTPS
  callbacks, observed public S256/resource/tool compatibility, explicit vault scope
  and receipt references for privacy, read-only behavior, credential delivery,
  rotation, rollback and lifecycle. Missing values remain blockers. Even a complete
  manifest returns activationAllowed=false: references are not executable authority.
- Immutable reference transitions for independent client revocation and family
  revocation. These require a future durable compare-and-swap owner; they do not
  mutate Mongo or implement cross-process concurrency control.
- Credential rotation states that withdraw recall until replacement adoption,
  positive recall and old-credential denial evidence exist. Expiry and privacy-lease
  expiry deny access. No secret values are handled.
- A recall guard requiring a trusted privacy authority and rechecking current
authorization after upstream work. The authority must filter and bind the exact
  response; this module is NOT a record-level privacy implementation.

## Integration contract for the issuer owner

`persistent-owner-boundary.mjs` composes the guard with injected canonical issuer,
scope/privacy and lifecycle owners, never a second admission database. It passes
the opaque OAuth token only to the issuer, forces readOnly upstream, and rechecks
canonical client version/expiry before response release. Four additional mocked
owner integration tests cover separate revocation, mid-request revocation,
withdrawal, unapproved vault and missing owners. These interfaces still need real
adapters and transport-lifetime coordination; no production HTTP route is enabled.
The boundary reuses the gateway's existing bounded recall argument validator and
checks issuer authorization after scope/lifecycle awaits. Six boundary tests plus
nine policy tests pass. Independent review confirmed fixes for argument propagation
and revocation during owner reads. Cross-owner fencing and authorization at final
transport delivery remain explicitly unimplemented.

Do not change global admission when revoking one client. Use the existing canonical
issuer's versioned client-admission/family mechanisms; do not create a second live
admission store from this reference state machine. Resolve dot's actual management
callback and runtime tool exposure separately; ChatGPT setup alone is insufficient.
Claude's known callback is retained by the coordinator, not hardcoded into fixture
code. Revocation during recall must withhold the response.

## Remaining production work

Concrete issuer adapter now implemented in `persistent-issuer-owner.mjs`: uses the
existing confidential `issuerAdapter` with broker credential callback and real
metadata/introspection wire format. It validates exact issuer/audience/scope,
subject/client allowlists and bounded iat/exp on every check. Canonical memory
introspection already verifies client/global admission versions but does not
return grant_version. Same-token SHA256 binding is not a fabricated grant epoch;
revocation authority remains fresh issuer introspection. Seventeen targeted tests
include composition, inactive token and the negative claims matrix. No issuer
source edit or live credential request was needed.

Required shared contracts, for coordinator assignment:
- Issuer owner retains `src/lib/auth/personal-oauth/*`; no request to expose token
  secrets or duplicate admission storage. Confirm current memory introspection
  preserves internal version checks during the LifeGraph authority transition.
- Privacy/storage owner must provide canonical versioned per-record eligibility
  and filtering for the approved real-memory vaults, binding the exact returned
  response under revocation/withdrawal fencing through transport completion.
  The current gateway explicitly has no verified real-record privacy filter.
  Text heuristics, copied LifeGraph receipts or operator booleans cannot replace it.
- Lifecycle owner must expose the adopted credential expiry and serving state,
  and coordinate rotation/withdrawal with in-flight responses. No existing durable
  implementation was identified; the reference model is not that implementation.

No account-specific ChatGPT callback is included. Observed unsaved-form callback
persistence remains unverified; production client configuration stays absent.

Actual per-record privacy authority and approved real-memory scope; durable
admission integration; broker rotation/adoption mechanism; persistent supervised
service, restart/health/observation implementation; tested rollback bundle; verified
client compatibility and exact action approval. No timeout extension of the 1185s
synthetic fixture is used here. This is an initial bounded code increment, not a
complete persistent rollout.

Validation: targeted public-fixture tests cover missing inputs, duplicate client
bindings, independent revocation, scope/expiry denial, rotation evidence, missing
privacy authority and revocation during recall. No network, database or credentials.
