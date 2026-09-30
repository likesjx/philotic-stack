---
title: Memory Token Self-Heal — Auto Re-Mint Across the Two-Store Token Binding
doc_type: proposal
proposal_id: memory-token-self-heal
domain: memory-context
status: accepted-current-slice
disposition: accepted for current slice
last_updated: 2026-07-24
verification_level: smoke-green (S1-S3 live on mac-jane); S4 drill executed end-to-end on an isolated hotel
tags:
- muninn
- memory
- self-healing
- secrets
- tokens
- substrate
related_docs:
- SUBSTRATE_HARDENING_PROPOSAL.md
- MUNINN_VPS_REHARDEN_PROPOSAL.md
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

The mint path requires an admin credential. When this proposal was filed that
credential did not exist on vps — the rebuilt MuninnDB was open (no admin
auth). The companion proposal
[**`muninn-vps-reharden`**](MUNINN_VPS_REHARDEN_PROPOSAL.md) has since
**shipped** (PR #346, 2026-07-21, applied live on jane-vps): admin + token auth
restored *without* rotating `auth_secret`, and `mesh-config.json.j2` now renders
`context_graph.muninn` from the same vaulted `vault_muninn_admin_password`, so
hotel provisioning and muninn admin creds cannot drift. That closes this
proposal's only external dependency — the `--load-config` `muninn` object
fallback in `resolve_admin_credential` now resolves fleet-wide, not just on
mbp/mac.

The interface remains narrow, and the degraded path is retained deliberately:
an admin credential resolvable from the Context Graph as a `SecretRecord`
(`secret_kind: "muninn_admin_credential"`), decrypted via the same
`resolve_secret` path as vault tokens, preferred over the config fallback.
Absent either source, heal degrades to a loud, throttled operator escalation
instead of a mint — a hotel with no way to mint must escalate, never spin.

## Slices

| Slice | Content | Size | Verify |
|---|---|---|---|
| S1 `auth-error-as-signal` | memory-core: introduce a typed error (`MemoryError::TokenRejected { vault }`) raised wherever a muninn response is `401 UNAUTHORIZED` — both the `discover_vault` loop and single-vault ops (check status before `error_for_status`). Philote callers in `memory_integration.rs` log it distinctly from unreachable; the hotel gains a `muninn_authorized` signal alongside `muninn_available` so doctor/status can say "reachable but rejecting tokens". No behavior change beyond classification. | S | test-green (unit: 401 → `TokenRejected`; 5xx/conn-refused unchanged) |
| S2 `hotel-remint-on-401` | New IPC op `HealMemoryToken { vault }` in aiua: guardrails (vault must already be in `vault_registry`; per-vault mint budget, e.g. 1 mint / 10 min with backoff; heal-queue event + graph mutation on every attempt), then admin login → `POST /api/admin/keys` → `rotate_secret` on the registry `secret_ref` → return fresh config. Admin credential resolved from the graph (`muninn_admin_credential`, from `muninn-vps-reharden`); absent credential → throttled operator escalation, no mint. Also fix the provisioning skip: `provision_muninn_vaults` validates the stored token with a cheap authenticated probe instead of the registry-presence `continue` at `muninn_provision.rs:152-156`, so `--load-config` after a key wipe re-mints instead of skipping. | M | smoke-green (test muninn: wipe keys → next write 401s → auto re-mint → retried write 201; budget exhaustion escalates, no mint storm) |
| S3 `config-propagation-without-restart` | Close the cached-config gap: `FetchMemoryConfig` serves the config LIVE from the Context Graph (`load_muninn_config`) instead of the boot snapshot — DBs are truth — so any post-boot rotation reaches re-fetching guests; the `HealMemoryToken` response itself carries the refreshed config, and the philote replaces its cached `muninn_config` from it before the single retry. (Tokens are never pushed over the hotel-wide broadcast — `RefreshMemoryConfig` stays a reachability bool.) Removes the "restart the hotel" step from the runbook entirely. | S–M | smoke-green (manual `rotate_secret` → next `FetchMemoryConfig`/heal picks up the new token with no restart) |
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
"Muninn token durability"). Coupled with
[`proposal:muninn-vps-reharden`](MUNINN_VPS_REHARDEN_PROPOSAL.md), the admin
credential source — **implemented and applied live 2026-07-21** (PR #346).

**Accepted for the current slice**: S1–S3 smoke-green and live; S4
implemented but its destructive path is unexercised. S1–S3 are merged
(PR #342) and live on mac-jane — running binary sha verified against the
build, probe green. S4's drill is implemented with its rails and unit tests,
but its destructive path has never executed; see the slice status below for
the two concrete blockers. This doc is the single
home for the spec; the short spec-stage stub that briefly lived at
`docs/specifications/MEMORY_TOKEN_SELF_HEAL_PROPOSAL.md` (added by
`af5fe885` for scanner visibility while this branch was open) was folded in
here to keep one canonical proposal per `proposal_id`.

### Slice status

- S1 `auth-error-as-signal` — implemented (this PR): typed
  `memory_core::TokenRejected` + `token_rejected_vault` helper, raised at
  every authed REST site including the cross-scope activate fan-out and the
  vault-discovery loop; unit-tested against canned 401/500 servers.
- S2 `hotel-remint-on-401` — implemented (this PR): `HealMemoryToken` IPC
  (`handle_heal_memory_token` in `ipc.rs`) with 10-min per-vault mint budget,
  registered-vaults-only refusal, and heal-queue escalation when no admin
  credential resolves; `remint_vault_token` + `resolve_admin_credential`
  (secret record preferred, `--load-config` `muninn` object fallback) in
  `muninn_provision.rs`; provisioning now probes stored-token validity
  (`probe_token_validity`, cookie-free client) and re-mints on 401 instead of
  the registry-presence skip. Budget semantics (adversarial-review-driven):
  inside the window the hotel does NOT mint again but DOES serve the live
  config — after the first guest's heal rotated the secret, every other
  guest of a shared vault heals off the same rotation instead of being
  stranded with a bare `HEAL_BUDGET_EXHAUSTED` for the rest of the window.
  The mint path resolves an admin credential fleet-wide now that
  `muninn-vps-reharden` (PR #346) renders `context_graph.muninn` from the
  vaulted admin password — that lands as the `muninn` config key the
  `resolve_admin_credential` fallback reads. **Not yet exercised against a
  real vps token-401**; until a live heal (or the S4 drill) confirms it,
  treat end-to-end vps re-mint as unverified. The no-credential escalation
  path is retained deliberately for hosts that lack one.
- S3 `config-propagation-without-restart` — implemented (this PR):
  `FetchMemoryConfig` loads live from the Context Graph; heal response
  carries refreshed config; philote replaces cached `muninn_config` and
  retries once at auto-recall, `memory.recall`, and `memory.remember`.
- S4 `key-wipe-drill` — **implemented and executed end-to-end**.
  New `memory-token-wipe` scenario in `scripts/chaos-smoke.sh` plus the
  `memory_token_drill_driver` example that drives it over IPC. Two phases:
  a **read-only probe** (memory config served live from the Context Graph +
  `HealMemoryToken` refusing an unregistered vault) that is safe on a live
  hotel, and a **destructive corrupt→heal assertion** gated behind a
  deliberately-provisioned sacrificial vault.

  Deliberate deviation from the slice as specified: the drill corrupts **the
  hotel's** half of the binding (`RotateSecret`) rather than wiping
  **MuninnDB's** key store. Both produce an identical token-401 at the same
  `with_auth` call site, so the circuit under test is unchanged, but this
  variant never touches Pebble, needs no admin credential to break anything,
  and so cannot strand a real vault. Rails: sacrificial-vault-only
  (`vault_name_denied`, duplicated in the driver so neither layer stands
  alone), never auto-selected by the round-robin, never auto-creates a vault,
  original token captured and restored on every failure path.

  Verification reached: **the corrupt→heal circuit has now executed
  end-to-end.** Run 2026-08-05 against a dedicated, isolated drill hotel
  (`PHILOTIC_PROFILE=drill`, own `context.db`, own socket, own
  auto-negotiated port cluster, **zero agents** so `derive_vault_names`
  yields `[]` and no real vault is ever provisioned), driven by a locally
  built binary so no Cellar deploy and no production hotel were involved.

  Proven, with server-side evidence rather than the driver's own word:

  | Case | Result |
  |---|---|
  | corrupt → heal → re-mint | **PASS** — placeholder `fp:8f64a815` corrupted, heal returned a different token `fp:c27e7dd0`, config refreshed with no restart |
  | mint really happened | **PASS** — MuninnDB shows a new key `mcfCnMADo2I` (label `aiua-1785941607`) created 10:53:27.84, matching `aiua::muninn_provision: MuninnDB vault token re-minted and rotated in place` to the second |
  | missing admin credential escalates | **PASS** — classified `NO_ADMIN_CREDENTIAL` refusal plus the `manual resync required` WARN; no mint attempted, no silence |
  | failure restores state | **PASS** — the driver's restore path returned the vault to its pre-drill token on every failed run |

  Note on the escalation case: the hotel's heal-queue write is optional
  (`if let Some(hq) = heal_queue`), and this minimal drill hotel has no queue
  wired, so the escalation there was log-only. The heal-queue leg was proven
  separately on mac-jane, which filed and dispatched an entry for a failed
  heal earlier the same session.

  Three findings the drill surfaced that no test had:

  1. **A heal that fails still consumes the mint budget.** The per-vault
     budget slot is claimed *before* the admin-credential check, so a heal
     that never attempted a mint still throttles the next attempt for the
     full 10-minute window — during which the hotel serves the live (still
     corrupted) config. Retrying a failed heal inside the window therefore
     looks like "no re-mint happened" for the wrong reason. The budget
     arguably belongs after the credential resolution.
  2. **The mint path is not Cortex-aware — self-heal cannot work on an
     observer hotel.** Mid-drill the local MuninnDB began rejecting the mint
     with `421 Misdirected Request`: *"this node is not the cluster's Cortex;
     writes are accepted only on the Cortex"*. Minting an API key is a write,
     and `remint_vault_token` posts straight to the configured endpoint.
     `MuninnConfig` already carries `shared_write_route` for forwarding
     *data* writes to the Cortex, but the *admin mint* has no equivalent — so
     on the Macs (observers) a token-401 can never self-heal. It works on
     vps-jane because vps-jane *is* the Cortex, which happens to be where
     both original incidents occurred. This is the highest-value next seam.
  3. **Classified refusals were being reported as protocol errors.** The
     hotel returns its designed refusals as `Standard{ok:false}`; the driver
     treated that shape as "unexpected response" and dumped the debug struct.
     Fixed: refusals now print `[CODE] message` with an operator hint.

  Remaining blocker for a *scheduled* drill (the drill itself no longer
  needs it): **the chaos preflight blocks every scenario on mac-jane.**
  `preflight_check` refuses unless `phil doctor` reports `ok == true`, but
  mac-jane's `ok` is `false` on **warnings only**
  (`ports.hotel-record-drift`, `logs.rotation-missing` — the latter a known
  false positive, since log rotation is handled by a copytruncate
  LaunchAgent). Pre-existing in SUBSTRATE_HARDENING S4, not introduced here.

  The earlier "no sacrificial vault can exist" blocker is **closed**: vaults
  could not be registered at all until `AddVaultEntry` gained an explicit
  `secret_kind` (PR #384), because `load_muninn_config` skips any entry whose
  kind is not `muninn_vault_token` and `derive_vault_names` only ever emits
  `self_*`/`user_*`. The driver's `provision` mode now registers a
  `chaos_smoke*` vault with a placeholder token MuninnDB rejects, so the
  first heal mints the real one and no admin credential lives in the driver.
