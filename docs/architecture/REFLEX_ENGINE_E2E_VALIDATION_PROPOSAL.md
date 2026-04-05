---
id: proposal:reflex-engine-e2e-validation
kind: proposal
status: planning
domain: voice-routing
merged_prs: [49, 50, 51, 48, 47]
---

# Reflex Engine E2E Validation

End-to-end validation of the three-layer reflex engine and related membrane
infrastructure merged to `develop` 2026-04-03.

## What was merged

| PR | Branch | Summary |
|----|--------|---------|
| #47 | `codex/telegram-provider-final` | Bounded restart backoff for Telegram provider |
| #48 | `codex/membrane-discord` | Whisper protocol: paracrine dispatch, lookaside reflex, membrane attribution |
| #49 | `codex/routing-reflex` | `apply_configure` expansion (10 media/voice paths) + `skills/routing-reflex/SKILL.md` |
| #50 | `codex/reflex-engine` | Three-layer reflex engine in `crates/philote/src/reflex.rs`; 12 new unit tests |
| #51 | `codex/reflex-membrane-binding` | `inject_membrane_binding` in `aiua/src/service/ipc.rs` |

## Seams under test

| Seam | File | What to verify |
|------|------|----------------|
| `inject_membrane_binding` | `crates/aiua/src/service/ipc.rs` | Bundle has binding after lease grant |
| `fetch_agent_profile` → `reflex_context` | `crates/philote/src/runtime.rs` | Deserialization of `MaterializationContext` |
| `apply_reflex_materialization` | `crates/philote/src/runtime.rs` | Fires on both fresh + checkpoint-restored sessions |
| `apply_reflex_ingress(VoiceDialogue)` | `crates/philote/src/runtime.rs` | Transcription policy set on voice turn entry |
| TTS failure → `ReflexEvent::TtsFailure` | `crates/philote/src/runtime.rs` | Fallback to text on synthesis failure |
| `apply_configure` routing paths | `crates/philote/src/session.rs` | All 10 paths accept/reject correctly |

## Test Scenarios

### Scenario 1 — Telegram membrane binding injection
**Pre:** Hotel running, membrane-telegram registered and holding a lease.

1. Membrane calls `AcquireTelegramPollLease { agent_id: "<philote-id>" }`
2. Fetch `GetConfig { key: "__agent_bundle__:<philote-id>" }`
3. Assert: `bundle_json.reflex_context.membrane_bindings` contains `{ kind: "telegram", membrane_guest_id: "<membrane-guest-id>" }`
4. Re-acquire (idempotent): assert no duplicate entries in bindings array

### Scenario 2 — Discord membrane binding injection
Same as Scenario 1 with `AcquireDiscordGatewayLease`.
Assert: binding with `kind: "discord_text"`.

### Scenario 3 — Philote picks up bindings at materialization (Layer 2)
**Pre:** Lease already granted (Scenario 1 or 2 complete).

1. Restart philote (or `just start-agent` fresh spawn)
2. Observe logs: `apply_reflex_materialization()` fires
3. Assert: if `discord_text` binding present → `media_routing_policy.voice_action = "transcribe"` and `voice_response_policy.mode = "auto"` applied to session
4. Assert: checkpoint-restored session also re-evaluates (not skipped)

### Scenario 4 — Voice dialogue ingress reflex (Layer 3)
**Pre:** Philote session active.

1. Send `voice.dialogue` task to philote
2. Assert: `handle_voice_dialogue` log shows reflex ingress fired
3. Assert: `media_routing_policy.voice_action` is `"transcribe"` in session state
4. Assert: model-router receives transcription request, not raw media analysis

### Scenario 5 — TTS failure fallback (Layer 3)
**Pre:** Session with `voice_response_policy.mode = "on"`, invalid/unavailable TTS provider.

1. Trigger a response that would produce voice output
2. Assert: TTS call fails, `ReflexEvent::TtsFailure` fires
3. Assert: `voice_response_policy.mode` transitions to `"off"`
4. Assert: response is delivered as text (not dropped)

### Scenario 6 — `agent.configure` routing paths
1. Agent calls `agent.configure("media_routing_policy.voice_action", "transcribe", "set")` → assert applied
2. Agent calls `agent.configure("media_routing_policy.forward_media_to_model", "false", "set")` → assert applied
3. Agent calls `agent.configure("voice_response_policy.mode", "auto", "set")` → assert applied
4. Agent calls `agent.configure("voice_response_policy.mode", "invalid", "set")` → assert error returned
5. Verify all 10 paths (5 `media_routing_policy.*`, 5 `voice_response_policy.*`)

### Scenario 7 — Routing reflex skill: Discord voice join
1. Invoke routing-reflex skill standard reflex:
   - `agent.configure("media_routing_policy.voice_action", "transcribe", "set")`
   - `agent.configure("voice_response_policy.mode", "auto", "set")`
2. Assert: session state reflects both changes
3. Assert: subsequent `voice.dialogue` task routes to transcription

### Scenario 8 — Telegram provider restart backoff (PR #47)
1. Kill the Telegram provider process mid-operation
2. Assert: restart log shows bounded backoff (not tight spin loop)
3. Assert: provider recovers and re-acquires lease within expected window

### Scenario 9 — Discord whisper protocol (PR #48)
1. Connect Discord membrane, join voice channel
2. Assert: paracrine dispatch routes PCM frames correctly
3. Assert: membrane attribution tracks which membrane sent which frame
4. Assert: lookaside reflex fires on re-entry after transcription

## Prerequisites

```bash
just start-aiua               # Hotel with bjork profile
just start-gateway            # membrane-telegram
just start-model              # model-router (needs valid API keys)
# For Discord scenarios:
# membrane-discord with valid bot token + voice channel
```

## Pass criteria

All 9 scenarios green. No tight restart loops. Reflex policy visible in session
state via `agent.configure("approval_policy.preapproved_classes", "config", "append")`
inspection pattern.
