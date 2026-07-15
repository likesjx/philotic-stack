---
doc_type: defect-tracker
status: active
last_updated: 2026-07-14
---

# Defects and Technical Debt

Tracked defects and known technical debt. Each entry carries status, severity, points estimate, and PR/commit cross-references. Rebuilt 2026-07-03 from session history (2026-04 → 2026-07): the ledger had not been updated since 2026-03-31 while ~30 real defects were found and fixed in the field.

**Status values**: `open` | `fixed`
**Severity values**: `high` | `medium` | `low`

## Ledger

| ID | Title | Severity | Status | Pts | Found | Fixed by |
|---|---|---|---|---|---|---|
| DEF-001 | Hotel-scoped capability advertisement off-by-one (inactive tool-runner counted) | low | open | 1 | 2026-03-10 | — |
| DEF-002 | Abstract tool storage methods unwired in sqlite storage (legacy `ansible` crate Gap 3) | medium | fixed | 2 | 2026-03-10 | port to `ansible-mesh-core` (methods live in `domain/mod.rs`) |
| DEF-003 | `aiua` binary test target could not compile/run | medium | fixed | 2 | 2026-03 | mock stubs + fallback fix (pre-04) |
| DEF-004 | Multi-tool synthesized response not surfacing | high | fixed | 2 | 2026-03-30 | superseded by cognitive loop v2 + resilient loop (PR #58, #59); no recurrence since |
| DEF-005 | `aiua` test suite hangs on 10 desktop-membrane/e2e tests | medium | fixed | 3 | 2026-06-22 | codex/aiua-test-unhang |
| DEF-006 | Watchdog evicts WaitingModel turns instead of escalating fallback tier | high | fixed | 1 | 2026-06-27 | PR #93 (2e33928) |
| DEF-007 | Voice routing lost on checkpoint restore (`agent_profile` not serialized) | medium | fixed | 1 | 2026-04-29 | PR #60 |
| DEF-008 | `IpcResponse` untagged-enum ordering: `UserProfileData` swallowed `MemoryConfig` | medium | fixed | 1 | 2026-04-29 | wrapper struct + `deny_unknown_fields` (2026-04-29) |
| DEF-009 | Gemini no-content SSE fell back to un-timed batch call — 27-minute hang | high | fixed | 2 | 2026-05-04 | PR #59 (batch fallback removed, 8s idle timeout) |
| DEF-010 | Gemini keep-alive "thinking-drip" resets SSE idle timer — unbounded stream | high | fixed | 1 | 2026-05-20 | f7f8715 (wall-clock cap); 816dadd (forwarder deadlock) |
| DEF-011 | membrane-mcp orphan storm — ghost processes fight one lease | medium | fixed | 2 | 2026-05-29 | 8abef40, 18cbf14 |
| DEF-012 | `hotel.status`/`hotel.logs`/`role.set_home` had no execution route | low | fixed | 1 | 2026-06-01 | `is_local_agent_tool` additions |
| DEF-013 | String `completed_at` killed whole session snapshot via `?` propagation | medium | fixed | 1 | 2026-06-01 | warn+skip in `list_session_turns` + data surgery |
| DEF-014 | Unbuffered OOB hotel broadcasts broke `send_request` (Telegram seats dark) | medium | fixed | 1 | 2026-06-04 | 63f7072 |
| DEF-015 | Paracrine replies silently dropped (`reply_to_guest_id` = membrane seat); delegate.merge unrouted | high | fixed | 2 | 2026-06-08 | d0b969e, 542d967 |
| DEF-016 | `mesh_events` table grew unboundedly (84k stale events found) | medium | fixed | 1 | 2026-06-12 | a2ed50d |
| DEF-017 | Cron jobs to dormant role incarnations silently dropped (bare `target_role`, no materialization) | high | fixed | 3 | 2026-06-19 | PR #80 |
| DEF-018 | `UserProfileData` classified as ignorable push — guest request hang | medium | fixed | 1 | 2026-06-19 | PR #81 (4687ae4) |
| DEF-019 | Agent responses parked ledger-only when `active_incarnation_id` guest not live | high | fixed | 2 | 2026-06-22 | PR #84 + #85 |
| DEF-020 | LifeGraph cross-hotel TaskInvoke parked forever (90s watchdog on every call) | high | fixed | 2 | 2026-06-19 | PR #86 |
| DEF-021 | Conversational-goal gate: `"ok"` substring-matched inside `"look"` — zero tools for turn | medium | fixed | 1 | 2026-06-24 | PR #87 (ab1ce23) |
| DEF-022 | Zombie `running` turns accumulate after restart storms (64 found on bjork) | medium | fixed | 2 | 2026-06-27 | PR #91 |
| DEF-023 | Role handoff delivered zero context and never auto-executed | high | fixed | 2 | 2026-06-27 | PR #92 |
| DEF-024 | `brain` role incarnation loops infinitely on mac-jane, never returns reply | medium | open | 3 | 2026-06-27 | — |
| DEF-025 | Boot-time fallback mesh port persisted as advertised port — hotel silently diverges from peers | medium | open | 2 | 2026-07-01 | peer-side mitigation only (94edd08) |
| DEF-026 | Provider secrets leaked into Muninn config payload | high | fixed | 1 | 2026-07-01 | PR #95 (bf5bee6) |
| DEF-027 | ElevenLabs voice synthesis and Telegram streaming broken | high | fixed | 2 | 2026-07-02 | PR #98 (c5e6263) |
| DEF-028 | Telegram poll lease lost on network flap — seats dark up to 600s | high | fixed | 2 | 2026-07-02 | PR #99 (648acf9) |
| DEF-029 | Approval inline buttons never resolved parked turns (300s timeout, 7 occurrences) | high | fixed | 1 | 2026-07-02 | PR #99 (1b86a45) |
| DEF-030 | `/role` to current role triggered infinite self-handoff loop | high | fixed | 2 | 2026-07-02 | PR #101 + #102 |
| DEF-031 | TTS replies delivered as music cards instead of Telegram voice notes | low | fixed | 1 | 2026-07-02 | PR #102 (36e73c2) |
| DEF-032 | `HotelStateSync` UDP broadcast EMSGSIZE every 30s (real ceiling: macOS maxdgram 9216 + 4x int-array wire inflation) | medium | fixed | 3 | 2026-07-03 | PRs #104/#107/#109 + fleet flip `PHILOTIC_BEACON_PAYLOAD_B64=1` (2026-07-04, watched-live-green: 0 EMSGSIZE post-flip; vps receives mac roster) |
| DEF-033 | `FailTask` never updates persisted `session_turn` status (record-keeping gap) | low | open | 1 | 2026-06-22 | — |
| DEF-034 | router-listener crashes on startup — missing `router_listener.config` DB key | medium | open | 1 | 2026-06-04 | — |
| DEF-035 | `push-homebrew-remote.sh` hand-starts launchd-managed hotels, orphaning them from supervision (stale `active_pid` rows then block re-bootstrap) | medium | fixed | 1 | 2026-07-06 | codex/deploy-launchd-logs (launchd-aware restart: detect label by pattern, clear `hotels.active_pid`, kickstart/bootstrap; hand-start only when no service) |
| DEF-036 | `aiua.log`/`aiua.err.log` unbounded growth — launchd `StandardOutPath` never rotates (61MB on 2026-07-06; 952MB historic on mbp-jane) | medium | fixed | 1 | 2026-07-06 | codex/deploy-launchd-logs (`scripts/install-log-rotation.sh`: newsyslog drop-in, 50MB/keep 5/compressed, run per-push, graceful without sudo; Linux hotels use journald — untouched) |
| DEF-037 | Literal substring `"life.observe"` in a cron prompt hijacked the direct-command shortcut, short-circuiting the whole chartered model pass and writing junk OpenLoop/Signal nodes | medium | fixed | 1 | 2026-07-08 | PR #169 (codex/direct-observe-guard) — shortcut now gated on task origin (`cron_job_id`/`transport=cron`/`source=cron-ticker`/paracrine signal never take the parser path) |
| DEF-038 | Vault root-key resolution order was context-dependent (keychain-first): launchd/ssh contexts silently fell through to the file key while GUI shells resolved the keychain key, so the same hotel used different keys depending on how it was started — bricked gemini/elevenlabs twice (2026-07-04, 2026-07-08) | high | fixed | 1 | 2026-07-08 | PR #170 (codex/vault-key-source-determinism) — deterministic env -> file -> keychain order regardless of execution context |
| DEF-039 | `push-homebrew-remote.sh` probe captured `rc=$?` after an `if` with no `else` (always 0) — every genuinely-new binary (first hit: `model-controller-anthropic`) misclassified as unreachable, aborting the push mid-stream and leaving alphabetically-later binaries (incl. `philote`) stale | high | fixed | 1 | 2026-07-08 | PR #171 (codex/push-probe-exit-code) |
| DEF-040 | `ProviderConfigs::load` hard-failed the entire provider-config refresh on the first undecryptable vault secret, taking down ALL providers instead of just the affected one (one stale `openai_api_key` row broke Gemini turns) | high | fixed | 1 | 2026-07-08 | PR #172 (codex/provider-refresh-isolation) — per-provider key/oauth fetch isolated; a failed fetch degrades only that provider to unconfigured |
| DEF-041 | Turn-level failures (provider errors, watchdog evictions, fallback-ladder exhaustion) terminated silently instead of feeding the self-heal queue | medium | fixed | 2 | 2026-07-08 | PR #173 (codex/turn-failure-heal-intake) |
| DEF-042 | Tool-result deliveries (e.g. `life.*` invokes) poisoned session placement provenance, causing 6/6 forensically-traced turns to die at the 90s watchdog because the tool's own result was misread as the session's persisted delivery target | high | fixed | 2 | 2026-07-08 | PR #174 (codex/tool-delivery-provenance) — only genuine agent-turn deliveries may update `agent_runtime_provenance` |
| DEF-043 | Telegram sent two identical replies per turn: cumulative `partial_reply` drafts converge on the final text, `editMessageText` 400s "message is not modified", and the silently-treated-as-failure path fell back to a fresh `sendMessage` without deleting the stale draft | medium | fixed | 1 | 2026-07-08 | PR #177 (codex/telegram-double-reply) — "not modified" treated as success; stale drafts deleted before any fallback send |
| DEF-044 | `ConfigureRole` / `role.create_or_update` unconditionally reset `fallback_tiers: Vec::new()` on every call with no way to set a ladder via IPC — every reconfigure silently wiped DB-edited fallback ladders (e.g. mac-jane's orchestrator losing its `model.openrouter` tier) | high | fixed | 1 | 2026-07-08 | PR #179 (codex/configure-role-ladder) — optional `fallback_tiers: Option<Vec<String>>`, wire-compatible; `None` preserves, `Some` sets after validation |
| DEF-045 | `philotic-client` IPC has no request/response correlation: a `tokio::time::timeout`-abandoned `send_request` leaves its reply queued on the stream, permanently desyncing the connection one frame — every subsequent request reads its predecessor's response (live symptom on agent-jane: `SyncApartment` receiving `sync_session_index`'s `ConfigData`). Philote wraps dozens of `send_request` calls in external 5s timeouts; any one firing after the write poisons the connection until process restart. membrane-mcp built its own correlation (d3f403c) but the SDK never got it | high | fixed | 3 | 2026-07-10 | PR #229 (codex/ipc-response-correlation) — `send_request_with_timeout` + stale-frame skip-counter (OOB broadcasts excluded from stale-consumption); 31 call sites migrated; wire-compatible. mbp-jane still needs the redeploy |
| DEF-046 | mbp-jane's reported "heal-dispatcher recurring-restart instability" was misdiagnosed at the guest level: `aiua.err.log` on mbp-jane shows ~9.6k untimestamped `Error: Hotel 'mbp-jane' is already running with PID <N>. Stop that instance before starting another.` (the `crates/aiua/src/main.rs` startup guard, ~line 7411), including 8076 hits against one long-lived PID and 1522 against its successor — evidence of a repeated *second* `aiua --hotel mbp-jane` launch attempt colliding with a genuinely-running instance, roughly every ~10s during the worst stretch (2026-07-10, 1532 boot attempts in one day). Every attempt (successful or refused) first logs "Hotel booting with N seeded guest(s)" — which re-lists heal-dispatcher — before the PID guard runs, so guest/heartbeat churn from these collisions plausibly reads as "the dispatcher keeps restarting" from the outside, when the dispatcher's own PID only actually changes on a genuine full hotel restart (confirmed once, 2026-07-11T23:47:24Z, clean boot through to a fresh heal-dispatcher PID). Root cause of the *duplicate launcher itself* is still unidentified: no crontab, no matching `launchctl list` entry, and mbp-jane has no `com.philotic.aiua.mbp-jane` job loaded at all (consistent with S1's "nohup, not supervised" finding) — likely a manual/ad-hoc retry loop rather than a registered service. mbp-jane's aiua process is not running at all as of this investigation (2026-07-12T01:19Z) | medium | resolved | 2 | 2026-07-11 | S2 (codex/substrate-s2-heal-the-healer) root-cause pass — no fix landed; needs S1 (launchd KeepAlive) to close the supervision gap, plus identifying and killing whatever issues the duplicate launch attempts **RESOLVED 2026-07-12 (live verification):** the duplicate launcher was launchd itself — `com.philotic.aiua.mbp-jane` IS bootstrapped in the gui domain (invisible to ssh-session `launchctl list`, which caused the "no job loaded" misread), and its KeepAlive retried every ~10s against a manually-started (nohup) aiua holding the instance lock. Storm ended Jul 10 15:57 (err.log untouched since) when the nohup instance died; since then exactly one aiua runs as launchd's own child, and launchd auto-restored the hotel unattended at 2026-07-12T01:20Z (the very window the S2 investigation read as "down"). Supervision invariant now holds; no code fix needed beyond S2's watchdog |
| DEF-047 | `phil` CLI hotel-IPC commands (`autonomy`, `heal`, `keys`, `component`) call `socket_path("aiua")` with the literal string as the hotel name — under `PHILOTIC_PROFILE` this resolves to `<profile>/aiua-aiua.sock`, but hotels create `aiua-<hotel>.sock` (e.g. `aiua-mac-jane.sock`), so every profile-based hotel gets "No such file or directory". Workaround (live-verified): unset the profile and pass `PHILOTIC_HOTEL_SOCKET=<socket path>`. Fix: thread the resolved hotel name (as `start.rs`/`status.rs` do) or glob `aiua-*.sock` in the profile dir | medium | open | 1 | 2026-07-12 | — |
| DEF-048 | Test runs recorded via the MCP `graph_record_test_run` tool do not surface in `phil graph green`, while runs recorded via REST `POST /api/test-run` do (live-verified: REST-recorded 1198/1198 shows green; six MCP-recorded runs from the same evening show `none`) — the two write paths create different TestRun node/edge shapes | low | open | 1 | 2026-07-12 | — |
| DEF-049 | `phil doctor` `logs.rotation-missing` check still looks for the newsyslog drop-in and warns even when the `com.philotic.logrotate` copytruncate LaunchAgent (the current, correct mechanism — newsyslog was retired because rename+bzip2 strands launchd writers) is installed and rotating | low | open | 1 | 2026-07-12 | — |
| DEF-050 | Role-addressed replies broadcast through every Telegram bot seat: a turn with no `final_reply_guest_id` (cron-originated turns never carry one) resolves its reply target to role `membrane` only, and aiua's `deliver_inbound_task` delivers a guest-less task to ALL role subscribers — each seat then posts to the same DM `chat_id` (identical under every bot token), so the operator sees one message per bot. Live-hit 2026-07-13 7:40a ET: aria's evicted dev-brief turn watchdog notice arrived from all 4 mbp-jane bots | medium | fixed | 2 | 2026-07-13 | PR #266 — seat-ownership filter on the session id's terminal agent segment, fail-open for non-Telegram shapes. Deployed mbp-jane + mac-jane 2026-07-13 |
| DEF-051 | Cron-originated turn's model responses silently swallowed — the turn rides the watchdog to a 600s eviction. Forensic (mbp-jane 2026-07-13, first cron fire on the Jul-12 18:47 deploy ≈ PR #260): 7:30a dev-brief turn dispatched to gemini; the response (tool_call `hotel.status`, `model_result: null`) reached aria's turn loop in 1.3s (`action peek` logged) and vanished with NO further log; 7:35a WaitingModel escalation to openrouter, response back in 7.5s, dropped identically; 7:40a CatchAll evicted at 600s. Neither provider hung. The only no-log drop paths in `handle_model_response` are empty `session_id`/`turn_id` and the no-active-turn arm (probe intercept ruled out — probe ids are `probe-<uuid>`; stale-turn mismatch warns). NOT a fresh regression: the identical 11:40:02Z eviction fired on Jul 10, 11, 12 AND 13 — every fire since the dev-brief cron was created, on the pre-#218 binary and the current one alike; the cron turn's model responses have never been consumed. Also hits some live turns (agent-jane evictions Jul 12 00:09/00:39 ×3, Jul 13 14:06Z). Silent-drop instrumentation (PR #272 warn/info on all three drop arms) deployed to mbp-jane philote 2026-07-13 17:51Z — the next 11:30Z cron fire names the guilty guard in the log. DEF-050's fix live-validated at the Jul-13 14:06Z eviction: exactly one seat sent the notice (was 4) | high | open | 3 | 2026-07-13 | — |
| DEF-052 | Deploy scripts accepted a build tree missing origin/develop commits — a stale-tree `push-homebrew-remote.sh` run silently reverts merged, already-deployed fixes fleet-wide. Live-hit 2026-07-14 13:42Z: a #274-era push from a worktree predating PR #266/#272 reverted the membrane seat filter and the DEF-051 instrumentation on mbp-jane; the 13:58Z aria eviction fanned out through all 4 bots again until a follow-up push from origin/develop restored them (14:25Z) | high | fixed | 1 | 2026-07-14 | `scripts/deploy-freshness-check.sh` sourced by `push-homebrew-remote.sh` + `deploy-mac-jane.sh`: hard abort when HEAD lacks origin/develop (lists missing merges; `PHILOTIC_DEPLOY_ALLOW_STALE=1` overrides; dry-run warns), plus warn-only checks for dirty trees and build artifacts older than HEAD |

---

## Open defects — detail

### DEF-001: `local_capability_advertisements_include_hotel_scoped_incarnations` off-by-one

**Found**: 2026-03-10 · **Seam**: active-membrane-routing

`tool-runner` is counted in hotel-scoped incarnation advertisements but is seeded as inactive. The test expects the active set only, producing an off-by-one failure. Pre-existing; does not affect runtime behavior since `tool-runner` is unused. Test still present at `crates/aiua/src/main.rs`.

### DEF-005: `aiua` test suite — 10 hung tests, 2 unrelated failures on develop HEAD

**Found**: 2026-06-22 · **Seam**: aiua-test-infra

`cargo test -p aiua` on unmodified develop (`d2478e8`) hangs indefinitely (0% CPU) on 10 tests: the `desktop_membrane_*` lease/status/target tests, `discord_lease_injects_membrane_binding`, and both `e2e_*_round_trip` tests. Reproduced in a clean detached worktree and single-threaded with one test alone — not parallel contention, not cache corruption. With those skipped, `emit_task_falls_back_to_orchestrator_when_active_incarnation_is_unregistered` and `default_guest_seed_injects_hotel_socket_env` fail order-dependently. Prime suspect for the hangs: tests bind `(dispatcher_tx, _dispatcher_rx)` and never drain the receiver, so any path sending more than channel capacity of `LedgerCommand`s blocks forever on `dispatcher_tx.send(...).await`. Blocks using `cargo test -p aiua` as an automated gate; sweeps work around it via `--skip` filters and live-fire verification.

**Fixed** (`codex/aiua-test-unhang`, 2026-07-03). The dispatcher-channel suspect was real but secondary; four distinct causes stacked:
1. **Client swallowed lease acquire/renew replies (the true hang)** — `PhiloticClient::is_expected_response` expected `DesktopMembraneLeaseStatus` for `AcquireDesktopMembraneLease`, but the server replies `DesktopMembraneLease`, which is also listed in `is_ignorable_push` — so the real reply was skipped as an OOB broadcast and `send_request` read forever. Only the 3 desktop-lease tests truly hung; the other 7 "hung" tests were queued behind them on the shared `ipc_env_guard()` mutex. Fixed by mapping Acquire/Renew requests to their actual reply variants (including the untagged-enum collapse of `DiscordGatewayLease` → `TelegramPollLease`).
2. **Undrained test dispatcher channels** — production's durable-writer thread drains `dispatcher_rx`; test fixtures never did, so chatty tests could block on `dispatcher_tx.send().await` at channel capacity. Fixed with a shared `ipc::test_dispatcher_channel()` helper that drains into an unbounded channel; all fixture sites in `ipc.rs`/`cron_ticker.rs` converted.
3. **e2e tests predated the response-route resolver** — their `model_response` emissions carried no `reply_guest_id`/`return_route` (production model-router always includes both), so the resolver fell back to the session's unregistered `primary_agent_id` and parked the reply. Tests updated to mirror production payloads.
4. **Real routing black hole + stale seed count** — `emit_task_falls_back_to_orchestrator_when_active_incarnation_is_unregistered` exposed that an active incarnation unknown both locally and to the mesh was silently dropped; EmitTask now falls back to the session's live orchestrator when the mesh reroute finds no home (production fix in `ipc.rs`). `default_guest_seed_injects_hotel_socket_env` was stale since PR #94 added the `model.openrouter` guest (11 → 12).

Full suite: 219/219 green in ~21s.

### DEF-024: `brain` role incarnation loops infinitely on mac-jane

**Found**: 2026-06-27 · **Seam**: role-incarnation

After the mesh-auth-key mismatch between mbp-jane and mac-jane was fixed, `agent-astrid:brain` materializes cross-hotel on mac-jane but loops infinitely against Gemini and never replies back to Telegram — likely missing vault/mempalace skills plus an unwired cross-hotel return path. Mitigated by deactivating the guest and reverting Astrid to orchestrator posture; `/role brain` remains unsafe until the loop and return path are fixed. Related but distinct from DEF-030 (the `/role` self-handoff loop, which is fixed).

### DEF-025: Boot-time fallback mesh port persisted as advertised port

**Found**: 2026-07-01 · **Seam**: mesh-transport

`resolve_runtime_ports` (`crates/aiua/src/main.rs:627`) falls back to an alternate port cluster on a boot-time collision and writes the fallback back into the hotel's `graph_nodes` record — while peers keep sending to the originally advertised port. One transient collision permanently diverged mac-jane from its peers (days-long silent mesh-receive blackout; brain TaskInvokes never arrived). Manual fix: reset `hotel:mac-jane` ports + full `launchctl bootout`/`bootstrap`. Peer-side heartbeat reconcile (94edd08) mitigates but the fallback port should not be persisted as the advertised port, or aiua should retry the canonical cluster on later boots.

### DEF-032: `HotelStateSync` broadcast exceeds UDP datagram ceiling (EMSGSIZE)

**Found**: 2026-07-03 · **Fixed**: 2026-07-04 · **Seam**: mesh-transport

**Resolution**: three layers — chunk `model_profiles` across datagrams (PR #104, a5b6f55), measure the FULL signed wire size against macOS's real `net.inet.udp.maxdgram` = 9216 ceiling instead of inner-byte proxies (PRs #107/#109), and a dual-read `BeaconPayload` (legacy int-array OR base64) with the sender flipped to base64 fleet-wide via `PHILOTIC_BEACON_PAYLOAD_B64=1` (~1.33x wire inflation vs ~4x — a realistic 20-guest roster could never fit under 9216 in the legacy encoding). Watched-live-green 2026-07-04: 0 EMSGSIZE on mac-jane post-flip; vps-jane journal shows `Hotel state sync from mac-jane: 30 guests, 3 agents` — the first Mac roster ever to cross the mesh.

Adding the `model_profiles` catalog to the `HotelStateSync` payload (on `codex/model-catalog-sync`) pushed the UDP broadcast over the 65507-byte ceiling — `Message too long (os error 40)` every 30s, dropping guests/agents routing state propagation to peers. Benign for local serving, degrades cross-hotel state sync. Fix `a5b6f55` (chunk oversized broadcasts under the ceiling) is committed on `codex/model-catalog-sync` but not yet PR'd/merged — was queued behind a WAN outage on the dev machine.

### DEF-033: `FailTask` never updates the persisted `session_turn` record

**Found**: 2026-06-22 · **Seam**: session-ledger

`IpcRequest::FailTask` (`crates/aiua/src/service/ipc.rs`) carries only `{error, reason}` with no session/turn envelope, so `extract_session_envelope` bails and the persisted turn stays `running` forever. Record-keeping gap only — in-memory watchdog recovery works and nothing gates new dispatch on the stale status — but it pollutes the DB with false zombie turns (see DEF-022) and misleads diagnostics.

### DEF-034: router-listener startup crash — missing `router_listener.config`

**Found**: 2026-06-04 · **Seam**: guest-lifecycle

The router-listener guest crashes on startup when the `router_listener.config` key is absent from the context graph. Pre-existing regression, observed repeatedly in mbp-jane's heal queue. Guest should seed a default config or degrade gracefully instead of crash-looping.

---

## Fixed defects — detail

### DEF-002: Abstract tool storage methods unwired (legacy `ansible` crate Gap 3)

`upsert_abstract_tool` / `get_abstract_tool` / `list_abstract_tools` were defined but not wired in the old `ansible` crate's sqlite storage, breaking its build. Resolved by the port: the methods are implemented in `crates/ansible-mesh-core/src/domain/mod.rs` and the workspace compiles.

### DEF-003: `aiua` binary test target could not compile/run

Missing `OperatorTargetView` import, incomplete `WorkflowSkillRecord` mock impls, and a missing `hotels["default"]` fallback in `extract_context_graph_entries` prevented `cargo test -p aiua` from running at all. All three fixed; 110 tests passed at close. (The later, separate hang is DEF-005.)

### DEF-004: Multi-tool synthesized response not surfacing

Re-entry loops completed tool iterations but the final synthesized summary never reached the user. No isolated fix commit; the cognitive loop was rebuilt in PR #58 (cognitive loop v2) and PR #59 (resilient loop, all 6 slices) and the failure has not reproduced since 2026-04. Closed as superseded.

### DEF-006: Watchdog evicted WaitingModel turns instead of escalating

When a model call hung past `WAITING_MODEL_SECS = 300`, the philote watchdog evicted the turn ("I seem to have gotten stuck") instead of escalating to the next fallback tier. Underlying trigger: silent IPC drop between model-router and philote leaves no error signal, so the watchdog is the only backstop. An earlier "fix" (951e113, 120s→300s bump) only delayed the give-up. PR #93 (2e33928) branches on `WaitingModel` in `evict_timed_out_turns` and calls `advance_turn_to_next_fallback_tier`; the 600s CatchAll ceiling remains a hard evict. Deployed to mac-jane 2026-06-29.

### DEF-007: Voice routing lost on checkpoint restore

Checkpoint snapshots didn't serialize `agent_profile`, so restored sessions got a default `VoiceResponsePolicy` (provider=None) and always fell back to ElevenLabs, ignoring the agent bundle's `"onnx"` setting. Fixed in `ensure_session_loaded` by applying `default_agent_profile` after `from_checkpoint`, mirroring the new-session path (shipped with PR #60).

### DEF-008: `IpcResponse` untagged-enum ordering swallowed `MemoryConfig`

`UserProfileData { Option, Option }` sat before `MemoryConfig { Option }` in the `#[serde(untagged)]` enum; an all-optional variant matches any JSON object, so `{ "config_json": ... }` deserialized as an empty `UserProfileData` and philote ran without memory. Fixed by extracting a `UserProfileDataPayload` wrapper with `#[serde(deny_unknown_fields)]`. Standing rule: every new all-optional variant needs a `deny_unknown_fields` wrapper; `MemoryConfig` stays last.

### DEF-009 / DEF-010: Gemini streaming hangs (batch fallback; keep-alive drip)

Two related hangs. (1) Gemini streaming sometimes returns SSE with no text (safety block, quota); the no-content path fell back to the batch endpoint which had no timeout — observed 27-minute hang. PR #59 removed the batch fallback (bail with `streaming_timeout`, routed to tier escalation) and added an 8s per-chunk idle timeout. (2) Gemini can drip keep-alive SSE bytes every ~7s, resetting the idle timer forever without progress; f7f8715 added a hard wall-clock cap on the whole SSE session (`STREAMING_TOTAL_SECS`). 816dadd later added connect/send timeouts in the streaming forwarder to close a related deadlock. Do not re-add the batch fallback; the correct escape is tier escalation.

### DEF-011: membrane-mcp orphan storm

Stale membrane-mcp processes accumulated and fought over a single lease (~5 lease attempts/sec observed with 7 orphans). 8abef40 made ghost reclamation kill stale PIDs directly; 18cbf14 eliminated the remaining accumulation path in guest-manager.

### DEF-012: Local-agent tools with no execution route

`hotel.status`, `hotel.logs`, `role.set_home` were in the abstract tool catalog and toolset profiles but missing from `is_local_agent_tool()` in `crates/aiua/src/service/ipc.rs`, so they silently had no route. Rule: any tool handled in philote's `execute_local_agent_tool` match MUST be added to `is_local_agent_tool`.

### DEF-013: Malformed `session_turn` record killed snapshot composition

An old binary wrote `completed_at` as a JSON string; `list_session_turns` used `?` per record, so one bad row failed the entire `compose_session_snapshot`, silently blocking toolset injection for the whole session. Hardened to warn+skip in `ansible-mesh-core/src/domain/mod.rs`; bad rows repaired via SQL.

### DEF-014: OOB broadcasts broke request/response on the guest socket

Hotel-initiated OOB broadcasts (`MuninnStatus`, `NetworkState`) arriving mid-`send_request` were consumed as the response, wedging membrane-telegram seats. 63f7072 buffers OOB push messages in `philotic-client::send_request` instead of misinterpreting them.

### DEF-015: Paracrine reply drops and unrouted delegate.merge

`reply_to_guest_id` for "self" whispers was set to the membrane seat, so specialist (brain) replies went brain → membrane → silently dropped instead of returning to the orchestrator (d0b969e). Separately, `delegate.merge` had no execution route in paracrine specialist turns (542d967). Confirmed e2e: Astrid → delegate.whisper → brain → synthesized Telegram reply.

### DEF-016: `mesh_events` unbounded growth

The append-only event ledger never pruned; 84,904 stale events from a renamed hotel had to be deleted by hand before a2ed50d added retention.

### DEF-017: Cron-fired tasks silently dropped for dormant role incarnations

Beacon's daily Chronos heartbeat failed silently for days: `target_role` was stored bare (`"orchestrator"`) instead of the routing key (`role:agent-beacon:orchestrator`), and even a correct key found zero subscribers when the role guest was dormant. PR #80 added `normalize_cron_target_role` at registration and park-and-materialize via `ensure_role_materialized` in `CronTicker::fire` (the first attempt wrongly reused the cross-hotel helper — caught in review). Live-verified on vps-jane: park → spawn → register → flush → delivery.

### DEF-018: `UserProfileData` treated as ignorable push

`UserProfileData` in `is_ignorable_push` caused a hang in the client push path. PR #81 removed it and added a guard test against re-adding it.

### DEF-019: Outbound responses parked ledger-only for non-live incarnations

Inbound delivery checked whether `session.active_incarnation_id` was a live registered guest and fell back to the orchestrator; outbound (`infer_response_target_guest_id_for_agent_task`) did not — replies resolved to an unregistered guest, logged "stays ledger-only for now", and were swallowed until the 300s watchdog fired. PR #84 mirrored the liveness fallback; live-firing showed PR #84 alone was insufficient because the fallback guest tripped `targets_base_agent` re-derivation in `resolve_agent_route`, which PR #85 made response-like payloads skip. Bit every hotel independently (vps-jane Beacon, then mac-jane Bjork within minutes); all three patched 2026-06-22.

### DEF-020: LifeGraph cross-hotel calls parked forever

Every `life.*`/`graph.query` call from mac-jane/mbp-jane routes cross-hotel to `vps-jane:life-graph-runner` — a hotel-level utility guest with no `role_incarnation` record. The cross-hotel dispatch path assumed the target was a dormant role guest, logged "No role incarnation found for cross-hotel guest; cannot materialize", and parked the task forever; the 90s WaitingTool watchdog then evicted the turn. 100% reproducible. PR #86 guards `deliver_event_envelope_or_park` to skip events addressed to another node. Live-verified: cross-hotel round trip in ~1.46s, no park.

### DEF-021: Conversational-goal gate zeroed all tools on "look"

`looks_like_conversational_goal` used plain `str::contains` against a filler list including `"ok"` — a substring of "look"/"took"/"book" — so natural requests like "take a look at the lifegraph" tripped the conversational heuristic and `project_tools_for_turn` returned an empty tool list for the whole turn with no fallback. PR #87 added word-boundary matching. Verification lesson: the standard smoke driver names the tool explicitly and bypasses this gate — regression checks must use natural phrasing.

### DEF-022: Zombie running turns after restart storms

Stuck `status=running` turns accumulated across hotel restarts (64 found on bjork, oldest from May), partly downstream of DEF-033. PR #91 added a startup sweep plus a heal-dispatcher scan that proactively repairs them.

### DEF-023: Role handoff arrived with no context and never executed

`handle_handoff_bundle` read only `bundle.working_summary` — always `None` from `handoff.to_role` — so receiving roles had zero context; and nothing triggered execution, so the role sat idle until another operator message. PR #92 synthesizes the summary from `goal + context_excerpt` and pushes a synthetic `role_directed_task` to `pending_drains` when `active_goal` is set.

### DEF-026: Provider secrets in Muninn config

The hotel's Muninn config payload included provider secrets from the vault registry. PR #95 filters them out.

### DEF-027: Voice synthesis and Telegram streaming broken

A keys/streaming regression broke ElevenLabs synthesis and Telegram streamed replies. PR #98 (c5e6263) restored both.

### DEF-028: Telegram poll lease lost on network flap

Marginal WiFi (reachability probe TCP-connects 1.1.1.1:53 with a 3s timeout) flapped the seat offline; missed renewals let the lease TTL lapse, and the client treated "lease lost to None" as contested — entering a never-resetting backoff that left seats dark up to 600s per flap. PR #99 (648acf9) re-acquires the lease in place.

### DEF-029: Approval buttons never resolved parked turns

philote's approval resolver only acted on `/approve`/`/deny` text, but membrane turned inline-button callbacks into plain "Telegram callback action: X" chat — so approval-parked turns always hit the 300s watchdog (7 occurrences since May 29). PR #99 (1b86a45) maps approve→`/approve`, deny→`/deny`, trust→`/approve`.

### DEF-030: `/role` self-handoff infinite loop

Issuing `/role` for the role a session was already in triggered an infinite role-swap loop. PR #101 (1b90cfa) hardened the handoff path with a loop guard + self-heal; PR #102 (6f992f7) makes `/role` to the current role a no-op.

### DEF-031: TTS replies rendered as music cards

Telegram TTS replies were sent as audio files (music cards) instead of voice notes. PR #102 (36e73c2) switches delivery to `sendVoice`.

---

## Technical debt

One line each; severity + pointer. These are structural, not behavioral defects.

- **Two drifting Telegram gateways** — medium — `crates/membrane` (legacy gateway) and `crates/membrane-telegram` implement overlapping Telegram logic and drift apart; fixes like DEF-028/029 land in one and not the other.
- **Four hand-rolled lease clients** — medium — membrane-telegram, membrane-discord, membrane-mcp, and the desktop membrane each reimplement lease acquire/renew/backoff (`lease.rs` per crate); DEF-028's bug class exists ×4.
- **park_and_materialize duplication + cron dual-delivery race** — medium — `park_and_materialize_local_role` vs `park_and_materialize_role_philote` in `crates/aiua/src/service/ipc.rs`, and a fired cron event is consumed by BOTH `CronTicker::fire` delivery and `deliver_event_envelope_or_park` reacting to the same `AppendLocal` — which one wins varies per fire (observed live, session 18).
- **Guest supervisor off by default** — RESOLVED (codex/supervisor-default-on) — supervisor loop now runs by default with flap protection (max 5 respawns per guest per 10-min window; breach marks `supervision_state:respawn_budget_exhausted` in the graph and pushes a heal-queue entry). Opt out with `PHILOTIC_DISABLE_GUEST_SUPERVISOR=1`; legacy `PHILOTIC_ENABLE_GUEST_SUPERVISOR` is deprecated (truthy = no-op, falsy = opt-out). Awaiting 24h watched-live soak on next fleet deploy.
- **Unbounded paracrine chains** — medium — the cross-hop budget (`charge_paracrine_hop`, default 5 hops / 900s) is charged ONLY in the `EnrichedToolResult` routing branch; `ReflectiveReEntry`/`CognitiveReEntry`/`MemoryEnrichment`/`DatasourceInjection`/`PriorityReEntry` all re-enter via `handle_user_message` on a fresh turn with `paracrine_hop_count` reset, so async whisper→response→whisper chains through those variants have no cross-turn bound (each turn is still iteration-capped + watchdog-bounded). Partially mitigated by the reflective-surface fix (paracrine PR — a reflection reply now surfaces to the user instead of silently ping-ponging, so a runaway loop produces visible output a human can stop). A proper per-chain (session-scoped) budget needs live validation and is deliberately deferred rather than added blind to the hot path.
- **On-demand role-philote churn** — low/medium — a whispered role that isn't already materialized (e.g. `theoretician`) is spawned on demand, but the process clean-exits (`exit status: 0`) and is respawned by the supervisor every ~15–20 min, so it is never warm; every whisper to a cold role pays a materialization + first-model-call penalty, and if that first model call stalls the specialist turn rides its 600s ceiling to a watchdog eviction (observed mac-jane 2026-07-10, the only real paracrine trace in the logs). Root cause (why the on-demand role philote exits cleanly) needs live investigation.
- **Primitives crates unused** — RESOLVED (2026-07-06, PR #147 `codex/crate-cleanup`) — the five empty `philotic-primitives-{agent,data,hotel,model,tool}` scaffolds (zero reverse dependencies, never workspace members) were deleted along with the retired `graph-runner` crate; `philotic-primitives-mesh` is the only surviving primitives crate and stays (consumed by `ansible-mesh-core`).
- **agent-graph-runner dead directory** — low — `crates/agent-graph-runner` has no `Cargo.toml` (not a buildable package, not a workspace member) but the directory and its `src/main.rs` still live on disk, superseded by `agent-datasource`; deletion pending. Historically caused orphaned-PID cleanup work from stale built binaries.
- **membrane-discord unshipped island** — low — `crates/membrane-discord` is a full gateway (voice bridge included) that is never seeded or deployed; it still costs build time and carries its own lease client.
- **membrane-mcp dead dispatch.rs** — low — `crates/membrane-mcp/src/dispatch.rs` is declared (`mod dispatch;`) but nothing references it; dead code to delete or wire in.
- **Mesh-key / OIDC decrypt mismatch on macOS hotels** — medium — operator-flagged 2026-07 (mac-jane / mbp-jane): the underlying keychain-vs-file root-key nondeterminism is fixed by DEF-038 (PR #170, deterministic env -> file -> keychain order), but OIDC provider-secret decrypt failures in the desktop auth path have not been independently re-verified live under the new deterministic order — treat as open until confirmed on a fleet deploy.
- **mesh-config.json plaintext provider keys** — medium — `mesh-config.json` / `mesh-config.example.json` still carry provider API keys in plaintext for local bootstrap, ahead of the vault-backed `provider_keys`/`*_secret_ref` path that hotels use once running; hygiene gap between first-boot config and steady-state vault storage, no tracked remediation slice yet.
