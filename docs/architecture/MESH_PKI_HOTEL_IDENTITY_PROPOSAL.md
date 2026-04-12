---
title: Philotic Mesh PKI and Hotel Identity Proposal
doc_type: proposal
domain: mesh-placement
status: accepted for current slice
last_updated: 2026-04-12
tags:
- pki
- identity
- security
- mesh
- crypto
- invite
- perimeter
related_docs:
- HOTEL_PERIMETER_TRUST_PROPOSAL.md
- NATIVE_OVERLAY_VPN_PROPOSAL.md (archive)
- PERIMETER_EGRESS_CONTROL_PROPOSAL.md
- INTER_HOTEL_ROUTING_PROPOSAL.md
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md
proposal_id: mesh-pki-hotel-identity
implements:
- hotel-perimeter-trust
implemented_by: []
active_seams:
- hotel-identity-keypair
- mesh-invite-ceremony
- per-peer-session-keys
- mesh-membership-revocation
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- HOTEL_PERIMETER_TRUST_PROPOSAL.md
---

# Philotic Mesh PKI and Hotel Identity

## Goal

Define the cryptographic identity model for Philotic hotels, aligning the
invite/join ceremony, inter-hotel beacon authentication, and the longer-term
native overlay vision into one coherent PKI posture.

The overarching idea: **Philotic hotels behave like WireGuard peers**.
Each hotel has a stable keypair. Identity is the public key. Trust is explicit and
auditable. Traffic is authenticated per-peer. Joining is a ceremony, not config osmosis.

## Disposition

`accepted for current slice`

S1 (hotel identity bootstrap) and S2 (signed invite + ECDH session key) are in
active implementation. S3–S6 are accepted direction for follow-on slices.

## Why PKI, Not Just PSK

| Property | PSK | Per-hotel keypairs |
|---|---|---|
| Identity | None — "speaks the language" | Public key = stable hotel identity |
| Compromise blast radius | One leaked PSK = entire mesh | One leaked private key = one hotel |
| Revocation | Change PSK = reconfigure every node | Remove one public key from members |
| Non-repudiation | Impossible | Messages provably from a keyholder |
| Join ceremony | Config file copy | Explicit signed invite ceremony |
| Scalability | Collapses at N>2 | Scales to any N |

The PSK model was fine for local dev with one or two hotels. It is not the right
primitive when hotels are on different machines, different operators may participate,
or when "which hotels are trusted and why" must be answerable.

## The Core Model (WireGuard-Inspired)

Each hotel has two keypairs:

### 1. Long-Term Identity Keypair (Ed25519)

- Generated once on `phil init` or first `aiua` startup
- Private key stored in hotel vault (`~/.philotic/<profile>/vault/hotel_private_key`)
- Public key freely shareable — this IS the hotel's identity
- Used for: signing invite payloads, signing control-plane messages (future), human fingerprint

### 2. Ephemeral Session Keypair (X25519 — per invite / per session)

- Generated fresh for each invite ceremony
- Used for: ECDH with the joining hotel's ephemeral X25519 key
- Yields a shared secret → HKDF → per-peer BeaconSessionKey
- Never stored beyond the active session
- Rotated on key expiry or explicit re-key

This is the same separation WireGuard uses (permanent identity + ephemeral DH keys).

## The Invitation Ceremony (Revised v1 → v2)

### What changes from v1

v1 (shipped): PSK in the invite URL. Operator delivery channel = trust anchor.
Nonce + TTL prevent replay, but a URL leak = mesh access.

v2 (this slice): **PSK is never transmitted**. The invite carries cryptographic
material that allows both sides to independently derive the same session key.
An intercepted invite URL cannot be used to join the mesh — it can only be used
by someone with Hotel A's private key to create new invites.

### Invite URL Structure (v2)

```
philotic-invite://v2/<base64url(payload)>.<base64url(ed25519_signature)>
```

**`InvitePayload` (v2):**
```json
{
  "version": 2,
  "inviter_hotel_id": "hotel-a",
  "inviter_ed25519_pubkey": "<base64url>",
  "inviter_x25519_ephemeral_pubkey": "<base64url>",
  "inviter_mesh_addr": "hotel-a.ts.net:8999",
  "inviter_execution_addr": "hotel-a.ts.net:9002",
  "nonce": "<32 random bytes, hex>",
  "valid_until": 1234567890,
  "mesh_domain": "default",
  "allowed_capabilities": ["route.model", "route.tool"]
}
```

**`signature`**: Ed25519 sign(hotel_private_key, canonical_json(payload))

