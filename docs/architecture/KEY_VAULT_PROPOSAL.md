# Key Vault Proposal

## Goal

Define a security-first secret-management model for Philotic that:

- removes raw secrets from the plain context-graph config path
- gives the hotel one canonical authority for secret storage, access, rotation, and audit
- supports hotel-managed OAuth refresh tokens safely
- supports operator secret onboarding and rotation with Telegram as a control surface, not a plaintext secret bucket

## Core Recommendation

Philotic should introduce a dedicated key-vault authority owned by the hotel.

The vault should:

- store secret payloads encrypted at rest
- expose only secret references and metadata in the context graph
- vend short-lived secret material to authorized local guests on demand
- maintain an audit trail for create/read/rotate/revoke operations
- support staged rotation and rollback

The context graph should not remain the long-term home for raw values like:

- API keys
- OAuth refresh tokens
- bot tokens
- webhook secrets
- signing keys

That was acceptable for the current bootstrap slice, but it is not an acceptable final authority boundary.

## Security Posture

Treat secret material like money.

That means:

- minimize who can see it
- minimize how long it exists in plaintext
- audit every meaningful operation
- assume accidental exposure is a real financial and operational risk, not an abstract hygiene issue

In particular:

- admin credentials are higher-trust than ordinary provider tokens
- model-facing components should not receive admin credentials
- LLM context should never contain raw secret values
- secret-bearing operations should prefer explicit control-plane flows over conversational improvisation

If a design leaves a raw key in prompt context, chat history, or model-visible tool output, the design is wrong even if the demo works.

## Disposition

Accepted for current slice.

## Current Slice

Pin and prove the first design contract for:

- hotel-owned key vault authority
- vault-backed model-controller auth
- Telegram-safe onboarding and rotation paths
- encrypted secret storage plus role/guest-gated local secret fetch over hotel IPC
- macOS Keychain-backed vault root key with env fallback only as a bootstrap path

Linked task surface: [docs/task.md](/Users/jaredlikes/code/philotic-stack-model-controller-abstraction/docs/task.md)

## Repo Truth Right Now

Current repo truth is that `node_config` stores values directly, including secrets, and guests fetch them through normal config reads.

That is convenient for bring-up and bad for long-term security.

The current implementation slice now begins that boundary:

- encrypted secrets are stored in `vault_secrets`
- config can store `*_ref` values instead of raw secret values
- guests can request secrets through dedicated hotel IPC instead of generic `GetConfig`
- Gemini OAuth access tokens now fit that path
- hotel-side validation can exercise the vaulted Gemini OAuth path directly before guest fallback obscures failures
- on macOS, the hotel can now load or create its vault root key in Keychain instead of requiring the operator to mint one manually in the shell

The next boundary should continue toward:

- context graph stores secret references plus metadata
- vault stores encrypted payloads
- guests request lease-scoped secret access from the hotel

## Recommended Vault Model

### Secret record

Each secret should have:

- `secret_id`
- `secret_kind`
- `provider`
- `scope`
  - hotel, component, session, agent, user
- `version`
- `state`
  - active, staged, revoked, archived
- `ciphertext`
- `created_at`
- `rotated_at`
- `expires_at`
- `owner`
- `rotation_policy`
- `usage_policy`
- `audit_labels`

### Context-graph reference

The context graph should store only:

- `secret_ref`
- intended consumer/component
- purpose
- allowed auth mode
- last rotation metadata

Example:

- `gemini_auth_ref = secret://hotel/default/gemini/oauth-refresh`
- `telegram_bot_token_ref = secret://membrane/telegram/bot-token`

## Encryption Recommendation

Use envelope encryption.

Recommended shape:

1. generate a random data-encryption key per secret version
2. encrypt the secret payload with an AEAD such as AES-256-GCM or XChaCha20-Poly1305
3. wrap the DEK with a root key
4. store ciphertext plus wrapped DEK and metadata

Root key priority:

1. hardware-backed OS secret store or TPM/Secure Enclave
2. cloud KMS / HSM-backed key for hosted hotels
3. operator-supplied master key only as a last fallback

Current implementation note:

- the current slice now uses macOS Keychain first for the local hotel root key
- `PHILOTIC_VAULT_MASTER_KEY` remains as a non-macOS/bootstrap fallback
- the Keychain record can be scoped locally with `PHILOTIC_VAULT_KEY_ID`
- hardware-backed attestation is still not proven just because Keychain is in the sentence

