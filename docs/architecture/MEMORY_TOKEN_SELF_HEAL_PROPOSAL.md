---
title: Memory Token Self-Heal — Auto Re-Mint Across the Two-Store Token Binding
doc_type: proposal
proposal_id: memory-token-self-heal
domain: memory-context
status: proposed
disposition: proposed
last_updated: 2026-07-21
verification_level: none
tags:
- muninn
- memory
- self-healing
- secrets
- tokens
- substrate
related_docs:
- SUBSTRATE_HARDENING_PROPOSAL.md
- MUNINN_V07_CAPABILITY_ADOPTION_PROPOSAL.md
- ../reference/MCP_CREDENTIAL_LIFECYCLE.md
- ../HANDOFF-2026-07-14-lifegraph-batch.md
---

# Memory Token Self-Heal — Auto Re-Mint Across the Two-Store Token Binding

> The hotel DB is truth. When the two halves of the muninn token↔key binding
> drift, the system should re-derive the disposable half from the durable one —
> automatically, not via an operator runbook at 1am.

## Goal

A muninn vault token is a binding that spans **two independent stores**:

- **MuninnDB's key store** holds the auth half (the hashed API key).
- **The hotel Context Graph** holds the token half — a `SecretRecord`
  (`crates/ansible-mesh-core/src/storage.rs:237`, node kind `"secret"` in
  `graph_nodes`) encrypted with the hotel vault master key
  (`crates/aiua/src/vault.rs`, AES-256-GCM, key from env → file → Keychain).

Nothing reconciles the two. This has now bitten twice in two days:

- **2026-07-20**: MuninnDB key reset → all stored hotel tokens stale → 401s on
  `/api/engrams` and `/api/activate`.
- **2026-07-21**: the vps cluster rebuild wiped MuninnDB's key store → Beacon's
  memory 401'd again. The hotel DB was fine; Muninn forgot its half.

Both incidents were resolved by the same **manual** resync: mint fresh keys via
the muninn admin API (login on UI port 8476, `POST /api/admin/keys`), IPC
`rotate_secret` on each registry `secret_ref`, restart the hotel. Verified
`WRITE 201`, 0 401s. That runbook is the reference implementation of this
proposal — the point is that no human should have to run it.

Four code-level gaps keep the binding brittle:

1. **Provisioning can't recover.** `provision_muninn_vaults`
   (`crates/aiua/src/muninn_provision.rs:152-156`) skips any vault whose name
   already appears in the graph `vault_registry` — a presence check, not a
   validity check. After a muninn key wipe, the stale entry blocks re-minting
   forever.
2. **401 is invisible.** memory-core surfaces `UNAUTHORIZED` only inside
   `discover_vault` (`crates/memory-core/src/rest_client.rs:584-600`), and even
   there it collapses to an opaque `anyhow` string. Single-vault ops
   (`remember`, `recall`) rely on `with_auth` (498) + `error_for_status`, so
   callers in `crates/philote/src/memory_integration.rs` (~1680, ~1718) log
   "memory engine unavailable/error" for both a dead server and a live server
   rejecting our token. The `muninn_available` flag is a TCP/health probe — a
   live-but-401 MuninnDB reads as "connected".
3. **Refresh doesn't refresh.** `IpcRequest::RefreshMemoryConfig`
   (`crates/aiua/src/service/ipc.rs:3022-3052`) only probes reachability and
   flips an atomic; it never re-reads `vault_registry` or re-decrypts tokens.
   Philotes fetch `muninn_config` **once** at startup
   (`memory_integration.rs:1211-1243`) and clone it per op — so even after a
   manual `rotate_secret`, a full hotel restart is required.
4. **No retry.** `discover_vault` walks known token pairs once and bails; no
   heal, no re-provision, no second attempt with a fresh token.

Goal: a token-401 becomes a first-class, self-healing event — detected as
distinct from unreachable, healed by re-minting from the durable hotel-DB
truth, propagated without a restart, and bounded by a mint budget.

## Core Recommendation

Heal on the hotel side, signal on the philote side. The philote sees the 401
but owns neither the master key, the graph, nor admin credentials; the aiua
hotel owns all three and already contains the complete mint+store flow in
`provision_muninn_vaults`. So:

1. memory-core reports `TokenRejected { vault }` as a typed error.
2. The philote forwards it over IPC as a heal request.
3. The hotel re-mints via the muninn admin API (exactly the provisioning flow:
   admin login on UI port = API port + 1, `POST /api/admin/keys`,
   `mode: "full"`), re-encrypts in place with `rotate_secret`
   (`crates/aiua/src/vault.rs:60` — preserves `secret_ref`, so the
   `vault_registry` entry stays valid), and returns the refreshed config.
4. The philote rebuilds its engine from the fresh config and retries the
   failed op **once**.