**What verified properties the recipient gets:**
- `inviter_ed25519_pubkey` is the claimed identity of the inviting hotel
- The signature proves the invite was created by whoever holds the corresponding private key
- The nonce ensures this exact invite was not replayed from a previous session
- `valid_until` ensures the invite expires
- TOFU (Trust On First Use): the operator is the trust anchor who delivers the URL; the public key is accepted on first join and pinned to the peer record thereafter

### Join Ceremony (v2)

```
Hotel A (inviter)                           Hotel B (joiner)
─────────────────                           ─────────────────
1. Generate ephemeral X25519 keypair
   Sign invite payload with Ed25519 priv key
   Emit: philotic-invite://v2/<payload>.<sig>

2. (Operator delivers URL via any confidential channel)

3.                                          Parse URL
                                            Verify Ed25519 signature
                                            Validate nonce (not seen before)
                                            Validate TTL (not expired)
                                            Generate own ephemeral X25519 keypair
                                            Compute: shared = X25519(B_priv, A_pub_ephemeral)
                                            session_key = HKDF-SHA256(shared, "philotic-mesh-v2")
                                            Store A's identity pubkey + session_key → MeshMemberRecord
                                            Mark nonce consumed in graph
                                            Send JoinRequest to A's execution addr:
                                              - B's hotel_id
                                              - B's Ed25519 pubkey (identity)
                                              - B's X25519 ephemeral pubkey
                                              - echo of invite nonce
                                              - B's mesh addr + execution addr

4. Receive JoinRequest:
   Verify nonce matches outstanding invite
   Mark nonce consumed
   Compute: same shared = X25519(A_priv_ephemeral, B_pub_ephemeral)
   session_key = HKDF-SHA256(shared, "philotic-mesh-v2")
   Store B's identity pubkey + session_key → MeshMemberRecord
   ← Send JoinAccepted:
     - A's Ed25519 pubkey (redundant but explicit)
     - A's X25519 ephemeral pubkey (B already has it from invite, but confirm)
     - assigned capabilities for B

5.                                          Receive JoinAccepted
                                            Confirm A's pubkey matches invite
                                            → Both sides now have matching session_key
                                            → Beacon traffic is HMAC'd with per-peer session_key

6. Both hotels begin heartbeating with per-peer HMAC
```

**Key property:** The PSK is never transmitted. Even if the invite URL is intercepted,
an attacker cannot complete the ECDH handshake without Hotel A's ephemeral private key.
An intercepted `JoinRequest` also can't be replayed because the nonce is single-use.

## Security Properties

| Attack | Defense |
|---|---|
| Invite URL interception | Attacker can't complete ECDH without A's ephemeral private key |
| Invite URL replay | Single-use nonce (consumed on first accept, both sides) |
| Stale invite from old operator | `valid_until` TTL (default 30 min) |
| Rogue hotel claiming a name | Must present invite signed by A's known private key |
| Beacon HMAC forgery | Per-peer session key; unknown hotels rejected at beacon |
| PSK leak from config | No PSK to leak — session keys are derived, never stored in plaintext config |
| Session key leak from graph | Keys encrypted at rest by hotel vault |
| Hotel revocation | Remove from `MeshMemberRecord`; add to deny list; rotate own ephemeral material |

## Beacon Authentication (per-peer HMAC)

Currently: one global PSK → one HMAC key for all traffic.

Target: per-peer HMAC key derived from the join ceremony.

```
BeaconMessage.hmac = HMAC-SHA256(
  key  = MeshMemberRecord[src_node].session_key,
  data = canonical_beacon_fields
)
```

The beacon handler looks up the source node's session key from the
`MeshMemberRecord` store before verifying. Unknown source nodes are rejected
before the payload is even parsed.

This is structurally equivalent to WireGuard's `AEAD(session_key, ...)` on each
receive — the session key proves both identity and freshness of the peer relationship.

## PKI Storage Layout

```
~/.philotic/<profile>/
├── vault/
│   ├── hotel_private_key    # Ed25519 secret key — vault-encrypted, never leaves host
│   └── ...other vault entries
├── identity/
│   ├── hotel_public.key     # Ed25519 public key — freely shareable (matches existing phil init output)
│   └── fingerprint          # human-readable fingerprint for operator display
└── context.db               # graph includes: encrypted session keys, MeshMemberRecords
```

## MeshMemberRecord (updated)

```rust
pub struct MeshMemberRecord {
    pub hotel_id: String,
    pub ed25519_pubkey: Vec<u8>,       // identity — pinned on first join (TOFU)
    pub session_key_encrypted: Vec<u8>, // HKDF-derived, encrypted at rest in vault
    pub execution_addr: String,
    pub beacon_addr: String,
    pub allowed_capabilities: Vec<String>,
    pub joined_at: u64,
    pub last_heartbeat: u64,
    pub mesh_domain: String,
    pub status: MemberStatus,
}
```