Strong recommendation:

- never leave the root key in the same SQLite file as the ciphertext
- never log decrypted values
- never persist raw secrets in guest process configs

## Access Recommendation

Guests should not read secrets through generic `GetConfig`.

Instead:

- the hotel authenticates the local guest identity
- policy checks the guest, role, hotel, and requested secret purpose
- the hotel returns either:
  - a short-lived bearer/access token
  - the decrypted secret payload over local authenticated IPC
  - or a lease handle that can be refreshed

Access rules:

- least privilege
- per-secret allowlist
- local-only delivery by default
- auditable reads
- optional operator approval for especially sensitive reads

### Secret classes

Not all secrets deserve the same exposure policy.

Recommended first classes:

- `provider-runtime`
  - example: short-lived Gemini access token
  - may be vended to a narrowly authorized local runtime guest
- `provider-root`
  - example: OAuth refresh token, long-lived API key
  - hotel/vault only unless there is no safer bounded alternative
- `admin`
  - example: operator signing keys, vault recovery material, destructive-control credentials
  - never exposed to model-facing components
  - never placed in prompt context
  - should require stronger admin-only workflows, stronger audit, and ideally staged or dual-control handling
- `transport`
  - example: mesh PSKs, webhook secrets, signing keys
  - only to the components that terminate or establish the transport boundary

The crucial rule is that admin secrets are not just “another config value with a scary name.”
They are a separate authority class.

### Model boundary

Model-facing components should receive only the smallest capability-specific secret material they absolutely need.

Examples:

- a model controller may receive a short-lived provider access token
- a membrane may receive a Telegram bot token if it must terminate the Telegram boundary
- an agent should receive references and capability outcomes, not raw admin credentials

Strong rule:

- no admin key material to the model
- no vault recovery material to the model
- no plaintext secret echo in model-visible error messages, traces, or logs

If a model needs to initiate an admin action, it should request a hotel-owned control-plane operation and receive a structured result, not the key itself.

Current implementation note:

- the first slice uses per-secret allowed roles/guests enforced by the hotel
- it returns the decrypted payload directly over local IPC after policy check
- lease handles, access audit records, and approval-gated reads are still deferred

## Rotation Recommendation

Every secret should support:

- stage new version
- validate new version
- cut over consumers
- revoke old version
- rollback if validation fails

Rotation should prefer:

- dual-read / single-write rollout where upstream systems allow overlap
- explicit version pinning during rollout
- automated expiry reminders

The vault should record:

- who initiated rotation
- what rotated
- which consumers were rebound
- whether rollback occurred

## OAuth Recommendation

OAuth credentials should be split by sensitivity:

- access token
  - short-lived, vendable to guests
- refresh token
  - long-lived, vault-only
- client secret / OAuth client config
  - vault-managed

The model-controller guest should normally receive only a short-lived access token.

The hotel should own:

- authorization-code exchange
- refresh flow
- token caching
- token expiry handling
- revocation

That avoids the deeply ironic design where the component with the least stable lifecycle becomes the owner of the most sensitive credential.

## Telegram Recommendation

Telegram should be treated as a control surface, not a trusted raw-secret transport.

Security-first rule:

- do not accept plaintext secrets through ordinary Telegram chat messages

Why:

- bots receive private-chat messages directly
- bot chats are not secret chats
- bots are cloud/API-integrated by design, not end-to-end secret channels

Telegram is still useful for:

- starting an auth flow
- requesting rotation
- approving or denying rotation
- launching a secure operator mini app
- monitoring vault events

The admin/control implication should be explicit:

- Telegram via `membrane` may initiate or broker secret administration
- Telegram via `membrane` should not own vault mutation authority
- secret add/rotate flows belong to the hotel control plane and vault, with `membrane` acting as the outside-world entry point only

## Admin Key Management Recommendation

Admin keys should be managed as a distinct control-plane concern, not folded into ordinary runtime secret handling.

Recommended rules:

- admin keys stay in the vault or hardware-backed store
- admin actions should use signing, approval, or delegated control-plane operations rather than raw key release whenever possible
- the hotel should expose admin operations as capability requests, not as “give me the key”
- recovery or export paths should be exceptional, audited, and ideally require stronger ceremony than normal runtime secret access

