---
title: Perimeter Enforcement — Turn Observed Boundaries Into Enforced Ones
doc_type: proposal
domain: operator-control-plane
status: proposed
last_updated: 2026-10-05
tags:
- security
- ipc
- vault
- mesh
- hmac
- egress
- placement
related_docs:
- HOTEL_PERIMETER_TRUST_PROPOSAL.md
- PERIMETER_EGRESS_CONTROL_PROPOSAL.md
- OUTBOUND_INTEGRATION_FABRIC_PROPOSAL.md
- KEY_VAULT_PROPOSAL.md
- MESH_PKI_HOTEL_IDENTITY_PROPOSAL.md
- OPERATOR_IDENTITY_AND_DANGEROUS_ACTION_CEREMONIES_PROPOSAL.md
- BLOB_EXECUTION_PERIMETER_HARDENING_PROPOSAL.md
- IPC_DISPATCH_SPLIT_PROPOSAL.md
proposal_id: perimeter-enforcement
implements:
- aiua
- ansible-mesh-core
- perimeter-core
- philotic-primitives-mesh
implemented_by: []
active_seams:
- ipc-socket-hardening
- ipc-reserved-roles
- ipc-guest-capability-token
- vault-aad-binding
- mesh-seq-u64
- mesh-mac-v2
- placement-signed-stamps
- egress-policy-enforcement
- execution-plane-accept-allowlist
---

# Perimeter Enforcement — Turn Observed Boundaries Into Enforced Ones

Origin: Philotic Stack Atlas (2026-09-30), next seam #5. Facts re-checked
against `origin/develop` @ `f170cb88` on 2026-10-05.

## Problem

The perimeter is designed, classified and audited, but very little of it is
**enforced**.

