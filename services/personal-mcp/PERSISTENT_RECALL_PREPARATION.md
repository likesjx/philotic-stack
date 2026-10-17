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

Do not change global admission when revoking one client. Use the existing canonical
issuer's versioned client-admission/family mechanisms; do not create a second live
admission store from this reference state machine. Resolve dot's actual management
callback and runtime tool exposure separately; ChatGPT setup alone is insufficient.
Claude's known callback is retained by the coordinator, not hardcoded into fixture
code. Revocation during recall must withhold the response.

## Remaining production work

Actual per-record privacy authority and approved real-memory scope; durable
admission integration; broker rotation/adoption mechanism; persistent supervised
service, restart/health/observation implementation; tested rollback bundle; verified
client compatibility and exact action approval. No timeout extension of the 1185s
synthetic fixture is used here. This is an initial bounded code increment, not a
complete persistent rollout.

Validation: targeted public-fixture tests cover missing inputs, duplicate client
bindings, independent revocation, scope/expiry denial, rotation evidence, missing
privacy authority and revocation during recall. No network, database or credentials.
