---
title: Memory Token Self-Heal
doc_type: proposal
domain: memory-context
status: proposed
last_updated: 2026-07-21
related_docs:
- docs/architecture/MUNINN_VPS_REHARDEN_PROPOSAL.md
tags:
- muninn
- memory-core
- resilience
- tokens
---

# proposal:memory-token-self-heal — PROPOSED (spec stage)

Filed 2026-07-21 after the muninn 401 incident (handoff PR #339, follow-up 5).
Coupled with `proposal:muninn-vps-reharden` (implemented — see related doc),
which supplies the durable admin-credential source this proposal needs.

## Problem

The muninn token↔key binding spans two independent stores: the hotel DB
holds encrypted `mk_` tokens (`vault_registry` → `graph_nodes` secret
records), MuninnDB holds the SHA-256 key hashes in Pebble. When Muninn's
half is wiped (2026-07-20 rebuild) the hotel's stored tokens go stale and
every memory-core call 401s until an operator manually resyncs. This has
now recurred twice.

## Design

At the memory-core `rest_client` `with_auth` call sites, distinguish a
**token-401** (server reachable, token rejected) from *unreachable*. On
token-401:

1. Re-mint a fresh `mk_` key via the muninn admin API, using the admin
   credential from hotel config (`context_graph.muninn` — now rendered from
   `vault_muninn_admin_password` by the reharden work, so it is always
   current).
2. Re-encrypt with the hotel master key and update the `graph_nodes` secret
   record + `vault_registry` entry in place (hotel DB is truth).
3. Retry the original request **once**. On a second 401, surface the error
   (no retry loops).

Reference implementation: the 2026-07-21 manual DB-centric resync
(mint fresh keys → re-encrypt with hotel master key → update `graph_nodes`
secret records in place → restart; verified WRITE 201, 0 401s).

## Rationale

Makes the two-store binding self-healing in the direction of the operator's
"DBs are truth" philosophy: the hotel DB remains authoritative, Muninn's
key store becomes reconstructible state. Together with the reharden
baseline (hardened config rebuilds restore + daily muninn backups that
include the key store), rebuilds can no longer strand the hotel's memory.

## Slices

1. Error taxonomy: token-401 vs unreachable at `rest_client` `with_auth`
   sites (no behavior change; log + metric).
2. Self-heal path behind a config flag: re-mint + re-store + single retry.
   Verify by deleting a key in muninn and watching the hotel recover.
3. Cache invalidation: philotes keep cached engines after
   `refresh_memory_config` (known gap — the broadcast never returns a
   response); ensure re-minted tokens propagate without a hotel restart.
4. Audit: emit a hotel event on every self-heal so silent key churn is
   visible.