| Boundary | Today | Where |
|---|---|---|
| **IPC identity (DEF-173)** | `Register` accepts any `{guest_id, role}` claim. The socket is never chmod'd. The peer is discarded at accept, and `peer_cred` is used nowhere. Any same-user process can claim `hotel.internal` and read the mesh Ed25519/X25519 private keys, whose `allowed_roles` is `["hotel.internal"]`. | `ipc.rs:3047-3053` (bind), `:3075` (accept), `:5112-5136` (Register), `:5367-5393` (GetSecret), `:14013`, `:14044`; `vault.rs:137-171` |
| **Config ACL** | `SetConfig` is guarded only for the `__mcp_` prefix. It can write `mesh_auth_key:<node>`, which **overrides** the ECDH-derived peer key, so a local process can pick the key that authenticates a peer. `GetConfig`, `RotateSecret`, `AddVaultEntry` and `ApplyAgentBundle` have no ACL. | `ipc.rs:5204`, `:5415-5433`, `:5435`, `:5444`, `:12406`; `aiua/src/mesh.rs:70-72` |
| **MCP owner check** | An unidentified caller passes `mcp_owner_identity_ok`, and the roles `operator`, `admin` and `desktop-membrane` are self-asserted. DEF-209 notes the provisioning scripts only work because of this. | `ipc.rs:4815-4833` (+ 9 callers) |
| **Vault ciphertext** | AES-GCM without AAD, so a ciphertext isn't bound to its `secret_ref` (part of DEF-175). | `vault.rs:188-189` |
| **Mesh MAC (DEF-187)** | HMAC covers `msg_id‖seq‖timestamp‖payload` only. `version`, `src_node`, `dest_node`, `msg_type` and `total` are not covered. One symmetric pair key serves both directions, which allows reflection. `dest_node` is never checked. The sender signs a u64 `seq` but sends it as u32 (`as u32`), so every batch fails once seq exceeds 2^32. | `ansible-mesh-core/src/authz.rs:46-100`; `mesh_dispatcher.rs:316,323`; `aiua/src/mesh.rs:59-63` |
| **Placement gossip (DEF-171)** | A role or transport home applies if its sender-supplied timestamp is newer, within a 300 s future-skew cap. Records carry no originator signature: any authenticated peer can claim any role's home. | `ansible-mesh-core/src/placement_sync.rs:71-157` |
| **Egress** | `HotelEgressGateway::new(vec![], …)` means allow-all. `CheckEgress` is advisory and reached only from a model tool, with a model-supplied `agent_id`. Real enforcement exists only inside `egress-http-runner`, per IntegrationBinding. 33 direct callers are inventoried by lint, not enforced at runtime. | `aiua/src/main.rs:7992-7997`; `perimeter-core/src/egress.rs:100-117`; `ipc.rs:3374-3428`; `egress-http-runner/src/lib.rs` |
| **Execution plane (DEF-175)** | Frames are capped at 32 MiB with 64 permits (fixed in #572), but the listener binds `0.0.0.0`. That allows 64 × 32 MiB of pre-auth buffering. | `execution_transport.rs:24-34,180-183`; `main.rs:7946` |
| **Host firewall** | Ansible ufw rules are skipped when ufw is absent (`ignore_errors`). vps-jane has no ufw and INPUT ACCEPT. There is no default deny. | `ansible/roles/philotic_hotel/tasks/main.yml:194-229,356-364` |
| **Provider keys in files** | The DB copy is vaulted (`migrate_plaintext_provider_api_keys`), but `mesh-config.json` still holds plaintext keys, rendered from `templates/mesh-config.json.j2:3-5` on vps and maintained by hand on the Macs. | `main.rs:1335-1395` |

**Accurate threat statement.**
- What this fixes:
  - guest impersonation and cross-role reads on a hotel;
  - local tampering with mesh keys;
  - reflected or relabelled mesh frames;
  - placement hijack by a compromised peer;
  - pre-auth resource exhaustion;
  - unaudited egress.
- What it does not fix: malware running as the hotel's user, which can read
  `context.db`, the vault master key and any token file. `DEFECTS.md` already
  says this.

## Goal

Every boundary the hotel claims to have is enforced in code, has a test, and
can be checked with `phil doctor`. Rollout must never partition the mesh or
break the operator's scripts.

### Non-goals

- OS-level per-guest sandboxing for egress. That is listed as a follow-up
  (P8).
- Defending against a compromised hotel user account.
- The full operator ceremony proposal (`OPERATOR_IDENTITY_AND_DANGEROUS_ACTION_CEREMONIES`).
  P3 introduces only the `operator.token` primitive it will need.

## Slices

### P1 — Quick hard stops (S, one PR)

1. **Socket hardening:**
   - after bind, `set_permissions(0o600)` on the socket and `0o700` on its
     parent directory;
   - drop the `/tmp/philotic-<hotel>.sock` fallback (`main.rs:1003-1019`) and
     refuse to start without a profile directory or an explicit
     `PHILOTIC_HOTEL_SOCKET`;
   - the client default `/tmp/philotic-aiua.sock` (`philotic-client/src/lib.rs:3372`)
     stays only for tests.
2. **UID check at accept:**
   - change `Ok((stream, _))` to inspect `stream.peer_cred()` and reject when
     `uid != geteuid()`;
   - tokio 1.50 `net` provides this on both Linux (SO_PEERCRED) and macOS
     (getpeereid / LOCAL_PEERPID).
3. **MCP owner check:** in `mcp_owner_identity_ok`, an unidentified caller
   now returns `false`.
4. **Migrate the scripts first, in the same PR.** These five register as
   `hotel.internal` or `hotel` today and must change to
   `register{guest_id:"<owner>:<suffix>", role:"operator"}` with
   read-until-reply framing. This also closes DEF-209.
   - `provision-agent-frontdoor.py:227`
   - `provision-lifegraph-mcp.py:194`
   - `provision-mcp-bearer.py:77`
   - `register-frontdoor-upstreams.py:159`
   - `sync-muninn-vault-tokens.py:116`
5. **Mesh `seq` to u64.** Widen `BeaconMessage.seq` to `u64` and remove the
   `as u32` (`mesh_dispatcher.rs:323`). JSON numbers stay wire-compatible
   until values exceed 2^32, which is exactly what this prevents.
6. **Execution-plane accept allowlist (DEF-175 remainder):**
   - accept only from known peer `mesh_host` IPs or `100.64.0.0/10`;
   - bind to the tailnet interface when one is configured.

**Tests:**
- An IPC e2e test where a foreign-uid connection is refused. Simulate it via
  an injected credential checker trait, because CI can't switch users.
- `mcp_owner_identity_ok(None, ..) == false`.
- A seq of 2^32 + 1 round-trips through sign/validate.
- An accept-allowlist unit test.

**Rollout:** each hotel is independent, with no cross-hotel skew. Deploy the
scripts and the hotel together.

**Status (2026-10-07): implemented** on `codex/perimeter-p1`, test-green.
- **Socket.** The socket is `0600`. Its directory becomes `0700` only when
  the hotel user owns it and it is not sticky, so `/tmp` and other shared
  directories are left alone. The vps unit's `RuntimeDirectoryMode` is now
  `0700` to match.
- **Startup.** A hotel with neither `PHILOTIC_PROFILE` nor
  `PHILOTIC_HOTEL_SOCKET` refuses to start. All three supervisors already set
  one:
  - vps-jane: `PHILOTIC_HOTEL_SOCKET` in the systemd unit;
  - mac-jane: `PHILOTIC_PROFILE` in the LaunchAgent;
  - mbp-jane: `PHILOTIC_PROFILE` from `push-homebrew-remote.sh`.

  Smoke scripts and the `just` dev recipes that relied on the fallback now
  pass the same `/tmp/philotic-<hotel>.sock` path explicitly.
- **Peer uid.** The check compares against the socket owner's uid and fails
  closed. On vps-jane every other caller already runs as `philotic`
  (`runuser`/`sudo -u philotic`), and nothing else uses `/run/philotic`.
- **MCP owner.** `mcp_owner_identity_ok(None, _)` is `false`. The provisioning
  scripts already register as `operator` (step 4), so hotels can deploy in
  any order.
- **`seq`.** It is now `u64`.
- **Execution plane.** The accept allowlist admits loopback, `100.64.0.0/10`,
  `fd7a:115c:a1e0::/48` and known `mesh_host` IPs, cached for 60 s.
  **Deferred:** binding to the tailnet interface. The listener still binds
  every interface; the allowlist and vps-jane's nftables filter cover it.
- **Live proof owed after deploy:**
  - `ls -l` shows the socket as `0600` on all 3 hotels;
  - `sudo -u nobody socat - UNIX:<sock>` is refused;
  - the provisioning scripts still work.

### P2 — Reserved roles and config ACL (S)

1. `Register` with role `hotel.internal` or `hotel` over IPC is refused. The
   in-process users (`mesh.rs:45`, `main.rs:1268`, `ipc.rs:13981`) don't use
   IPC.
2. **Config key families.** Add a `ConfigKeyClass` classifier:
   - **Hotel-only** (deny any IPC writer):
     `mesh_auth_key:*`, `*_private_key_ref`, `*_api_key_ref`,
     `__hotel_perimeter__`, `egress_policy:*`.
   - **Operator** (requires `role == operator` *and*, after P3, a verified
     operator token):
     `muninn_*`, `vault_registry`, `authority_hotel`, `placement*`.
   - **Guest-owned:** anything prefixed with the caller's `guest_id`.
   - **Public read:** everything else stays readable.
   - `GetConfig` denies reads of the hotel-only class.
3. `RotateSecret`, `AddVaultEntry` and `ApplyAgentBundle` require the operator
   class.
4. **Vault AAD (B4):**
   - encrypt with `Payload { msg, aad: secret_ref.as_bytes() }`;
   - decrypt with AAD first, then fall back to no-AAD for legacy rows, and
     re-encrypt those on read;
   - add `phil vault reseal` to migrate everything eagerly.

**Tests:**
- Extend `get_secret_returns_vault_secret_for_authorized_guest` (`ipc.rs:~27155`)
  and `vault_round_trips_secret_with_role_policy` (`vault.rs:562`).
- New: a hotel-only key write is refused; a legacy no-AAD ciphertext decrypts
  and is resealed.

### P3 — Spawn-time guest capability token (M)

1. GuestManager (`guest_manager.rs:153-220`) generates 32 random bytes per
   spawn and passes them as `PHILOTIC_GUEST_TOKEN`. The hotel keeps
   `blake3(token) → (guest_id, role, child_pid)` in memory and rotates it on
   respawn.
2. `GuestIdentity` gains `#[serde(default)] token: Option<String>`.
   `philotic-client` reads `PHILOTIC_GUEST_TOKEN` automatically in `connect_at`.
3. `Register` binds identity **from the token**. A claimed `guest_id` or
   `role` that disagrees with the token is refused.
4. **Defense in depth (advisory):**
   - compare `peer_cred().pid()` with the tracked `child_pid`;
   - otherwise check the executable path (Linux `/proc/<pid>/exe`, macOS
     `proc_pidpath`) is under `PHILOTIC_BIN_DIR`.
   Wrapper scripts and grandchildren make this advisory only: log a mismatch,
   don't refuse.
5. **Operator token:**
   - `aiua` writes `<profile>/operator.token` (0600) at first start;
   - `phil` (philotic-web; `integration.rs:112` plus 14 `management`
     registrations) and the scripts read it and register as `operator` with
     the token;
   - an `operator` claim without the token is downgraded to an unprivileged
     role.
6. **Soft, then enforce:**
   - release N logs each unauthenticated register per role (heal tag
     `ipc_unauthenticated_register`);
   - release N+1 enforces for every role except an allowlisted
     `management-readonly`;
   - the flag `PHILOTIC_IPC_ENFORCE_GUEST_TOKEN` flips the default.

**Tests:**
- A register with a matching token is accepted.
- A register with a mismatched role is refused.
- With no token, soft mode logs and enforce mode refuses.
- Existing `PhiloticClient` e2e tests need a test-token path (an
  `ipc_env_guard` helper).

**Coordinate with `IPC_DISPATCH_SPLIT_PROPOSAL.md`:** P2 and P3 edit Register,
`config_vault` and `mcp_endpoint`. Land those family extractions first, or
hold them until P3 merges.

### P4 — Mesh MAC v2 with a dual-verify rollout (M, 2–3 deploys)

1. **Directional key:**
   `k_dir = HKDF(pair_key, info = "philotic-mesh-mac-v2|{src}->{dst}")`.
   This kills reflection.
2. **MAC input:** a canonical, length-prefixed encoding of
   `version=2, key_id, msg_id, src_node, dest_node, msg_type (stable string),
   seq u64, total, timestamp, payload`.
3. **`key_id`:** add `#[serde(default)] key_id: Option<String>` to
   `BeaconMessage`. First verify there is no `deny_unknown_fields` anywhere on
   the wire types.
4. **Receivers enforce `dest_node == local_node_id`**, or `broadcast` for
   heartbeat and roster, on both planes:
   - UDP: `beacon.rs:181-216`;
   - TCP: `execution_transport.rs:201-225`.
5. **Phases:**
   - **A:** receivers accept v1 or v2; senders send v1. Deploy to all three
     hotels.
   - **B:** add `mesh_mac_versions: [1,2]` to `NodeCapabilities` (with the
     `features` list from `MESH_DELIVERY_GUARANTEES_PROPOSAL.md` L5). Senders
     emit v2 to a peer once its heartbeat advertises 2.
   - **C:** a sticky per-peer latch. After a valid v2 frame from peer P,
     reject v1 from P. The `version` field is the downgrade lever, so the
     latch must persist (config `mesh_mac_latched:<peer>`).
   - **D:** `mesh_mac_min_version=2` once all three hotels report v2. Compile
     the `PHILOTIC_ENABLE_RUST_AUTH` kill switch out of release builds
     (BLOB_EXECUTION_PERIMETER_HARDENING slice 4.2).
6. Remove the `mesh_auth_key:*` override from guest reach. P2 already does
   this; P4 also logs a warning if the override exists at boot.
7. Fix stale docs that mention `PHILOTIC_MESH_PSK`, which ansible renders but
   no code reads: `ARCHITECTURE.md`, `RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md`,
   `crates/aiua/README.md`. Delete it from `secrets.env.j2`.

**Tests:**
- `authz.rs:240-320` with a v1/v2 matrix.
- Reflection: an A→B frame relabelled B→A is refused.
- `dest_node` mismatch.
- seq above 2^32.
- The latch.
- `tests/parity_harness.rs:34`.

**Rollout risk:** the hotels deploy independently, so never skip a phase.
mac-jane can sleep through phase B adverts; the latch handles late joiners.
Watch for `mesh_event_undecodable` and `mesh_auth_failed` heal tags between
phases.

### P5 — Signed placement records (M, depends on P4)

1. **Signed stamps.** `HotelStateSyncRoleHome` (`heartbeat.rs:88-94`) and
   `MembraneTransportHomeRecord` (`graph.rs:811-828`) gain `set_by` (node id),
   an `hlc` `(physical_ms, logical, origin)` stamp, and `sig`: an Ed25519
   signature by `set_by`'s mesh identity key (`ensure_mesh_identity`,
   `ipc.rs:13988`) over the canonical record.