## HotelIdentity (added to ansible-mesh-core)

```rust
pub struct HotelIdentity {
    pub hotel_id: String,
    pub ed25519_public_key: Vec<u8>,   // stable, shareable
    pub fingerprint: String,           // human-readable: "AB:CD:EF:..."
    pub created_at: u64,
}
// private key lives in vault only — never in this struct
```

## Alignment With Existing Proposals

### HOTEL_PERIMETER_TRUST_PROPOSAL.md

This proposal directly implements the three-layer model from that doc:

- **Discovery**: `MeshMemberRecord` provides the inventory of known hotels
- **Identity**: Ed25519 keypair is the cryptographic identity
- **Authorization**: `allowed_capabilities` in `MeshMemberRecord` is the per-hotel authz scope

### NATIVE_OVERLAY_VPN_PROPOSAL.md (archive)

That proposal calls out "certificate fingerprint / public key / identity material"
as what execution-plane reachability records should eventually carry.
`HotelIdentity.fingerprint` and `MeshMemberRecord.ed25519_pubkey` are exactly that material.

The invite ceremony PKI also provides the "cryptographic node identity and key lifecycle"
that the archived proposal listed as a requirement for any future native overlay.

### PERIMETER_EGRESS_CONTROL_PROPOSAL.md

The per-peer session key model gives us a machine-checkable egress boundary:
outbound beacons only carry HMACs derived from known session keys. Outbound traffic
to unknown hotels fails immediately, providing a natural control-plane egress gate.

## VPN Nature of the Hotel Mesh

A Philotic mesh, when fully realized, behaves like a private WireGuard-style overlay network:

- **Each hotel = a peer with a keypair** (not an IP address)
- **Joining = an explicit ceremony** (invite → ECDH → session key derivation)
- **Traffic = authenticated per-peer** (HMAC using derived session key)
- **Membership = explicit and revocable** (MeshMemberRecord + deny list)
- **Discovery = layered** (beacons for liveness, execution plane for task traffic)
- **Identity survives network changes** (hotel_id + pubkey ≠ IP address)

The Tailscale underlay remains the transitional network transport in the current deployment.
This PKI layer sits above it and is independent of it — Philotic's identity and trust model
is defined by keypairs, not by Tailscale node addresses.

## Implementation Slices

| Slice | Scope | Status |
|---|---|---|
| S1 — Hotel identity bootstrap | Ed25519 keypair generation on `phil init`; `HotelIdentity` struct; `aiua mesh identity` CLI | Partially exists (`phil init` generates operator key; needs hotel-owned key) |
| S2 — Signed invite + ECDH session key | v2 invite URL with Ed25519 sig; X25519 ephemeral keypair; HKDF session key derivation; replace v1 PSK flow | **This slice** |
| S3 — Per-peer beacon HMAC | Beacon handler looks up per-peer session key; unknown source = reject; `PHILOTIC_MESH_DEV_MODE` bypasses | Next slice after S2 |
| S4 — MeshMemberRecord persistence | Encrypted session keys in vault; `MeshMemberRecord` in graph; `aiua mesh list`, `aiua mesh revoke` CLI | Follows S3 |
| S5 — Revocation propagation | `MeshRevoke` beacon broadcast; deny list in graph; TTL-based stale member eviction | Follows S4 |
| S6 — Execution plane auth | Per-peer auth on TCP execution plane (`:9002`), not just beacon | Long-term |

## Open Decisions

1. **TOFU vs pre-shared fingerprint**: On first join, the recipient trusts the embedded
   public key without prior verification (TOFU). The operator CAN verify the fingerprint
   out-of-band. Should we require fingerprint confirmation before join completes?
   Recommend: optional `--confirm-fingerprint <fp>` flag on `accept`; warn but don't block.

2. **Key rotation**: How often are ephemeral X25519 keys rotated in long-running meshes?
   Recommend: new ephemeral key per-invite; session keys have 24h TTL and re-key via
   a new `MeshRekey` ceremony (re-using the same long-term identity pubkeys).

3. **Vault encryption of session keys**: The session key derived from ECDH must be
   encrypted at rest. Use the hotel vault's AES-GCM key (already exists in the vault subsystem).

4. **`PHILOTIC_MESH_DEV_MODE`**: In dev mode, skip HMAC verification and accept all
   heartbeats. Must not compile into release builds for prod targets.
