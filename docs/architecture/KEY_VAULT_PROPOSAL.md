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

## Disposition

Proposed.

## Current Slice

Pin the first design contract for:

- hotel-owned key vault authority
- vault-backed model-controller auth
- Telegram-safe onboarding and rotation paths

Linked task surface: [docs/task.md](/Users/jaredlikes/code/philotic-stack-model-controller-abstraction/docs/task.md)

## Repo Truth Right Now

Current repo truth is that `node_config` stores values directly, including secrets, and guests fetch them through normal config reads.

That is convenient for bring-up and bad for long-term security.

The next boundary should be:

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
- `telegram_bot_token_ref = secret://hegemon/telegram/bot-token`

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