2. **Authority rule.** Accept a record only if `set_by` is the record's
   **current home**. Exception: the record is a ceremony-backed relocation
   whose CLOSE state names the new home.
3. **Route `set_home` through the home.** `role.set_home` / `transport.set_home`
   issued on a non-home hotel are forwarded as a mesh request to the current
   home, so only the home emits the new record. This also settles the
   "own orchestrator on another hotel" conflict noted in DEF-171.
4. Replace the wall-clock skew check with HLC ordering.
5. **Compat:** for one release, unsigned legacy records apply only if the
   sender is the current home.
6. Keep the existing invariant: placement never authorizes secret release
   (`seal_transport_secret`, `role_materialization.rs:2549-2623`).

**Tests:**
- Extend `placement_sync.rs:205-336`.
- A forged non-home claim is refused.
- A relocation CLOSE record is accepted.
- HLC tie-break.
- `only_the_roles_home_may_place_it` (`role_materialization.rs:4179`).

### P6 — Egress policy enforcement (M)

1. **Load policies.** Read `EgressPolicy` per tier from graph config
   `egress_policy:<tier>`, seeded by `aiua load` from `mesh-config.json`.
   Add suffix host matching (`*.telegram.org`) to `perimeter-core/src/egress.rs:107,117`.
2. **Default for the Public tier: `AllowWithAudit`** in the first release.
   Every decision is counted per inventory id from
   `docs/architecture/outbound-egress-inventory.json`.
