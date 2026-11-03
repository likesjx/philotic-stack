---
title: Native endpoint approval maintenance
doc_type: reference
domain: runtime-sessions
status: proposed
last_updated: 2026-11-03
related_docs: [architecture/NATIVE_MEMORY_CAPTURE_PROPOSAL.md, task.md]
---

# Purpose and boundary

Extending two existing native remember/recall approvals must not provision a new
guest, clear its PID, stop a protected child, or reconcile the HTTP listener.
`ProvisionMcpEndpoint` is a full provisioning operation and is not a safe hot
maintenance mechanism. `SetComponentActive(false)` is not a substitute: its
signal/state writes are not serialized with supervision.

`ExtendNativeMcpPreapprovals` is a **local OS-account administrative IPC** operation,
not a routed capability or MCP tool. The Unix socket's same-UID check remains the
security boundary. Legacy `Register` accepts identity/role strings; these are
context, not independent authentication of agents sharing that OS account. The
operation additionally requires registered role `operator` and exact stored owner
identity. The owner is rechecked in the atomic configuration snapshot. No new
account, remote authentication, delegation, or socket permission is introduced.

# Protocol

1. Read `GetMcpEndpointStatus`, including `config_sha256`. Inspect the exact owner,
   namespace-related tool definitions, destination, authentication and two finite
   native approvals. Obtain action-specific operator approval for the new expiry.
2. Send `extend_native_mcp_preapprovals` with `endpoint_id`,
   `expected_config_sha256`, `expires_at` and `approval_reference`. The reference
   is audit metadata, never authorization by itself. There is no caller owner,
   target, scope, tool, key or PID replacement field.
3. The hotel requires exactly distinct `native-memory.remember` and
   `native-memory.recall` rules, the same explicit native destination, finite
   increasing expiry no later than 24 hours from its clock. It preserves every
   other stored field. Endpoint and approval-index records must agree and are
   compared/replaced in one SQLite immediate transaction. Other backends fail
   closed unless they implement atomic node compare-exchange.
4. Only approval times/references/expiries and endpoint `updated_at` change.
   No guest or launch record is written. A dedicated push changes only the
   membrane approval table, with immutable configuration and monotonic-version
   checks; it does not rebind or restart anything.
5. Full provisioning mints `approval_incarnation` regardless of a caller value.
   Maintenance preserves it. Thus revoke/recreate with otherwise identical
   settings cannot accept delayed maintenance from the previous incarnation.
   Existing endpoints without this optional field remain readable; first full
   provisioning by the new hotel establishes it. Durable unknown fields are
   preserved, but typed membrane views do not claim unknown-field equivalence.

# Failure, retries and evidence

The transaction is the durable update's linearization point. A failed second
write rolls back the first. A stale digest, changed owner, inconsistent index,
unsupported adapter or invalid rules produces refusal without partial writes.
This is not a transaction over receiver grant files or issuer state.

`persisted`, `push_enqueued`, `lifecycle_changed:false` and
`effective_state_verified:false` deliberately distinguish storage from live
application. A lost push leaves the older shorter approval active. Re-read the
current digest and resend the same expiry/reference to enqueue reconciliation
without another durable write or version increment. Do not blindly retry an old
digest, decrease policy revisions, or assert a push is an application receipt.
Reordered older maintenance pushes are rejected; duplicate applied pushes are
idempotent. A revoked table rejects maintenance rather than recreating it.

Receiver/ingress grant revision synchronization remains separately governed and
may temporarily reject requests. This operation does not renew OAuth families,
receiver grants, certificates or user consent. It does not delete captured data,
receipts or persistent admissions at expiry. Backend write/result fences remain
the authority on live memory operations; endpoint preapproval is not a substitute.

# Qualification and rollout

Tests cover atomic storage failure/rollback, concurrent independent SQLite
connections, owner/index/digest drift, malformed/wildcard/unbounded rules,
out-of-order/duplicate/revoked/recreated membrane updates, real Unix IPC
authorization/refusal/reconciliation, and repeated updates while a real local
child remains under concurrent hotel supervision. Existing protected-generation
retirement tests remain required. Native Memory Capture CI runs the new hotel
tests and existing full Linux composition/package gates. Local macOS tests are
not Linux release or installed-service evidence.

Local review checkpoint: five storage/domain tests, two membrane tests and two
hotel IPC/actual-child tests passed. `cargo fmt --all --check`, outbound inventory
and affected core/client/membrane Clippy passed (existing non-denied warnings
remain). Independent review's owner-snapshot and endpoint-recreation findings
were corrected and regression-tested. Exact-head hosted checks are recorded in
the draft PR; they are not implied by these local results. Incarnation fencing
here covers maintenance messages, not every legacy full-config message ordering.

This draft installs nothing. Adoption needs qualified `aiua` and `membrane-mcp`
artifacts, pinned rollback copies and a separately approved hotel/process restart
window to replace running binaries. That installation can interrupt hotel guests;
it is not the previously contemplated single membrane hot update. Once installed,
the maintenance operation itself requires no restart. No website/Traefik change,
runner/database replacement, credential rotation or scope expansion is required
by this code. Exact installed hashes and an effective live approval check remain
deployment prerequisites. Never reuse an expired interruption approval.

No personal memory reads, production registrations, grant writes, installation,
service changes or new credentials were part of this implementation. Memory
bootstrap and telemetry are deferred under the explicit task restrictions; the
operator/parent owns rechecking before a memory-dependent decision.
