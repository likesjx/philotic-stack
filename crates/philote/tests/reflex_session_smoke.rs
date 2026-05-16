/// Reflex engine session smoke tests.
///
/// Exercises the three integration points between `ReflexEngine` and `SessionState`:
///
///   - `apply_reflex_materialization()` — Layer 2, fired at session open and checkpoint restore.
///   - `apply_reflex_ingress()` — Layer 3 belt-and-suspenders on each task arrival.
///   - `fire_reflex_event()` — Layer 3 runtime delta (TTS failure, etc.).
///
/// These tests operate entirely in-process against `SessionState` — no hotel or
/// IPC socket required.
use agent_core::reflex::{IngressAction, MaterializationContext, MembraneBind, ReflexEvent};
use agent_core::session::{SessionState, TtsMode};

fn discord_voice_context() -> MaterializationContext {
    MaterializationContext {
        membrane_bindings: vec![MembraneBind {
            kind: "discord_voice".into(),
            channel_id: Some("vc-111".into()),
            guild_id: Some("guild-999".into()),
        }],
        capabilities: vec!["elevenlabs_tts".into()],
    }
}

fn fresh_session() -> SessionState {
    SessionState::new(
        "sess-reflex-smoke".into(),
        "agent-jane-01".into(),
        "discord_voice".into(),
    )
}

// ── Scenario 3a: fresh session materialization ────────────────────────────────

/// A freshly created session with a discord_voice membrane binding and
/// ElevenLabs TTS capability must set voice_action=transcribe and mode=auto
/// after `apply_reflex_materialization()`.
#[test]
fn fresh_materialization_sets_voice_action_and_tts_mode() {
    let mut state = fresh_session();
    state.agent_profile.reflex_context = discord_voice_context();

    state.apply_reflex_materialization();

    assert_eq!(
        state
            .agent_profile
            .media_routing_policy
            .voice_action
            .as_deref(),
        Some("transcribe"),
        "discord_voice membrane must assert voice_action=transcribe"
    );
    assert_eq!(
        state.agent_profile.voice_response_policy.mode,
        TtsMode::Auto,
        "elevenlabs_tts capability must assert TTS mode=auto"
    );
}

// ── Scenario 3b: checkpoint-restore re-materialization ────────────────────────

/// After a checkpoint round-trip, re-applying materialization (with the same
/// agent profile) must yield the same policy as the fresh case.
///
/// This mirrors what `AgentRuntime` does:
///   `from_checkpoint(&checkpoint)` → set `agent_profile` → `apply_reflex_materialization()`.
#[test]
fn checkpoint_restore_reapplies_reflex_correctly() {
    // ── Set up and checkpoint a session ──────────────────────────────────────
    let mut original = fresh_session();
    original.agent_profile.reflex_context = discord_voice_context();
    original.apply_reflex_materialization();

    let checkpoint = original.checkpoint_json();

    // ── Restore from checkpoint ───────────────────────────────────────────────
    let mut restored = SessionState::from_checkpoint(&checkpoint)
        .expect("checkpoint must deserialize to a valid SessionState");

    // The hotel re-injects the agent_profile after restoration.
    // We simulate that here by setting the same profile with reflex_context.
    restored.agent_profile.reflex_context = discord_voice_context();
    restored.apply_reflex_materialization();

    assert_eq!(
        restored
            .agent_profile
            .media_routing_policy
            .voice_action
            .as_deref(),
        Some("transcribe"),
        "checkpoint-restored session must re-assert voice_action=transcribe"
    );
    assert_eq!(
        restored.agent_profile.voice_response_policy.mode,
        TtsMode::Auto,
        "checkpoint-restored session must re-assert TTS mode=auto"
    );
}

// ── Scenario 4: voice.dialogue ingress reflex ─────────────────────────────────

/// When a `voice.dialogue` task arrives, `apply_reflex_ingress(VoiceDialogue)`
/// must assert voice_action=transcribe — even on a session with no membrane
/// binding in the reflex_context (belt-and-suspenders).
#[test]
fn voice_dialogue_ingress_asserts_transcribe() {
    let mut state = fresh_session();
    // No discord_voice binding — ingress reflex fires anyway.

    state.apply_reflex_ingress(IngressAction::VoiceDialogue);

    assert_eq!(
        state
            .agent_profile
            .media_routing_policy
            .voice_action
            .as_deref(),
        Some("transcribe"),
        "voice.dialogue ingress must assert voice_action=transcribe regardless of binding"
    );
}

/// `apply_reflex_ingress` on a non-voice action must not alter the voice policy.
#[test]
fn text_message_ingress_does_not_alter_voice_policy() {
    let mut state = fresh_session();

    state.apply_reflex_ingress(IngressAction::TextMessage);

    assert_eq!(
        state.agent_profile.media_routing_policy.voice_action, None,
        "text message ingress must leave voice_action unset"
    );
}

// ── Scenario 5: TTS failure fallback ─────────────────────────────────────────

/// A TTS failure event must set fallback_to_text=true so the turn can still
/// complete via text delivery even if synthesis failed.
#[test]
fn tts_failure_event_enables_fallback_to_text() {
    let mut state = fresh_session();
    // Ensure fallback starts at the default (true per VoiceResponsePolicy::default).
    // Explicitly set to false to prove the reflex actually sets it.
    state.agent_profile.voice_response_policy.fallback_to_text = false;

    state.fire_reflex_event(ReflexEvent::TtsFailure {
        provider: "elevenlabs".into(),
    });

    assert!(
        state.agent_profile.voice_response_policy.fallback_to_text,
        "TtsFailure event must set fallback_to_text=true"
    );
}

/// A TTS failure must not alter the TTS mode — the agent stays in auto/on
/// mode and the next turn can re-attempt synthesis.
#[test]
fn tts_failure_does_not_change_tts_mode() {
    let mut state = fresh_session();
    state.agent_profile.reflex_context = discord_voice_context();
    state.apply_reflex_materialization(); // mode → Auto

    state.fire_reflex_event(ReflexEvent::TtsFailure {
        provider: "elevenlabs".into(),
    });

    assert_eq!(
        state.agent_profile.voice_response_policy.mode,
        TtsMode::Auto,
        "TtsFailure must not roll back the TTS mode"
    );
}

// ── Scenario 3+4 combined: materialization + ingress compose correctly ────────

/// Materialization sets the baseline policy; ingress-time rules are additive
/// and must not clobber the materialized state.
#[test]
fn materialization_and_ingress_compose_without_conflict() {
    let mut state = fresh_session();
    state.agent_profile.reflex_context = discord_voice_context();
    state.apply_reflex_materialization();

    // voice_action should already be "transcribe" from materialization.
    // Ingress fires the same assertion — must remain "transcribe" (idempotent).
    state.apply_reflex_ingress(IngressAction::VoiceDialogue);

    assert_eq!(
        state
            .agent_profile
            .media_routing_policy
            .voice_action
            .as_deref(),
        Some("transcribe")
    );
    assert_eq!(
        state.agent_profile.voice_response_policy.mode,
        TtsMode::Auto,
        "materialized TTS mode must survive ingress-time reflex application"
    );
}
