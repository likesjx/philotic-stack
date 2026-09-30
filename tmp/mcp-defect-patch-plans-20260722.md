# Patch plans — philotic-stack deployed-code defects (drafted 2026-07-22)

**ref_read (all defects):** `origin/develop` @ `43bcbc7c96480323c5ea16e116affb927a2e2eb6` (2026-07-21, "Merge PR #345"). `git fetch` failed (no network at analysis time); re-fetch before implementing. Working tree was on a different branch — all reads were `git show origin/develop:PATH`.

Sequencing note: defects A, B, D each have a client/philote side and a hotel side deployed as separate processes — land the client-side fixes first (A's dual-shape content, B's philotic-client ack arm); each is backward compatible with the unfixed peer.

---

## DEFECT A — membrane-mcp endpoint envelope mismatch + ignored target_node

Root cause:
1. Empty captures: the config-driven (FieldMap) dispatch path builds content `{"action","payload","target_kind","target_id"}` at `crates/membrane-mcp/src/server.rs:508-514`, but philote's `handle_context_capture` (`crates/philote/src/memory_integration.rs:1519-1525`) parses `content.get("args")` — the legacy shape `{"tool","args"}` emitted only by the legacy route path (`server.rs:629-633`). `args` absent → `unwrap_or_default()` → empty capture.
2. target_node ignored: `target_parts()` in `crates/membrane-mcp/src/transform.rs:94-105` matches `McpRouteTarget::Philote { agent_id, .. }` — discarding `target_node`. The transform path sets `raw_transport` without a `target_node` key and `envelope.target_node: None` (`server.rs:518-528`). `build_inbound_request` (`crates/membrane/src/runtime.rs:254-261, 311-323`) only emits cross-hotel `EmitTask` when a target_node is set — otherwise local `CreateTask`.

Fix side: membrane-mcp (emit both shapes), plus a defensive philote fallback.
- `transform.rs`: `InboundResult` gains `pub target_node: Option<String>`; `target_parts()` returns it for Philote targets.
- `server.rs` (~499-529): content json adds `"tool": inbound.action, "args": inbound.payload` alongside existing keys; `raw_transport` adds `"target_node": inbound.target_node`.
- `memory_integration.rs:1519-1525`: fall back `v.get("args").or_else(|| v.get("payload"))`.
- Extend `dispatch_tests.rs` envelope-shape test.

Test: unit as above + live call against a node-pinned target; verify non-empty capture in target vault. Risk: low; additive content keys. Cross-hotel EmitTask path becomes live for MCP — verify mesh auth first.

---

## DEFECT B — RefreshMemoryConfig hangs; vault tokens not reloaded

Root cause: the hotel replies (`crates/aiua/src/service/ipc.rs:3022-3057` sends `IpcResponse::MuninnStatus`) but the client misclassifies: `read_matching_response` (`crates/philotic-client/src/lib.rs:3015-3023`) treats MuninnStatus as a push (`is_push_message`, lib.rs:3119-3131) before the expected-response check; no `(RefreshMemoryConfig, MuninnStatus)` arm exists in `is_expected_response` (lib.rs:3074-3117) — same pattern as documented DEF-005.

Token non-reload (two gaps): philote caches `self.muninn_config` once at boot (`memory_integration.rs:1200-1245`); MuninnStatus broadcast handler (`philote/src/turn_loop.rs:821-830`) only flips availability. And the hotel's `IpcServer.muninn_config` is loaded once at boot (`ipc.rs:1349`, `main.rs:7649`) — RefreshMemoryConfig never re-runs `memory::load_muninn_config`.

Fix:
- Client ack: in `read_matching_response`, return a push frame if it is the expected reply; add `(RefreshMemoryConfig, MuninnStatus)` to `is_expected_response`.
- Real reload: make hotel `muninn_config` swappable (ArcSwapOption); RefreshMemoryConfig re-runs load + swap + broadcast; philote MuninnStatus handler refetches config when available.

Test: client unit test mirroring lib.rs:4365; hotel integration (rotate token → refresh → fetch shows new token); live `memory.fix` returns in seconds. Risk: low (ack), medium (reload — use ArcSwap).

---

## DEFECT C — heal-dispatcher restart loops, panic misclassification, no re-bump

Root cause (`crates/heal-dispatcher/src/main.rs`):
1. `execute_action` (1015-1067) treats GUEST_NOT_FOUND as generic `restart_failed`; `is_session_like` (999-1011) misses `heal:`, `model-catalog-sync`, `mcp-perplexity-uat` → infinite retry.
2. `rule_classify` (912-914) fires "panic" on any substring `"panicked"` including quoted text.
3. Recurrence key `(pattern_tag, guest_id)` (`recurrence.rs:82-104`) never aggregates unique `heal:ephemeral:<uuid>` ids → open items never re-bumped.

Fix:
- Add terminal outcome `restart_skipped_not_registered` for GUEST_NOT_FOUND / COMPONENT_INACTIVE.
- Extend `is_session_like` with `heal:` prefix; add `is_unrestartable_service` set `["model-catalog-sync","model-oracle","hotel"]`; post-gate in `process_row`: downgrade restart_guest → escalate for session-like/unrestartable ids.
- Tighten panic classifier to `"panicked at"`.
- Normalize recurrence key: collapse `heal:ephemeral:<anything>` → `heal:ephemeral`.

Test: unit in existing module (main.rs:1115+) + recurrence aggregation test. Risk: low, dispatcher-local.

---

## DEFECT D — revoke_mcp_endpoint leaves `{active:true, config:null}` + guest re-materializes

Root cause (`crates/aiua/src/service/ipc.rs`, RevokeMcpEndpoint at 8600-8680):
1. Revoke writes literal string `"null"` tombstones (`set_config_value(&config_key, "null")`, ipc.rs:8644-8648); `GetMcpEndpointStatus` (8681-8703) parses `"null"` → `Some(Value::Null)` → `active: true`.
2. Shutdown push mis-addressed: `deliver_inbound_task(inboxes, local_node_id, &guest_id, None, ...)` (8657, and provision fan-out 8470/8497) — third param is target_ROLE; inboxes are keyed by role `"mcp-membrane"`, so zero subscribers.
3. Guest never torn down: no materializer reclaim; `set_guest_active(false)` result swallowed (8667-8670) and silently no-ops on miss; GuestRecord stays active → `materialize_all` (`guest_manager.rs:667-728`) respawns on every restart.

Fix:
- Replace tombstones with `graph.remove_config_value` (`domain/mod.rs:1304`); keep ownership check treating missing key = unowned.
- Fix push addressing: `deliver_inbound_task(inboxes, local_node_id, "mcp-membrane", Some(&guest_id), ...)` in revoke AND provision fan-out.
- Tear down guest: signal → materializer reclaim (mirror RestartComponent kill path ipc.rs:11920-11935) → `graph.remove_guest`; log failures.
- Harden `GetMcpEndpointStatus`: `.filter(|v| !v.is_null())`; one-time cleanup of existing `"null"` rows (perplexity-capture on vps-jane).

Test: provision→revoke→status inactive + guest absent; inbox delivery test; live on vps-jane with two restarts. Risk: medium (provision fan-out addressing changes a live path — currently a provable no-op, can only improve).