3. **Make it binding.** `egress-http-runner` and `GovernedHttpService` call
   the gateway `check` as a **deny overlay**: a policy deny beats a binding
   allow.
4. **Bind `CheckEgress.agent_id`** to the registered (P3) identity, not the
   model argument (`philote/src/tool_exec.rs:1838-1856`).
5. **Audit counters on named-exception callers:** model-router providers,
   membranes, `mcp_upstream` HTTP. Expose them in `phil doctor egress.audit`.
6. After two weeks of audit data, flip the Public tier to `Deny` with
   explicit allows for the 33 inventoried callers.

**Tests:**
- Extend `perimeter-core/src/egress.rs:173-300` (suffix matching, overlay
  precedence).
- Runner tests (`egress-http-runner/src/lib.rs:764-993`).

### P7 — Host and config hygiene (S, ops)

1. **Firewall:**
   - vps-jane: ship nftables rules (or install ufw) with default deny for the
     hotel's mesh, blob and exec ports (16466–16468), except from
     `100.64.0.0/10`;
   - remove `ignore_errors`, so the task fails loudly when the firewall tool
     is missing;
   - Macs: rely on P1's accept allowlist plus the tailnet bind, and document
     that pf is optional.
2. **Plaintext keys in `mesh-config.json`:**
   - `aiua load` accepts `secret://` references and `*_file` / env
     indirection;
   - after vaulting, it warns, and with `--scrub` rewrites the file with refs;
   - change `templates/mesh-config.json.j2` to emit refs.
