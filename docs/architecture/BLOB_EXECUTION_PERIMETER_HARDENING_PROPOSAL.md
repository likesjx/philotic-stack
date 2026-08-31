---
title: Blob and Execution Plane Perimeter Hardening Proposal
doc_type: proposal
domain: mesh-placement
status: accepted-current-slice
last_updated: 2026-08-31
tags:
- perimeter
- security
- blob
- execution-plane
- active-seam
related_docs:
- HOTEL_PERIMETER_TRUST_PROPOSAL.md
- MESH_PKI_HOTEL_IDENTITY_PROPOSAL.md
- PERIMETER_EGRESS_CONTROL_PROPOSAL.md
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md
- docs/DEFECTS.md
proposal_id: blob-execution-perimeter-hardening
implements:
- hotel-perimeter-trust
implemented_by: []
active_seams:
- blob-transport-auth
- execution-plane-connection-hardening
- perimeter-firewall-parity
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- docs/DEFECTS.md
---

# Blob and Execution Plane Perimeter Hardening Proposal

## Goal

Close the gap between what `HotelPerimeterService` *declares* about the hotel's
network exposure and what the hotel's listeners *actually* do — starting from
a confirmed, live, unauthenticated write primitive on the blob plane — and
extend the same audit to every other listener the hotel daemon binds, so the
answer to "does the perimeter have holes" is backed by a full listener
inventory, not a single finding.

## Disposition

`accepted for current slice`