The mint path requires an admin credential. On vps that credential does not
currently exist — the rebuilt MuninnDB is open (no admin auth). That is the
companion proposal **`muninn-vps-reharden`** (spec-stage, not yet authored as a
doc): restore admin + token auth *without* rotating `auth_secret` (which would
re-break resynced tokens), and bake the hardened baseline into the deploy
source so rebuilds restore it. The interface this proposal needs from it is
narrow: an admin credential resolvable from the Context Graph as a
`SecretRecord` (proposed `secret_kind: "muninn_admin_credential"`), decrypted
via the same `resolve_secret` path as vault tokens. Slice S2 lands behind
that credential's presence: no credential → heal degrades to a loud,
throttled operator escalation instead of a mint.

## Slices

| Slice | Content | Size | Verify |
|---|---|---|---|
| S1 `auth-error-as-signal` | memory-core: introduce a typed error (`MemoryError::TokenRejected { vault }`) raised wherever a muninn response is `401 UNAUTHORIZED` — both the `discover_vault` loop and single-vault ops (check status before `error_for_status`). Philote callers in `memory_integration.rs` log it distinctly from unreachable; the hotel gains a `muninn_authorized` signal alongside `muninn_available` so doctor/status can say "reachable but rejecting tokens". No behavior change beyond classification. | S | test-green (unit: 401 → `TokenRejected`; 5xx/conn-refused unchanged) |
| S2 `hotel-remint-on-401` | New IPC op `HealMemoryToken { vault }` in aiua: guardrails (vault must already be in `vault_registry`; per-vault mint budget, e.g. 1 mint / 10 min with backoff; heal-queue event + graph mutation on every attempt), then admin login → `POST /api/admin/keys` → `rotate_secret` on the registry `secret_ref` → return fresh config. Admin credential resolved from the graph (`muninn_admin_credential`, from `muninn-vps-reharden`); absent credential → throttled operator escalation, no mint. Also fix the provisioning skip: `provision_muninn_vaults` validates the stored token with a cheap authenticated probe instead of the registry-presence `continue` at `muninn_provision.rs:152-156`, so `--load-config` after a key wipe re-mints instead of skipping. | M | smoke-green (test muninn: wipe keys → next write 401s → auto re-mint → retried write 201; budget exhaustion escalates, no mint storm) |
| S3 `config-propagation-without-restart` | Close the cached-config gap: philote reacts to `TokenRejected` (or a successful heal response) by re-fetching memory config (`FetchMemoryConfig`) and dropping the cached `muninn_config` before the single retry; `RefreshMemoryConfig` (`ipc.rs:3022`) additionally re-runs `load_muninn_config` so its broadcast carries refreshed tokens, not just a reachability bool. Removes the "restart the hotel" step from the runbook entirely. | S–M | smoke-green (manual `rotate_secret` → next memory op picks up the new token with no restart) |
| S4 `key-wipe-drill` | Fold the two-store drift into the substrate chaos smokes (SUBSTRATE_HARDENING_PROPOSAL S4): a scheduled drill on a designated hotel wipes/rotates a test vault's muninn key and asserts the full circuit — `TokenRejected` classified, heal-queue entry filed, re-mint within budget, retried op succeeds, audit mutation recorded — with zero manual steps. This is the regression gate for the 2026-07-20/21 recurrence class. | S | watched-live (first drill cycle reviewed; a deliberately-missing admin credential produces escalation, not silence) |

## Guardrails

- **Never race provisioning.** `HealMemoryToken` only re-mints for vaults
  already registered; new-vault creation stays in `provision_muninn_vaults`.
- **Mint budget.** Re-mint is idempotent from the graph's perspective
  (`secret_ref` preserved) but each mint invalidates nothing on the muninn
  side except by supersession — still, a genuinely misconfigured muninn must
  produce one escalation, not an unbounded mint loop. Budget + backoff +
  heal-queue visibility on every attempt.
- **Retry once.** The failed memory op retries exactly once after a heal;
  a second 401 in the same turn surfaces as a hard, classified failure.
- **Secret hygiene.** Raw tokens never appear in logs, graph mutations, heal
  events, or docs (per the Secret Handling rules in
  MUNINN_V07_CAPABILITY_ADOPTION_PROPOSAL.md); heal records carry vault name,
  key label, and mode only.
- **`auth_secret` is out of bounds.** Healing operates at the API-key layer.
  Rotating muninn's `auth_secret` invalidates every token at once and is an
  operator ceremony, never an automatic action.

## Disposition

Filed to the intel-graph 2026-07-21 as `proposal:memory-token-self-heal`
after the second manual resync in two days (see
[HANDOFF-2026-07-14-lifegraph-batch.md](../HANDOFF-2026-07-14-lifegraph-batch.md),
"Muninn token durability"). Coupled with `proposal:muninn-vps-reharden`
(admin credential source; not yet authored as a doc). Spec-stage; no slice
started.

### Slice status

- S1 `auth-error-as-signal` — not started
- S2 `hotel-remint-on-401` — not started (blocked on an admin credential
  source for vps; mbp-jane/mac-jane can land first where admin auth exists)
- S3 `config-propagation-without-restart` — not started
- S4 `key-wipe-drill` — not started (depends on S1–S3; slots into substrate
  chaos smokes)