Examples of admin-key uses that should stay hotel-owned:

- signing membership or invite actions for trusted hotels
- rotating mesh trust material
- approving destructive or perimeter-changing actions
- vault recovery or break-glass operations

This keeps the system from committing the deeply ironic mistake of making the most improvisational component the holder of the highest-trust credentials.

## Safe Telegram Onboarding Path

Recommended safe path:

1. operator issues `/vault add gemini` or `/vault rotate elevenlabs`
2. bot verifies the operator against allowlists and operator policy
3. bot returns a one-time action link or Mini App launch
4. Mini App provides a secure operator UI
5. Mini App authenticates the Telegram launch using validated `initData`
6. hotel/backend binds the session to a one-time vault action grant
7. secret is submitted through an end-to-end encrypted application flow
8. hotel validates, stores, stages, and confirms

### Important detail

The safe version is not:

- user sends the API key as a chat message

The safe version is:

- Telegram starts an authenticated control session
- a dedicated secure flow handles the actual secret value

## Telegram Submission Modes

### Mode A: Browser-to-hotel direct

Best when the hotel exposes a secure operator endpoint.

Flow:

- bot opens Mini App or browser link
- hotel provides one-time nonce and ephemeral public key
- browser encrypts the submitted secret client-side to the hotel key
- hotel receives ciphertext, decrypts locally, stores in vault

Pros:

- end-to-end at app layer between operator browser and hotel

Cons:

- requires reachable operator endpoint

### Mode B: Browser-to-relay encrypted envelope

Best when the hotel is not directly reachable.

Flow:

- hotel creates one-time action grant plus ephemeral public key
- operator Mini App encrypts secret to hotel public key
- relay/backend stores ciphertext only
- hotel pulls pending ciphertext and decrypts locally

Pros:

- relay never sees plaintext
- works when hotel is behind NAT

Cons:

- more moving pieces

### Mode C: Plain Telegram message

Not recommended for raw secrets.

May be allowed only for:

- non-sensitive references
- secret labels
- rotation commands
- confirmations

## Telegram Mini App Recommendation

If Telegram is used for secret administration, use a Mini App rather than plain messages.

Why:

- Telegram provides signed `initData` validation for Mini Apps
- Mini Apps support richer operator UX
- sensitive values can avoid appearing in raw chat transcript text

Even then:

- validate `initData` on the server
- require recent `auth_date`
- bind every session to a one-time action grant
- require operator re-auth for high-risk actions

Optional step-up controls:

- local hotel approval prompt
- passkey/WebAuthn in the Mini App
- TOTP confirmation
- out-of-band confirmation for destructive rotations

## Operator Flows

Recommended commands:

- `/vault status`
- `/vault add <provider>`
- `/vault rotate <provider>`
- `/vault revoke <secret_ref>`
- `/vault leases <provider>`

Sensitive actions should return:

- a signed action summary
- a one-time action id
- a short expiry
- a secure action link/Mini App button

## Audit Requirements

The vault should log:

- create
- read
- lease issuance
- refresh
- rotate
- revoke
- failed access attempts
- Telegram-triggered admin actions

Logs must include:

- operator identity
- guest/component identity
- secret reference
- action id
- source channel
- timestamp

Logs must not include:

- plaintext secret values
- full bearer/access tokens
- full refresh tokens

## Near-Term Slice Recommendation

Implement in this order:

1. define vault records and secret references
2. stop storing new raw secrets directly in plain `node_config`
3. add hotel-side secret fetch API for local guests
4. move Gemini OAuth refresh tokens and API keys behind vault references
5. add operator audit log for secret actions
6. define Telegram Mini App onboarding and rotation flow
7. only after that, implement Telegram-driven secret add/rotate UX

## Relationship To Other Proposals

- [AGENT_CONTEXT_MANAGEMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_CONTEXT_MANAGEMENT_PROPOSAL.md)
  - ordinary profile/context mutation must stay separate from secret mutation
- [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md)
  - admin surfaces should invoke hotel-owned secret operations without surfacing raw key material to model-facing roles
- [HOTEL_PERIMETER_TRUST_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HOTEL_PERIMETER_TRUST_PROPOSAL.md)
  - perimeter membership and trust operations will eventually depend on higher-trust admin key material