3. **DEF-089:** redact the Telegram bot token from reqwest error URLs before
   logging.
4. **Blob upload quota (DEF-078 risk):** an aggregate byte quota and per-guest
   rate on loopback `/upload` (`blob.rs:41`).

### P8 — OS-level egress (L, follow-up, not scheduled)

- Linux: a per-guest network namespace, or an nftables cgroup match.
- macOS: a `sandbox-exec` profile that denies `network-outbound` for guests
  without an exception.

Scope it after P6's audit data shows which guests really need direct network.

## Ordering

```
P1 ─► P2 ─► P3
P1 ─► P4 (A→B→C→D across deploys) ─► P5
P6 (independent; P3 improves step 4)
P7 (independent ops)
```

## Verification ladder

| Slice | Test-green | Smoke / live proof |
|---|---|---|
| P1 | unit + IPC e2e | `ls -l` socket 0600 on 3 hotels; a foreign-uid connect refused (manual `sudo -u nobody socat`); scripts work |
| P2 | ACL + AAD tests | `phil vault reseal` on each hotel; `SetConfig mesh_auth_key:x` refused |
| P3 | token tests | soft-mode release shows zero unexpected `ipc_unauthenticated_register`; then enforce |
| P4 | v1/v2 matrix | each phase deployed to 3 hotels with zero `mesh_auth_failed` spikes; reflection drill refused |
| P5 | placement tests | forged-claim drill from mbp refused; relocation R6 (WATCH_LIVE_BURNDOWN W5) still passes |
| P6 | egress tests | 2 weeks of audit counts, then a deny flip with no unexpected denials |
| P7 | — | `nft list ruleset` default deny; external port scan of the vps shows the mesh ports closed off-tailnet |

## Definition of done

- DEF-171, DEF-173, DEF-175, DEF-187, DEF-089 and DEF-209 are closed.
- The egress Public tier is enforced, with every exception from the inventory.
- `phil doctor` has `ipc.socket-mode`, `ipc.unauthenticated-registers`,
  `mesh.mac-version` and `egress.audit`.
- The mesh runs MAC v2 at min-version 2 on all three hotels.

## Operator decisions needed

1. P3 enforcement date, which ends the soft-mode window.
2. P6: when to flip the Public tier from AllowWithAudit to Deny.
3. P7: nftables on vps-jane, or install ufw?
4. P5: is "only the current home can move a role" acceptable, given it
   requires the home hotel to be awake to hand off? The ceremony covers the
   planned case. For a dead home hotel, add an operator break-glass override.