Slice 1 is in flight as PR #479 (`codex/blob-perimeter-bind`, DEF-104). Slices
2–4 below are this proposal's scope. Track follow-on work in
[docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Core Finding

`HotelPerimeterService` (`crates/aiua/src/service/perimeter.rs`) is **purely
observational**. It classifies each declared listener's exposure tier from
live interface probes, persists a snapshot (`__hotel_perimeter__`), and logs a
warning on downgrade — there is no `enforce`/`firewall`/`block`/`deny`/`reject`
logic anywhere in that file. The perimeter is a self-report, not a gate. That
makes the accuracy of the declaration itself load-bearing: an operator (or an
automated policy) reading `graph_status`/perimeter snapshot has no way to know
it's wrong short of independently probing the socket.

`crates/aiua/src/main.rs` declared the blob listener to the perimeter as
`ListenerDecl { purpose: "blob", bind_addr: Ipv4Addr::LOCALHOST }` (with a
comment claiming *"Blob binds to 127.0.0.1"*) while the actual `serve()` call
~700 lines away hardcoded `format!("0.0.0.0:{}", blob_port)`. The two sites
were never tied together. The persisted snapshot reported `Local` while the
real socket was world-reachable.

The blob HTTP service (`crates/aiua/src/service/blob.rs`, 108 lines) has **no
authentication of any kind**:

- `POST /upload` — anonymous multipart write, 100MB per request, no aggregate
  storage quota.
- `GET /download/*` — a bare `ServeDir` over the whole blob storage directory.

Verified live off-tailnet 2026-08-31: vps-jane's `31.97.130.98:16467` (blob)
and `:16468` (execution) were both reachable from the public internet.
`iptables` INPUT policy was `ACCEPT`; `ufw` was not installed, so the
Ansible-managed firewall rule intended to restrict the blob port to the
tailnet (`ansible/roles/philotic_hotel/tasks/main.yml:210-218`, gated on
`ufw_check.rc == 0`) silently no-opped on that host.

Downloads are content-addressed (`sha256-<hex>`); `tower-http`'s `ServeDir`
(confirmed against the pinned 0.5.2 source, not assumed) has no directory-index
capability, so `/download/` 404s rather than listing — reads require already
knowing the hash. **Upload is the fully exposed vector**, and per DEF-078 (a
full disk wedges the hotel silently), an anonymous unlimited-request write
endpoint is a standing disk-fill DoS against any hotel with a public or
otherwise inbound-reachable address.

## What PR #479 Fixes, and What It Doesn't

PR #479 ties the `ListenerDecl` and the `serve()` bind to one
`BLOB_BIND_ADDR` constant so they can't drift again, narrows the real bind to
`127.0.0.1`, and adds a regression test. That's correct and should merge — it
closes the anonymous internet-facing write endpoint immediately, and the
failure mode of over-narrowing (below) is a loud connection error, not silent
data loss.

But its safety argument — *"`EventPayload::BlobRef.source_hotel_ip` exists in
the protocol but has no fetch-site consumer... there is no cross-hotel blob
transfer implemented to break"* — is factually incorrect. There is a real,
wired, end-to-end cross-hotel fetch path, independent of `BlobRef`:

- **Upload + URL construction** (`crates/aiua/src/service/ipc.rs:12608-12643`,
  the `agent.deploy_bundle` migration handler): uploads locally over
  `127.0.0.1`, then explicitly resolves `source_mesh_host` from
  `hotel_record.mesh_host` — the code's own comment reads *"Use the hotel's
  mesh_host for the externally-reachable URL"* — and builds
  `blob_url = http://{source_mesh_host}:{blob_port}/download/{blob_id}`.
- **Cross-hotel dispatch** (`ipc.rs:12685-12719`): that URL is embedded in the
  `agent.deploy_bundle` payload and sent to the destination hotel via
  `EmitTask` — a genuine cross-hotel task, not a local one.
- **Cross-hotel fetch** (`crates/aiua/src/main.rs:576-611`, `"agent.deploy_bundle"`
  surface handler): the destination hotel does `reqwest::get(blob_url)`
  (line 585) against that exact non-loopback URL and feeds the bytes into
  `ApplyAgentBundle`.
- **What's inside**: the bundle carries `VaultEntryExport { plaintext, .. }`
  (`ipc.rs:12570-12575`) — plaintext secret values, not references.

Merging PR #479 as written will silently break agent migration the next time
it's actually invoked between two different hosts — a rare, operator-triggered
action, which means the break could go unnoticed until someone needs it during
an actual migration or DR scenario. Worse, the current state (pre-#479) is not
merely "broken" — it means plaintext vault secrets are today fetched over a
mesh transport that is signed but **not encrypted**
(`crates/ansible-mesh-core/src/authz.rs` is `Hmac<Sha256>` only — Tailscale
supplies the only wire confidentiality), via an HTTP endpoint anyone on the
network path can also reach directly, unauthenticated, if they can guess or
observe the sha256 blob_id. Narrowing the bind without restoring authenticated
transfer trades one problem for a different one; it doesn't finish the fix.

## Proposed Slices

### Slice 1 — Bind/declaration consistency (PR #479, in flight)

Status: test-green, not deployed. Recommend merging with two corrections:

1. Fix the PR body / DEF-104 text's false "no fetch-site consumer" claim
   before it becomes the historical record — link this proposal instead.
2. Land Slice 2 promptly; don't treat the loopback narrowing as done.

### Slice 2 — Authenticated cross-hotel blob transfer

Restore the `agent.deploy_bundle` migration path (and any future legitimate
cross-hotel blob need) without reopening the anonymous-write hole.

Recommended mechanism: reuse `mesh_auth_key_for_node`
(`crates/aiua/src/mesh.rs:65-90`) — the existing per-hotel-pair symmetric key
(config-stored or ECDH-derived via `derive_transport_shared_key`) already used
identically to authenticate execution-plane TCP traffic
(`execution_transport.rs:109-118`) and mesh beacon signing between the same
two hotels. Concretely:

- `POST /upload` and `GET /download/*` require an `X-Philotic-Mesh-Auth`
  header: `HMAC-SHA256(mesh_auth_key_for_node(peer), canonical(method, path,
  timestamp))`, with the same replay-window discipline the beacon path already
  uses (`REPLAY_WINDOW_SECS`, nonce tracking via the existing `NonceTracker`).
- The blob listener widens from loopback to the mesh-bound interface only
  (not `0.0.0.0`) — matching the pattern `membrane-mcp`
  (`crates/membrane-mcp/src/main.rs:136-146`) and `graph-intelligence`
  (`crates/graph-intelligence/src/server/mod.rs`) already use: bind loopback
  by default, widen only to the interface the perimeter tier actually
  requires, and refuse to widen without an auth mechanism attached. Those two
  services are the correct in-repo precedent this slice should mirror, not
  invent a third pattern.
- Add an aggregate storage quota (not just the existing 100MB/request cap) so
  even an authenticated, mesh-scoped peer can't disk-fill a hotel by mistake
  or compromise — ties directly to DEF-078 (a full disk wedges the hotel
  silently, no error).
- Correct the `ListenerDecl` and its persisted perimeter snapshot to reflect
  the new tier honestly once this lands — the whole point of Slice 1 was
  declaration accuracy; Slice 2 must not regress it by silently widening
  again without updating the declaration.

### Slice 3 — Execution plane connection-layer hardening

The execution port (`0.0.0.0:{execution_port}`, `main.rs:8542`) authenticates
*message content* correctly by default (`validate_execution_message`,
`execution_transport.rs:97-124`, HMAC + nonce via `mesh_auth_key_for_node`),
but the TCP *connection* itself is unprotected: `read_execution_message`
(`execution_transport.rs:90-94`) reads a 4-byte length header and allocates
that many bytes **before** any auth check runs, with no connection cap and no
read timeout. Any TCP peer that completes a handshake can claim up to ~4 GiB
and force an allocation attempt, and a fresh `tokio::spawn` +
`NonceTracker::open` (a SQLite open) happens per accepted connection
regardless of whether the frame ever authenticates. This is a distinct,
lower-but-real severity finding from the blob hole: it's a connection-level
resource-exhaustion surface, not a data-exposure one, and confirmed publicly
reachable in the same 2026-08-31 off-tailnet check (`:16468`).

Recommended: cap the claimed frame length before allocating, add a connection
accept limit and read timeout, and move the SQLite `NonceTracker` open out of
the per-connection hot path (open once, share across connections — it's
already keyed by peer, not by connection).

Separately: **no Ansible firewall rule exists for the execution port at all**
(`ansible/roles/philotic_hotel/tasks/main.yml` has rules for beacon, blob, and
the ONNX sidecar port — nothing for execution). Add one, gated the same way
the blob rule should be (see Slice 4) so it doesn't silently no-op.

### Slice 4 — Firewall parity and the global auth kill switch

Two smaller, standalone gaps found during this audit:

1. **The blob (and proposed execution) Ansible firewall rules silently no-op
   on hosts without `ufw` installed** — confirmed live on vps-jane. The
   playbook's own intended mitigation didn't take effect not because of any
   permission or policy block, but because the target host's package state
   made the guard condition false and `ignore_errors: yes` swallowed it
   silently. Either ensure `ufw` (or ship an `iptables`/`nftables` fallback
   rule set for hosts without it) as part of hotel provisioning, or add an
   explicit boot-time check that warns loudly (not silently) when the
   intended firewall rule couldn't be applied.
2. **`PHILOTIC_ENABLE_RUST_AUTH` is a single global env-var kill switch**
   (`main.rs:762-764`, defaults `true`) that, if set `false`, bypasses HMAC and
   nonce verification for every beacon message type on that hotel — not a
   per-message-type or per-peer control. `MESH_PKI_HOTEL_IDENTITY_PROPOSAL.md`
   already flags the equivalent `PHILOTIC_MESH_DEV_MODE` concern under its own
   Open Decisions ("must not compile into release builds for prod targets").
   Recommend the same treatment here: this flag should not exist in release
   builds, or should require an explicit, loudly-logged, non-default opt-out
   rather than a plain boolean env var indistinguishable from any other
   feature flag.

## Full Listener Inventory (this audit)

| Listener | Bind | Auth | Status |
|---|---|---|---|
| Blob HTTP (`blob.rs:45`) | `0.0.0.0` → `127.0.0.1` (PR #479) | None | **Fixed pending Slice 2 for cross-hotel restore** |
| Execution TCP (`execution_transport.rs:21`) | `0.0.0.0` | Message-layer HMAC (default on); connection layer unprotected | Slice 3 |
| Mesh beacon UDP (`beacon.rs:64`) | `0.0.0.0` | HMAC-PSK + Ed25519 join handshake; signed, not encrypted | Tracked by MESH_PKI_HOTEL_IDENTITY_PROPOSAL (S3–S6) |
| membrane-mcp (`main.rs:136-146`) | Loopback by default; perimeter-conditional widening | Bearer token | Correct precedent pattern |
| graph-intelligence (`server/mod.rs`) | Configurable | Refuses non-loopback bind without an auth token | Correct precedent pattern |
| philotic-web (`serve.rs:906`) | Loopback by default | Operator session / edge bearer gate, perimeter-tier-enforced | Sound |
| ONNX sidecar (`model-controller-onnx`) | `127.0.0.1:11435` by default | None on the handlers themselves | Safe only by default-loopback convention — fragile if `--sidecar-addr` is ever widened; recommend the same perimeter-conditional pattern as defense-in-depth |
| Ephemeral/outbound-only UDP sockets (perimeter probes, Discord voice, various `0.0.0.0:0` binds) | Various | N/A — send-only or `.connect()`'d | Benign, audited, no action needed |
| Startup self-test mocks (`main.rs:6116,7163,7474,7485`; membrane-telegram) | `127.0.0.1` | N/A | Not production listeners |

## Corrections to Prior Understanding

- The earlier working note that firewall mitigation was "blocked by a
  permission classifier" does not correspond to anything in this repository's
  history, code, or Ansible playbooks. The actual reason the intended `ufw`
  rule didn't take effect on vps-jane is documented above (Slice 4, item 1) —
  package state, not a policy block. Correcting this now so it doesn't
  propagate.
- "philotic-web is the only authz boundary" is no longer precise given this
  audit: `membrane-mcp` and `graph-intelligence` independently perform real
  authorization on their own surfaces. The accurate framing is that each
  listener owns its own boundary at a different layer — operator session
  (philotic-web), tool-call bearer token (membrane-mcp), message-content HMAC
  (execution plane, beacon) — and blob was the one surface with none at all
  until PR #479's bind fix and this proposal's Slice 2.

## Verification

Each slice should carry its own verification per the standard ladder
(`$verification-ladder`):

- Slice 1: test-green (already true for PR #479); watched-live-green once
  deployed, confirming the socket actually rejects a raw `curl` from a
  non-loopback source.
- Slice 2: test-green for the HMAC auth path, then watched-live-green
  exercising a real two-hotel `agent.deploy_bundle` migration end to end.
- Slice 3: a targeted smoke sending an oversized/garbage length header from an
  unauthenticated connection, confirming it's rejected/capped before
  allocation, plus a connection-count test.
- Slice 4: confirm the firewall rule actually applies (not just "task ok") on
  a representative Debian and non-Debian host, and confirm
  `PHILOTIC_ENABLE_RUST_AUTH=false` either fails to compile in release or logs
  loudly and persistently, not just once at boot.
