//! Muninn memory and knowledge-graph integration for [`AgentRuntime`]:
//! memory engine construction (`memory_engine_for`, `fetch_memory_config`),
//! per-turn auto-recall (`maybe_auto_recall_turn_memory`), the `memory.*`
//! local tool implementations (including `memory.fix` / `MuninnStatus`
//! handling), `context.capture` handling, direct `life.observe` command
//! parsing and execution, the session graph preload dispatch, and the
//! memory shaping / true-up helper functions.
//!
//! Mechanically extracted from `runtime.rs` and `tool_exec.rs` (declared as a
//! `#[path]` child module of `runtime` so private `AgentRuntime` fields stay
//! accessible). No behavior change.

use super::*;

pub(super) fn engram_metadata_string(metadata: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        metadata
            .get(*key)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

pub(super) fn engram_metadata_array(metadata: &Value, keys: &[&str]) -> Vec<Value> {
    keys.iter()
        .find_map(|key| metadata.get(*key).and_then(|value| value.as_array()))
        .cloned()
        .unwrap_or_default()
}

pub(super) fn engram_annotations(metadata: &Value) -> Option<Value> {
    metadata
        .get("annotations")
        .or_else(|| metadata.get("annotation"))
        .cloned()
        .filter(|value| !value.is_null())
}

pub(super) fn engram_spacetime_frame(metadata: &Value) -> Option<MemorySpacetimeFrame> {
    metadata
        .get("spacetime_frame")
        .or_else(|| metadata.get("memory_spacetime_frame"))
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

pub(super) fn recalled_memory_from_engram(
    engram: Engram,
    recall_reason: Option<&str>,
) -> RecalledMemoryRecord {
    let summary = engram_metadata_string(&engram.metadata, &["summary"]);
    let memory_type = engram_metadata_string(&engram.metadata, &["memory_type", "type"]);
    let trust = engram_metadata_string(&engram.metadata, &["trust", "trust_level"]);
    let source = engram_metadata_string(&engram.metadata, &["source", "provenance_source"]);
    let entities = engram_metadata_array(&engram.metadata, &["entities"]);
    let relationships =
        engram_metadata_array(&engram.metadata, &["relationships", "entity_relationships"]);
    let annotations = engram_annotations(&engram.metadata);
    let spacetime_frame = engram_spacetime_frame(&engram.metadata);

    RecalledMemoryRecord {
        id: Some(engram.id),
        vault_id: if engram.vault_id.is_empty() {
            None
        } else {
            Some(engram.vault_id)
        },
        concept: engram.concept,
        content: engram.content,
        tags: engram.tags,
        summary,
        memory_type,
        confidence: Some(engram.confidence),
        trust,
        source,
        created_at: Some(engram.created_at),
        updated_at: Some(engram.updated_at),
        entities,
        relationships,
        annotations,
        recall_reason: recall_reason.map(str::to_string),
        spacetime_frame,
    }
}

pub(super) fn turn_memory_user_id(state: &SessionState) -> String {
    state
        .active_turn
        .as_ref()
        .and_then(|turn| turn.primary_user_id.as_deref())
        .map(str::trim)
        .filter(|user_id| !user_id.is_empty())
        .map(str::to_string)
        .or_else(|| {
            state
                .active_turn
                .as_ref()
                .map(|turn| turn.chat_id.trim())
                .filter(|chat_id| !chat_id.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| state.session_id.clone())
}

pub(super) fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub(super) fn memory_enum_arg(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub(super) fn memory_temporal_kind_from_arg(value: Option<&Value>) -> Option<MemoryTemporalKind> {
    match memory_enum_arg(value)?.as_str() {
        "event" => Some(MemoryTemporalKind::Event),
        "state" => Some(MemoryTemporalKind::State),
        "preference" => Some(MemoryTemporalKind::Preference),
        "rule" => Some(MemoryTemporalKind::Rule),
        "decision" => Some(MemoryTemporalKind::Decision),
        "hypothesis" => Some(MemoryTemporalKind::Hypothesis),
        "gap" => Some(MemoryTemporalKind::Gap),
        "checkpoint" => Some(MemoryTemporalKind::Checkpoint),
        _ => None,
    }
}

pub(super) fn memory_spatial_scope_from_arg(
    value: Option<&Value>,
    scope: &MemoryScope,
) -> MemorySpatialScope {
    match memory_enum_arg(value).as_deref() {
        Some("self") => MemorySpatialScope::SelfScope,
        Some("agent") => MemorySpatialScope::Agent,
        Some("user") => MemorySpatialScope::User,
        Some("session") => MemorySpatialScope::Session,
        Some("workspace") => MemorySpatialScope::Workspace,
        Some("hotel") => MemorySpatialScope::Hotel,
        Some("mesh") => MemorySpatialScope::Mesh,
        Some("global") => MemorySpatialScope::Global,
        _ => match scope {
            MemoryScope::SharedUser => MemorySpatialScope::User,
            MemoryScope::Session(_) => MemorySpatialScope::Session,
            MemoryScope::CrossScope(_) => MemorySpatialScope::Mesh,
            MemoryScope::SelfOnly => MemorySpatialScope::SelfScope,
        },
    }
}

pub(super) fn memory_authority_from_arg(value: Option<&Value>) -> Option<MemoryAuthority> {
    match memory_enum_arg(value)?.as_str() {
        "observed_runtime" => Some(MemoryAuthority::ObservedRuntime),
        "observed_repo" => Some(MemoryAuthority::ObservedRepo),
        "graph_structured" => Some(MemoryAuthority::GraphStructured),
        "user_stated" => Some(MemoryAuthority::UserStated),
        "verified_memory" => Some(MemoryAuthority::VerifiedMemory),
        "inferred_memory" => Some(MemoryAuthority::InferredMemory),
        "external" => Some(MemoryAuthority::External),
        "untrusted" => Some(MemoryAuthority::Untrusted),
        _ => None,
    }
}

pub(super) fn memory_validation_level_from_arg(
    value: Option<&Value>,
) -> Option<MemoryValidationLevel> {
    match memory_enum_arg(value)?.as_str() {
        "unverified" => Some(MemoryValidationLevel::Unverified),
        "check-green" | "check_green" => Some(MemoryValidationLevel::CheckGreen),
        "test-green" | "test_green" => Some(MemoryValidationLevel::TestGreen),
        "smoke-green" | "smoke_green" => Some(MemoryValidationLevel::SmokeGreen),
        "watched-live-green" | "watched_live_green" => {
            Some(MemoryValidationLevel::WatchedLiveGreen)
        }
        _ => None,
    }
}

pub(super) fn string_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub(super) fn string_array_arg(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn graph_anchors_from_args(args: &Value) -> GraphAnchors {
    let graph_anchors_arg = args.get("graph_anchors").filter(|v| v.is_object());
    let value_for = |key: &str| -> Option<String> {
        string_arg(args, key).or_else(|| graph_anchors_arg.and_then(|v| string_arg(v, key)))
    };
    let affected_nodes = {
        let mut nodes = string_array_arg(args, "affected_nodes");
        if nodes.is_empty() {
            if let Some(v) = graph_anchors_arg {
                nodes = string_array_arg(v, "affected_nodes");
            }
        }
        nodes
    };

    GraphAnchors {
        proposal_id: value_for("proposal_id"),
        seam_id: value_for("seam_id"),
        task_id: value_for("task_id"),
        decision_id: value_for("decision_id"),
        test_run_id: value_for("test_run_id"),
        source_doc: value_for("source_doc"),
        source_file: value_for("source_file"),
        affected_nodes,
    }
}

pub(super) fn memory_shaping_context_for_write(
    state: Option<&SessionState>,
    args: &Value,
    scope: &MemoryScope,
) -> MemoryShapingContext {
    let frame_arg = args.get("spacetime_frame").filter(|v| v.is_object());
    let value_for = |key: &str| -> Option<String> {
        string_arg(args, key).or_else(|| frame_arg.and_then(|v| string_arg(v, key)))
    };
    let number_for = |key: &str| -> Option<u64> {
        args.get(key)
            .and_then(Value::as_u64)
            .or_else(|| frame_arg.and_then(|v| v.get(key)).and_then(Value::as_u64))
    };
    let primary_user_id = state
        .and_then(|s| s.active_turn.as_ref())
        .and_then(|turn| turn.primary_user_id.clone())
        .or_else(|| value_for("primary_user_id"));

    let frame = MemorySpacetimeFrame {
        observed_at: number_for("observed_at").or_else(|| Some(now_unix_ms())),
        valid_from: number_for("valid_from"),
        valid_until: number_for("valid_until"),
        last_verified_at: number_for("last_verified_at"),
        temporal_kind: memory_temporal_kind_from_arg(
            args.get("temporal_kind")
                .or_else(|| frame_arg.and_then(|v| v.get("temporal_kind"))),
        )
        .or(Some(MemoryTemporalKind::State)),
        spatial_scope: Some(memory_spatial_scope_from_arg(
            args.get("spatial_scope")
                .or_else(|| frame_arg.and_then(|v| v.get("spatial_scope"))),
            scope,
        )),
        hotel_id: value_for("hotel_id")
            .or_else(|| std::env::var("PHILOTIC_HOTEL_NAME").ok())
            .or_else(|| std::env::var("PHILOTIC_HOTEL_ID").ok()),
        node_id: value_for("node_id").or_else(|| std::env::var("PHILOTIC_NODE_ID").ok()),
        workspace_path: value_for("workspace_path")
            .or_else(|| std::env::var("PHILOTIC_WORKSPACE").ok()),
        repo_id: value_for("repo_id"),
        branch: value_for("branch"),
        worktree_id: value_for("worktree_id"),
        session_id: state.map(|s| s.session_id.clone()),
        agent_id: state.map(|s| s.agent_id.clone()),
        primary_user_id,
        authority: memory_authority_from_arg(
            args.get("authority")
                .or_else(|| frame_arg.and_then(|v| v.get("authority"))),
        )
        .or(Some(MemoryAuthority::InferredMemory)),
        validation_level: memory_validation_level_from_arg(
            args.get("validation_level")
                .or_else(|| args.get("validation"))
                .or_else(|| frame_arg.and_then(|v| v.get("validation_level")))
                .or_else(|| frame_arg.and_then(|v| v.get("validation"))),
        ),
    };

    MemoryShapingContext {
        frame,
        graph_anchors: graph_anchors_from_args(args),
        recall_policy: string_arg(args, "recall_policy"),
        write_policy: string_arg(args, "write_policy"),
        promotion_policy: string_arg(args, "promotion_policy"),
    }
}

pub(super) fn shaped_memory_tags(
    mut tags: Vec<String>,
    shaping: &MemoryShapingContext,
) -> Vec<String> {
    if let Some(kind) = shaping.frame.temporal_kind {
        tags.push(format!("temporal:{}", kind.as_str()));
    }
    if let Some(scope) = shaping.frame.spatial_scope {
        tags.push(format!("scope:{}", scope.as_str()));
    }
    if let Some(authority) = shaping.frame.authority {
        tags.push(format!("authority:{}", authority.as_str()));
    }
    if let Some(seam_id) = shaping.graph_anchors.seam_id.as_deref() {
        tags.push(format!("seam:{seam_id}"));
    }
    if let Some(proposal_id) = shaping.graph_anchors.proposal_id.as_deref() {
        tags.push(format!("proposal:{proposal_id}"));
    }
    tags.sort();
    tags.dedup();
    tags
}

pub(super) fn shaped_memory_metadata(shaping: &MemoryShapingContext) -> Value {
    let entities = shaped_memory_entities(shaping);
    let relationships = shaped_memory_relationships(shaping, &entities);
    serde_json::json!({
        "spacetime_frame": shaping.frame,
        "graph_anchors": shaping.graph_anchors,
        "entities": entities,
        "relationships": relationships,
        "recall_policy": shaping.recall_policy,
        "write_policy": shaping.write_policy,
        "promotion_policy": shaping.promotion_policy,
        "source": "philote.memory.remember",
    })
}

pub(super) fn shaped_memory_entities(shaping: &MemoryShapingContext) -> Vec<Value> {
    let mut entities = Vec::new();
    let mut push = |name: Option<&str>, entity_type: &str| {
        if let Some(name) = name.map(str::trim).filter(|value| !value.is_empty()) {
            entities.push(serde_json::json!({
                "name": name,
                "type": entity_type,
            }));
        }
    };

    push(shaping.graph_anchors.proposal_id.as_deref(), "proposal");
    push(shaping.graph_anchors.seam_id.as_deref(), "seam");
    push(shaping.graph_anchors.task_id.as_deref(), "task");
    push(shaping.frame.session_id.as_deref(), "session");
    push(shaping.frame.agent_id.as_deref(), "agent");
    push(shaping.frame.primary_user_id.as_deref(), "user");
    push(shaping.frame.hotel_id.as_deref(), "hotel");
    push(shaping.frame.node_id.as_deref(), "node");

    for affected in &shaping.graph_anchors.affected_nodes {
        push(Some(affected), "graph_node");
    }

    entities
}

pub(super) fn shaped_memory_relationships(
    shaping: &MemoryShapingContext,
    entities: &[Value],
) -> Vec<Value> {
    let mut relationships = Vec::new();
    let has_entity = |name: &str| {
        entities
            .iter()
            .any(|entity| entity.get("name").and_then(Value::as_str) == Some(name))
    };
    let mut push = |from: Option<&str>, rel_type: &str, to: Option<&str>| {
        let Some(from) = from.map(str::trim).filter(|value| !value.is_empty()) else {
            return;
        };
        let Some(to) = to.map(str::trim).filter(|value| !value.is_empty()) else {
            return;
        };
        if has_entity(from) && has_entity(to) {
            relationships.push(serde_json::json!({
                "from_entity": from,
                "rel_type": rel_type,
                "to_entity": to,
                "weight": 0.9,
            }));
        }
    };

    push(
        shaping.graph_anchors.seam_id.as_deref(),
        "belongs_to",
        shaping.graph_anchors.proposal_id.as_deref(),
    );
    push(
        shaping.graph_anchors.task_id.as_deref(),
        "belongs_to",
        shaping.graph_anchors.seam_id.as_deref(),
    );
    push(
        shaping.frame.session_id.as_deref(),
        "worked_by",
        shaping.frame.agent_id.as_deref(),
    );
    push(
        shaping.frame.session_id.as_deref(),
        "serves_user",
        shaping.frame.primary_user_id.as_deref(),
    );
    push(
        shaping.frame.node_id.as_deref(),
        "runs_in",
        shaping.frame.hotel_id.as_deref(),
    );

    relationships
}

pub(super) fn memory_text_arg(args: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| string_arg(args, key))
}

pub(super) fn classify_memory_true_up(args: &Value) -> Value {
    let memory_claim = memory_text_arg(args, &["memory_claim", "claim"]);
    let observed_truth = memory_text_arg(args, &["observed_truth", "observed"]);
    let graph_truth = memory_text_arg(args, &["graph_truth", "graph"]);
    let resolution = memory_text_arg(args, &["resolution"]);

    let normalize = |value: &str| value.trim().to_ascii_lowercase();
    let finding_type = match (&memory_claim, &observed_truth, &graph_truth) {
        (Some(memory), Some(observed), _) if normalize(memory) == normalize(observed) => {
            "confirmed"
        }
        (Some(memory), Some(observed), _) if normalize(memory) != normalize(observed) => {
            "contradicted"
        }
        (Some(memory), None, Some(graph)) if normalize(memory) == normalize(graph) => "confirmed",
        (Some(memory), None, Some(graph)) if normalize(memory) != normalize(graph) => {
            "contradicted"
        }
        (Some(_), None, None) => "underspecified",
        (None, Some(_), Some(_)) => "missing_memory",
        _ => "needs_operator",
    };

    serde_json::json!({
        "finding_type": finding_type,
        "memory_claim": memory_claim,
        "observed_truth": observed_truth,
        "graph_truth": graph_truth,
        "resolution": resolution,
        "recommended_action": match finding_type {
            "confirmed" => "keep",
            "contradicted" => "true_up_required",
            "missing_memory" => "consider_remember",
            "underspecified" => "gather_evidence",
            _ => "operator_review",
        },
        "requires_operator": matches!(finding_type, "contradicted" | "needs_operator"),
    })
}

pub(super) fn promotion_gate_report(args: &Value) -> Value {
    let authority =
        memory_authority_from_arg(args.get("authority")).unwrap_or(MemoryAuthority::InferredMemory);
    let validation_level = memory_validation_level_from_arg(
        args.get("validation_level")
            .or_else(|| args.get("validation")),
    )
    .unwrap_or(MemoryValidationLevel::Unverified);
    let evidence_refs = string_array_arg(args, "evidence_refs");
    let operator_approved = args
        .get("operator_approved")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let authority_ok = !matches!(
        authority,
        MemoryAuthority::InferredMemory | MemoryAuthority::Untrusted
    ) || operator_approved;
    let validation_ok =
        !matches!(validation_level, MemoryValidationLevel::Unverified) || operator_approved;
    let evidence_ok = !evidence_refs.is_empty() || operator_approved;
    let allowed = authority_ok && validation_ok && evidence_ok;

    serde_json::json!({
        "allowed": allowed,
        "authority": authority.as_str(),
        "validation_level": validation_level.as_str(),
        "evidence_refs": evidence_refs,
        "operator_approved": operator_approved,
        "required_before_promotion": {
            "authority": if authority_ok { Value::Null } else { Value::String("observed_runtime, observed_repo, graph_structured, user_stated, verified_memory, or operator approval".into()) },
            "validation": if validation_ok { Value::Null } else { Value::String("check-green or stronger, or operator approval".into()) },
            "evidence": if evidence_ok { Value::Null } else { Value::String("at least one evidence_ref, or operator approval".into()) },
        }
    })
}

pub(super) fn inbound_primary_user_id(task: &InboundTaskPayload) -> Option<String> {
    task.sender_id
        .as_deref()
        .or(task.sender_username.as_deref())
        .map(str::trim)
        .filter(|user_id| !user_id.is_empty())
        .map(str::to_string)
}

pub(super) fn default_turn_recall_scope(session_id: &str) -> MemoryScope {
    MemoryScope::CrossScope(vec![
        MemoryScope::SelfOnly,
        MemoryScope::SharedUser,
        MemoryScope::Session(session_id.to_string()),
    ])
}

pub(super) fn memory_scope_from_tool_arg(scope: Option<&str>, session_id: &str) -> MemoryScope {
    match scope.unwrap_or("self") {
        "shared_user" | "user" => MemoryScope::SharedUser,
        "session" | "working" => MemoryScope::Session(session_id.to_string()),
        "cross" | "all" => default_turn_recall_scope(session_id),
        _ => MemoryScope::SelfOnly,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct DirectLifeObserveCommand {
    pub(super) label: String,
    pub(super) claim_summary: String,
    pub(super) source_id: String,
    pub(super) confidence: f64,
}

pub(super) fn parse_direct_life_observe_command(content: &str) -> Option<DirectLifeObserveCommand> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lower = trimmed.to_lowercase();
    let explicit_life_observe = lower.contains("life.observe");
    let explicit_record_open_loop =
        lower.contains("record this open loop") || lower.contains("record an open loop");
    if !explicit_life_observe && !explicit_record_open_loop {
        return None;
    }

    let label = extract_direct_life_observe_field(trimmed, "label")
        .unwrap_or_else(|| {
            if lower.contains("open loop") {
                "OpenLoop".into()
            } else {
                "Signal".into()
            }
        })
        .trim_matches(|ch: char| ch == '.' || ch == ',' || ch == ';' || ch == ':')
        .to_string();

    let source_id = extract_direct_life_observe_source(trimmed)
        .unwrap_or_else(|| "membrane:telegram".into())
        .trim_matches(|ch: char| ch == '.' || ch == ',' || ch == ';')
        .to_string();

    let confidence = extract_direct_life_observe_confidence(trimmed).unwrap_or(0.8);

    let claim_summary = extract_direct_life_observe_claim(trimmed)
        .unwrap_or_else(|| trimmed.to_string())
        .trim()
        .trim_matches('"')
        .trim()
        .to_string();

    if claim_summary.is_empty() {
        return None;
    }

    Some(DirectLifeObserveCommand {
        label,
        claim_summary,
        source_id,
        confidence,
    })
}

pub(super) fn extract_direct_life_observe_claim(content: &str) -> Option<String> {
    let lower = content.to_lowercase();
    let start = lower
        .find("record this open loop:")
        .map(|idx| idx + "record this open loop:".len())
        .or_else(|| {
            lower
                .find("record an open loop:")
                .map(|idx| idx + "record an open loop:".len())
        })
        .or_else(|| {
            lower
                .find("record this:")
                .map(|idx| idx + "record this:".len())
        });

    let mut claim = start
        .and_then(|idx| content.get(idx..))
        .map(str::trim)
        .unwrap_or(content);

    for marker in [" Use label", " use label", " Label", " label "] {
        if let Some(idx) = claim.find(marker) {
            claim = claim[..idx].trim();
            break;
        }
    }

    Some(
        claim
            .trim_matches(|ch: char| ch == '"' || ch == '\'' || ch.is_whitespace())
            .trim_end_matches('.')
            .to_string(),
    )
}

pub(super) fn extract_direct_life_observe_field(content: &str, field: &str) -> Option<String> {
    let lower = content.to_lowercase();
    let marker = format!("{field}");
    let start = lower.find(&marker)?;
    let after_marker = content.get(start + marker.len()..)?.trim_start();
    let after_separator = after_marker
        .strip_prefix(':')
        .or_else(|| after_marker.strip_prefix('='))
        .unwrap_or(after_marker)
        .trim_start();
    after_separator
        .split_whitespace()
        .next()
        .map(|value| value.trim_matches(|ch: char| ch == ',' || ch == ';' || ch == '.'))
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(super) fn extract_direct_life_observe_source(content: &str) -> Option<String> {
    let lower = content.to_lowercase();
    let marker = "source";
    let start = lower.find(marker)?;
    let after_marker = content.get(start + marker.len()..)?.trim_start();
    let after_separator = after_marker
        .strip_prefix(':')
        .or_else(|| after_marker.strip_prefix('='))
        .unwrap_or(after_marker)
        .trim_start();
    let value = after_separator
        .split_whitespace()
        .take_while(|word| {
            let word_lower = word
                .trim_matches(|ch: char| ch == ',' || ch == ';' || ch == '.')
                .to_lowercase();
            !matches!(word_lower.as_str(), "confidence" | "label" | "use")
        })
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .trim_matches(|ch: char| ch == ',' || ch == ';' || ch == '.')
        .to_string();

    (!value.is_empty()).then_some(value)
}

pub(super) fn extract_direct_life_observe_confidence(content: &str) -> Option<f64> {
    let lower = content.to_lowercase();
    let marker = "confidence";
    let start = lower.find(marker)?;
    let after_marker = content.get(start + marker.len()..)?.trim_start();
    let after_separator = after_marker
        .strip_prefix(':')
        .or_else(|| after_marker.strip_prefix('='))
        .unwrap_or(after_marker)
        .trim_start();
    after_separator
        .split_whitespace()
        .next()
        .and_then(|value| {
            value
                .trim_matches(|ch: char| ch == ',' || ch == ';' || ch == '.')
                .parse::<f64>()
                .ok()
        })
        .map(|value| value.clamp(0.0, 1.0))
}

pub(super) fn direct_life_observe_input(
    command: &DirectLifeObserveCommand,
    session_id: &str,
    turn_id: &str,
    chat_id: &str,
    agent_id: &str,
) -> Value {
    let now_iso = chrono::Utc::now().to_rfc3339();
    let suffix = Uuid::new_v4().simple().to_string();
    let label_id = command.label.to_lowercase();
    let node_id = format!("life:{}:{suffix}", label_id.replace('_', "-"));
    let packet_id = format!("evidence:{suffix}");
    let observation_id = format!("obs:{suffix}");

    serde_json::json!({
        "observation_id": observation_id,
        "evidence": {
            "packet_id": packet_id,
            "claim_ref": {
                "id": node_id,
                "label": command.label,
                "datasource": "life-graph"
            },
            "claim_summary": command.claim_summary,
                "source_refs": [{
                    "source_id": command.source_id,
                    "source_kind": "runtime_observation",
                    "reliability": {
                        "score": 0.95,
                        "basis": "direct_observation"
                    }
                }],
            "passage_refs": [],
            "confidence": command.confidence,
            "validation_state": "proposed",
            "observed_at": now_iso,
            "source_reliability": 0.95,
            "conflict_ids": [],
            "adjudication_status": "not_needed",
            "metadata": {
                "route": "philote_direct_life_observe",
                "session_id": session_id,
                "turn_id": turn_id,
                "chat_id": chat_id,
                "agent_id": agent_id
            }
        },
        "proposed_graph_refs": []
    })
}

pub(super) fn direct_life_observe_success_reply(
    command: &DirectLifeObserveCommand,
    tool_content: &str,
) -> String {
    let content = tool_content.trim();
    if content.is_empty() {
        return format!(
            "Recorded this {} in LifeGraph: {}",
            command.label, command.claim_summary
        );
    }

    match serde_json::from_str::<Value>(content) {
        Ok(value) => {
            let node_id = value
                .pointer("/node_id")
                .or_else(|| value.pointer("/data/node_id"))
                .or_else(|| value.pointer("/result/node_id"))
                .and_then(Value::as_str);
            if let Some(node_id) = node_id {
                format!(
                    "Recorded this {} in LifeGraph: {} (`{}`)",
                    command.label, command.claim_summary, node_id
                )
            } else {
                format!(
                    "Recorded this {} in LifeGraph: {}",
                    command.label, command.claim_summary
                )
            }
        }
        Err(_) => format!(
            "Recorded this {} in LifeGraph: {}",
            command.label, command.claim_summary
        ),
    }
}

pub(super) fn direct_life_observe_failure_reply(
    command: &DirectLifeObserveCommand,
    tool_content: &str,
) -> String {
    let detail = tool_content.trim();
    if detail.is_empty() {
        return format!(
            "I tried to record this {} in LifeGraph, but the runner did not return a successful result.",
            command.label
        );
    }
    format!(
        "I tried to record this {} in LifeGraph, but the runner failed: {}",
        command.label, detail
    )
}

pub(super) fn direct_life_observe_command_from_arguments(
    arguments: &Value,
) -> Option<DirectLifeObserveCommand> {
    let evidence = arguments.get("evidence")?;
    let label = evidence
        .pointer("/claim_ref/label")
        .and_then(Value::as_str)
        .unwrap_or("OpenLoop")
        .to_string();
    let claim_summary = evidence
        .get("claim_summary")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())?
        .to_string();
    let source_id = evidence
        .get("source_refs")
        .and_then(Value::as_array)
        .and_then(|refs| refs.first())
        .and_then(|source| source.get("source_id"))
        .and_then(Value::as_str)
        .unwrap_or("membrane:telegram")
        .to_string();
    let confidence = evidence
        .get("confidence")
        .and_then(Value::as_f64)
        .unwrap_or(0.8)
        .clamp(0.0, 1.0);

    Some(DirectLifeObserveCommand {
        label,
        claim_summary,
        source_id,
        confidence,
    })
}

#[derive(Debug, Clone)]
pub(super) struct MemoryCandidate {
    pub(super) concept: String,
    pub(super) content: String,
    pub(super) tags: Vec<String>,
}

pub(super) fn parse_memory_candidate(value: Option<&Value>) -> Option<MemoryCandidate> {
    let candidate = value?;
    let concept = candidate
        .get("concept")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let content = candidate
        .get("content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let tags = candidate
        .get("tags")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(MemoryCandidate {
        concept,
        content,
        tags,
    })
}

impl AgentRuntime {
    /// Fetch MuninnDB config from hotel IPC and store it for session use.
    pub(super) async fn fetch_memory_config(&mut self) {
        // Retry up to 3 times with a 3-second timeout each attempt.
        // Background: during hotel startup several guests register concurrently. The IPC
        // response for FetchMemoryConfig can be consumed by a racing guest's send_request
        // loop before this philote reads it (no per-request correlation ID on this path).
        // A short timeout + retry window clears that race without hanging indefinitely.
        for attempt in 1u8..=3 {
            info!("Requesting MuninnDB config from hotel (attempt {attempt})...");
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                self.ipc_client.send_request(IpcRequest::FetchMemoryConfig),
            )
            .await;
            match result {
                Ok(Ok(IpcResponse::MemoryConfig(config))) if config.config_json.is_some() => {
                    let json = config.config_json.expect("checked is_some");
                    match serde_json::from_str::<MuninnConfig>(&json) {
                        Ok(cfg) => {
                            info!(endpoint = %cfg.base_url, vaults = cfg.vault_tokens.len(), "MuninnDB config loaded");
                            self.muninn_config = Some(cfg);
                        }
                        Err(e) => warn!("Failed to parse MuninnConfig from hotel: {}", e),
                    }
                    return;
                }
                Ok(Ok(IpcResponse::MemoryConfig(config))) if config.config_json.is_none() => {
                    info!("Hotel has no MuninnDB config — running without memory");
                    return;
                }
                Ok(Ok(_)) | Ok(Err(_)) => {
                    warn!(
                        "Unexpected response to FetchMemoryConfig (attempt {attempt}) — retrying"
                    );
                }
                Err(_) => {
                    warn!(
                        "FetchMemoryConfig timed out (attempt {attempt}) — response likely consumed by concurrent guest registration; retrying"
                    );
                }
            }
        }
        warn!("FetchMemoryConfig failed after 3 attempts — running without memory");
    }

    /// Build a `MuninnRestEngine` scoped to the given agent and user.
    /// Returns `None` if MuninnDB is not configured or the hotel has reported it unreachable.
    pub(super) fn memory_engine_for(
        &self,
        agent_id: &str,
        user_id: &str,
    ) -> Option<MuninnRestEngine> {
        if !self.muninn_available {
            return None;
        }
        self.muninn_config.clone().map(|cfg| {
            MuninnRestEngine::new(
                cfg,
                VaultResolver {
                    agent_id: agent_id.to_string(),
                    user_id: user_id.to_string(),
                },
            )
        })
    }

    /// Fire-and-forget: fetch the agent's personal knowledge graph once per session load.
    pub(super) async fn dispatch_graph_preload_once(&mut self, session_id: &str) {
        let should_preload = self
            .sessions
            .get_mut(session_id)
            .map(|s| {
                if !s.graph_preload_dispatched {
                    s.graph_preload_dispatched = true;
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);

        if should_preload {
            let local_node_id = local_node_id();
            let graph_datasource_node_id = graph_datasource_node_id();
            let agent_id = self.agent_id.clone();
            let _ = self
                .ipc_client
                .send_request(IpcRequest::EmitTask {
                    target_node: graph_datasource_node_id,
                    target_role: "graph-datasource".into(),
                    target_guest_id: None,
                    task_json: serde_json::json!({
                        "action": "graph.query",
                        "graph_id": agent_id,
                        "query": "MATCH (n) RETURN n",
                        "reply_to": local_node_id,
                        "reply_role": "agent",
                        "session_id": session_id,
                        "turn_id": "",
                        "chat_id": "",
                    })
                    .to_string(),
                })
                .await;
        }
    }

    /// Handles a `context.capture` tool call arriving from membrane-mcp.
    ///
    /// Bypasses the LLM: parses the capture payload, stores it in Muninn, and
    /// emits a `send_reply` back to the mcp-membrane so the HTTP caller gets a
    /// response.
    pub(super) async fn handle_context_capture(
        &mut self,
        task: InboundTaskPayload,
        task_id: Uuid,
    ) -> Result<()> {
        use memory_core::MemoryEngine as _;

        // Content is JSON: {"tool": "context.capture", "args": {...}}
        let args: serde_json::Value = task
            .content
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| v.get("args").cloned())
            .unwrap_or_default();

        let capture_text = args
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let category = args
            .get("category")
            .and_then(|v| v.as_str())
            .unwrap_or("note")
            .to_string();

        let mut tags: Vec<String> = args
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        tags.push("perplexity".to_string());
        if !tags.contains(&category) {
            tags.push(category.clone());
        }

        let first_line: String = capture_text
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(50)
            .collect();
        let concept = format!("perplexity.{}: {}", category, first_line);

        let result_text = match self.memory_engine_for(&self.agent_id, &self.agent_id) {
            None => "Captured (Muninn not configured on this node).".to_string(),
            Some(engine) => {
                match engine
                    .remember(MemoryScope::SelfOnly, &concept, &capture_text, tags)
                    .await
                {
                    Ok(engram_ref) => format!("Captured to memory (id: {}).", engram_ref.id),
                    Err(e) => format!("context.capture: memory error — {e}"),
                }
            }
        };

        info!(turn_id = ?task.turn_id, result = %result_text, "context.capture handled");

        let final_reply_to = task
            .final_reply_to
            .clone()
            .unwrap_or_else(|| local_node_id());
        let final_reply_role = task
            .final_reply_role
            .clone()
            .unwrap_or_else(|| DEFAULT_REPLY_ROLE.to_string());
        let final_reply_guest_id = task.final_reply_guest_id.clone();
        let session_id = task.session_id_or_default(&self.agent_id);
        let turn_id = task.turn_id.clone().unwrap_or_else(|| task_id.to_string());
        let chat_id = task.chat_id.clone().unwrap_or_default();

        let payload = FinalReplyPayload {
            action: "send_reply",
            session_id,
            turn_id,
            chat_id,
            content: result_text,
            audio_artifact: None,
            send_text_caption: false,
            reply_markup: None,
        };

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: final_reply_to,
                target_role: final_reply_role,
                target_guest_id: final_reply_guest_id,
                task_json: serde_json::to_string(&payload)?,
            })
            .await?;

        Ok(())
    }

    pub(super) async fn maybe_auto_recall_turn_memory(&mut self, session_id: &str) -> Result<()> {
        use memory_core::MemoryEngine as _;

        let Some(state) = self.sessions.get(session_id) else {
            return Ok(());
        };
        let Some(active_turn) = state.active_turn.as_ref() else {
            return Ok(());
        };
        if active_turn.user_content.trim_start().starts_with('/') {
            return Ok(());
        }

        let recall_context = RecallContext {
            trigger: RecallTrigger::UserTurnStart,
            scope: default_turn_recall_scope(session_id),
            recall_seed_text: active_turn.user_content.clone(),
            active_goal: active_turn
                .active_plan
                .as_ref()
                .map(|plan| plan.goal.clone()),
            role_name: state
                .role_activation
                .as_ref()
                .map(|role| role.role_name.clone()),
            recent_turns: state
                .recent_turns
                .iter()
                .rev()
                .take(3)
                .map(|turn| turn.user_content.clone())
                .collect(),
            local_memory_summaries: state
                .agent_profile
                .memory_summary
                .as_deref()
                .map(|text| vec![text.to_string()])
                .unwrap_or_default(),
            tool_history_summary: active_turn
                .working_tool_history
                .iter()
                .map(|(call, _)| call.tool_name.clone())
                .collect(),
            lens: None,
        };

        let memory_user_id = turn_memory_user_id(state);
        let Some(engine) = self.memory_engine_for(&self.agent_id, &memory_user_id) else {
            let decision = memory_core::evaluate_recall(&recall_context);
            info!(
                session_id = %session_id,
                reason = %decision.reason,
                "Auto recall skipped: no Muninn memory backend configured."
            );
            let _ = self
                .emit_turn_event(
                    session_id,
                    "memory_auto_recall_skipped",
                    Some(format!("Skipped auto recall: no memory backend configured")),
                )
                .await;
            return Ok(());
        };

        let decision = match engine.evaluate_recall(&recall_context).await {
            Ok(d) => d,
            Err(err) => {
                warn!(session_id = %session_id, error = %err, "Auto recall skipped: memory engine unavailable.");
                let _ = self
                    .emit_turn_event(
                        session_id,
                        "memory_auto_recall_skipped",
                        Some(format!("Skipped auto recall: memory engine unavailable")),
                    )
                    .await;
                return Ok(());
            }
        };
        if !decision.should_recall() {
            info!(
                session_id = %session_id,
                reason = %decision.reason,
                "Auto recall skipped for turn."
            );
            let _ = self
                .emit_turn_event(
                    session_id,
                    "memory_auto_recall_skipped",
                    Some(format!("Skipped auto recall: {}", decision.reason)),
                )
                .await;
            return Ok(());
        }

        info!(
            session_id = %session_id,
            reason = %decision.reason,
            query = %decision.query.as_deref().unwrap_or(""),
            limit = ?decision.limit,
            "Running auto recall for turn."
        );

        let result = match engine.recall_for_turn(&recall_context).await {
            Ok(r) => r,
            Err(err) => {
                warn!(session_id = %session_id, error = %err, "Auto recall failed: memory engine error.");
                return Ok(());
            }
        };
        let recall_reason = result.decision.reason.clone();
        let recalled_memories = result
            .engrams
            .into_iter()
            .map(|engram| recalled_memory_from_engram(engram, Some(recall_reason.as_str())))
            .collect::<Vec<_>>();
        let recalled_count = recalled_memories.len();
        let concept_summary = recalled_memories
            .iter()
            .take(3)
            .map(|memory| memory.concept.clone())
            .collect::<Vec<_>>()
            .join(", ");

        if let Some(state) = self.sessions.get_mut(session_id) {
            if let Some(active_turn) = state.active_turn.as_mut() {
                active_turn.recalled_memories = recalled_memories;
            }
        }

        info!(
            session_id = %session_id,
            total = recalled_count,
            concepts = %concept_summary,
            "Auto recall completed for turn."
        );
        let _ = self
            .emit_turn_event(
                session_id,
                "memory_auto_recall_completed",
                Some(format!(
                    "Recalled {} memory item(s){}",
                    recalled_count,
                    if concept_summary.is_empty() {
                        String::new()
                    } else {
                        format!(": {concept_summary}")
                    }
                )),
            )
            .await;

        Ok(())
    }

    pub(super) async fn handle_direct_life_observe_command(
        &mut self,
        command_task_id: Uuid,
        session_id: String,
        turn_id: String,
        chat_id: String,
        reply_to: String,
        reply_role: String,
        reply_guest_id: Option<String>,
        command: DirectLifeObserveCommand,
        user_id: Option<String>,
    ) -> Result<()> {
        let route = self.sessions.get(&session_id).and_then(|state| {
            state
                .tool_is_enabled("life.observe")
                .then(|| state.resolve_tool_route("life.observe"))
                .flatten()
                .cloned()
        });

        let Some(route) = route else {
            warn!(
                session_id = %session_id,
                "Direct life.observe command could not run because life.observe is not enabled"
            );
            return self
                .complete_command_without_turn(
                    command_task_id,
                    session_id,
                    turn_id,
                    chat_id,
                    reply_to,
                    reply_role,
                    reply_guest_id,
                    "I could not record that because life.observe is not enabled for this session."
                        .into(),
                    Some("failed"),
                    None,
                )
                .await;
        };

        let arguments =
            direct_life_observe_input(&command, &session_id, &turn_id, &chat_id, &self.agent_id);
        let payload = ToolExecutionPayload {
            action: "execute_tool",
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            chat_id: chat_id.clone(),
            tool_name: "life.observe".into(),
            arguments: arguments.clone(),
            execution_mode: route.execution_mode.clone(),
            agent_id: self.agent_id.clone(),
            user_id: user_id.clone(),
            runner_id: route.runner_id.clone(),
            incarnation_id: route.incarnation_id.clone(),
            hotel_id: route.hotel_id.clone(),
            environment_id: route.environment_id.clone(),
            task_runner_kind: route.task_runner_kind.clone(),
            task_runner_config: route.task_runner_config.clone(),
            selection_reason: route.selection_reason.clone(),
            workspace_ref: None,
            task_runner_overlay: None,
            return_route: Some(philotic_client::ReturnRoute {
                node: local_node_id(),
                role: "agent".into(),
                guest_id: Some(self.agent_id.clone()),
                session_id: Some(session_id.clone()),
                turn_id: Some(turn_id.clone()),
                correlation_id: None,
            }),
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            reply_guest_id: Some(self.agent_id.clone()),
            final_reply_to: reply_to.clone(),
            final_reply_role: reply_role.clone(),
            final_reply_guest_id: reply_guest_id.clone(),
        };

        let tool_call = ToolCall {
            tool_name: "life.observe".into(),
            arguments: arguments.clone(),
        };
        let checkpoint = if let Some(state) = self.sessions.get_mut(&session_id) {
            state.start_turn(WorkingTurn {
                task_id: command_task_id,
                turn_id: turn_id.clone(),
                chat_id: chat_id.clone(),
                primary_user_id: user_id.clone(),
                user_content: format!("life.observe direct command: {}", command.claim_summary),
                final_reply_to: reply_to.clone(),
                final_reply_role: reply_role.clone(),
                final_reply_guest_id: reply_guest_id.clone(),
                phase: TurnPhase::WaitingTool,
                iteration: 0,
                pending_tool_call: Some(tool_call),
                pending_approval: None,
                working_tool_history: Vec::new(),
                recalled_memories: Vec::new(),
                active_plan: None,
                consecutive_step_failures: 0,
                provider_repair_note: None,
                provider_repair_attempts: 0,
                pending_text_reply: None,
                had_voice_input: false,
                awaiting_transcription_reentry: false,
                scripted_loop_context: None,
                associated_paracrine_ids: Vec::new(),
                paracrine_origin: None,
                paracrine_reply_session_id: None,
                paracrine_reply_chat_id: None,
                paracrine_response_routing: None,
                paracrine_merge_completed: false,
                plan_confirmed: false,
                plan_confirm_note: None,
                fallback_tier: if self.network_offline { 1 } else { 0 },
                streaming_retry_attempts: 0,
                streamed_content: String::new(),
                paracrine_hop_count: 0,
                paracrine_chain_started_at: None,
            });
            Some((
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
            ))
        } else {
            None
        };
        if let Some((checkpoint_memory_type, checkpoint_json, index_state)) = checkpoint {
            self.ipc_client
                .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
                .await?;
            self.sync_session_index(&index_state).await?;
        }

        self.ipc_client
            .send_request(IpcRequest::UpdateTask {
                task_id: command_task_id,
                state: "waiting_tool".into(),
                payload: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": chat_id,
                    "tool_name": "life.observe",
                    "direct_route": true,
                }),
            })
            .await?;

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: route.target_node,
                target_role: route.target_role,
                target_guest_id: if route.execution_mode == "life_graph" {
                    None
                } else {
                    route.incarnation_id.clone()
                },
                task_json: serde_json::to_string(&payload)?,
            })
            .await?;

        let node_id = arguments
            .pointer("/evidence/claim_ref/id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("life:unknown");
        let reply_content = format!(
            "Recorded this {} in LifeGraph: {} (`{}`)",
            command.label, command.claim_summary, node_id
        );
        self.complete_local_command(session_id.clone(), turn_id.clone(), reply_content)
            .await?;

        info!(
            session_id = %session_id,
            label = %command.label,
            source_id = %command.source_id,
            confidence = command.confidence,
            "Direct life.observe command routed without model turn"
        );
        Ok(())
    }

    pub(super) async fn execute_memory_recall_tool(
        &mut self,
        payload: ToolExecutionPayload,
    ) -> Result<()> {
        use memory_core::MemoryEngine as _;

        let query = payload
            .arguments
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let explicit_limit = payload
            .arguments
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);

        let recall_limit = self
            .sessions
            .get(&payload.session_id)
            .map(|s| s.settings.memory.recall_limit)
            .unwrap_or(5);
        let limit = explicit_limit.unwrap_or(recall_limit).clamp(1, 20);

        let memory_user_id = self
            .sessions
            .get(&payload.session_id)
            .map(turn_memory_user_id)
            .unwrap_or_else(|| self.agent_id.clone());
        let scope = memory_scope_from_tool_arg(
            payload.arguments.get("scope").and_then(|v| v.as_str()),
            &payload.session_id,
        );

        let content = match self.memory_engine_for(&self.agent_id, &memory_user_id) {
            None => "Memory unavailable: MuninnDB not configured.".to_string(),
            Some(engine) => match engine.activate(&query, scope, Some(limit)).await {
                Err(e) => format!("memory.recall error: {e}"),
                Ok(result) if result.engrams.is_empty() => {
                    "No relevant memories found.".to_string()
                }
                Ok(result) => {
                    let mut out = format!("{} engram(s) recalled:\n", result.engrams.len());
                    for (i, eng) in result.engrams.iter().enumerate() {
                        out.push_str(&format!(
                            "{}. [{}] {} — {}{}\n",
                            i + 1,
                            eng.concept,
                            eng.content,
                            eng.tags.join(", "),
                            if eng.vault_id.is_empty() {
                                String::new()
                            } else {
                                format!(" (vault: {})", eng.vault_id)
                            }
                        ));
                    }
                    out
                }
            },
        };

        self.handle_tool_result(InboundTaskPayload {
            action: Some("tool_result".into()),
            agent_action: None,
            handoff_bundle: None,
            source: Some("agent".into()),
            session_id: Some(payload.session_id),
            turn_id: Some(payload.turn_id),
            transport: None,
            chat_id: Some(payload.chat_id),
            thread_id: None,
            sender_id: None,
            sender_username: None,
            message_kind: None,
            content: Some(content),
            attachments: Vec::new(),
            command: None,
            callback_data: None,
            raw_transport_event: None,
            error: None,
            tool_name: Some(payload.tool_name),
            arguments: None,
            final_reply_to: Some(payload.final_reply_to),
            final_reply_role: Some(payload.final_reply_role),
            final_reply_guest_id: payload.final_reply_guest_id,
            ..Default::default()
        })
        .await
    }

    pub(super) async fn execute_memory_remember_tool(
        &mut self,
        payload: ToolExecutionPayload,
    ) -> Result<()> {
        use memory_core::MemoryEngine as _;

        let concept = payload
            .arguments
            .get("concept")
            .and_then(|v| v.as_str())
            .unwrap_or("untitled")
            .to_string();
        let content_str = payload
            .arguments
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let tags: Vec<String> = payload
            .arguments
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let scope = memory_scope_from_tool_arg(
            payload.arguments.get("scope").and_then(|v| v.as_str()),
            &payload.session_id,
        );

        let memory_user_id = self
            .sessions
            .get(&payload.session_id)
            .map(turn_memory_user_id)
            .unwrap_or_else(|| self.agent_id.clone());
        let shaping = memory_shaping_context_for_write(
            self.sessions.get(&payload.session_id),
            &payload.arguments,
            &scope,
        );
        let tags = shaped_memory_tags(tags, &shaping);
        let metadata = shaped_memory_metadata(&shaping);
        let result_text = match self.memory_engine_for(&self.agent_id, &memory_user_id) {
            None => "Memory unavailable: MuninnDB not configured.".to_string(),
            Some(engine) => {
                match engine
                    .remember_with_metadata(scope, &concept, &content_str, tags, metadata)
                    .await
                {
                    Ok(engram_ref) => {
                        let _ = engine.retry_enrich(&engram_ref.id).await;
                        format!(
                            "Stored memory '{}' (id: {}, vault: {}).",
                            concept, engram_ref.id, engram_ref.vault_id
                        )
                    }
                    Err(e) => format!("memory.remember error: {e}"),
                }
            }
        };

        self.handle_tool_result(InboundTaskPayload {
            action: Some("tool_result".into()),
            agent_action: None,
            handoff_bundle: None,
            source: Some("agent".into()),
            session_id: Some(payload.session_id),
            turn_id: Some(payload.turn_id),
            transport: None,
            chat_id: Some(payload.chat_id),
            thread_id: None,
            sender_id: None,
            sender_username: None,
            message_kind: None,
            content: Some(result_text),
            attachments: Vec::new(),
            command: None,
            callback_data: None,
            raw_transport_event: None,
            error: None,
            tool_name: Some(payload.tool_name),
            arguments: None,
            final_reply_to: Some(payload.final_reply_to),
            final_reply_role: Some(payload.final_reply_role),
            final_reply_guest_id: payload.final_reply_guest_id,
            ..Default::default()
        })
        .await
    }

    pub(super) async fn execute_memory_cultivate_tool(
        &mut self,
        payload: ToolExecutionPayload,
    ) -> Result<()> {
        use memory_core::MemoryEngine as _;

        let query = payload
            .arguments
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("memory cultivation candidates")
            .to_string();
        let limit = payload
            .arguments
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(8)
            .clamp(1, 20);
        let mode = payload
            .arguments
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("report");
        let scope = memory_scope_from_tool_arg(
            payload.arguments.get("scope").and_then(|v| v.as_str()),
            &payload.session_id,
        );
        let memory_user_id = self
            .sessions
            .get(&payload.session_id)
            .map(turn_memory_user_id)
            .unwrap_or_else(|| self.agent_id.clone());

        let report = match self.memory_engine_for(&self.agent_id, &memory_user_id) {
            None => serde_json::json!({
                "ok": false,
                "error": "MuninnDB not configured",
            }),
            Some(engine) => match engine.activate(&query, scope, Some(limit)).await {
                Err(e) => serde_json::json!({
                    "ok": false,
                    "error": e.to_string(),
                }),
                Ok(result) => {
                    let candidates = result
                        .engrams
                        .iter()
                        .map(|engram| {
                            let has_frame = engram_spacetime_frame(&engram.metadata).is_some();
                            let has_entities =
                                !engram_metadata_array(&engram.metadata, &["entities"]).is_empty();
                            serde_json::json!({
                                "id": engram.id,
                                "concept": engram.concept,
                                "vault_id": engram.vault_id,
                                "needs_spacetime_frame": !has_frame,
                                "needs_entity_overlay": !has_entities,
                                "recommended_action": if !has_frame || !has_entities {
                                    "enrich_metadata"
                                } else {
                                    "keep"
                                },
                            })
                        })
                        .collect::<Vec<_>>();
                    serde_json::json!({
                        "ok": true,
                        "mode": mode,
                        "query": query,
                        "mutation_performed": false,
                        "candidates": candidates,
                    })
                }
            },
        };

        self.handle_tool_result(InboundTaskPayload {
            action: Some("tool_result".into()),
            agent_action: None,
            handoff_bundle: None,
            source: Some("agent".into()),
            session_id: Some(payload.session_id),
            turn_id: Some(payload.turn_id),
            transport: None,
            chat_id: Some(payload.chat_id),
            thread_id: None,
            sender_id: None,
            sender_username: None,
            message_kind: None,
            content: Some(report.to_string()),
            attachments: Vec::new(),
            command: None,
            callback_data: None,
            raw_transport_event: None,
            error: None,
            tool_name: Some(payload.tool_name),
            arguments: None,
            final_reply_to: Some(payload.final_reply_to),
            final_reply_role: Some(payload.final_reply_role),
            final_reply_guest_id: payload.final_reply_guest_id,
            ..Default::default()
        })
        .await
    }

    pub(super) async fn execute_memory_true_up_tool(
        &mut self,
        payload: ToolExecutionPayload,
    ) -> Result<()> {
        let report = classify_memory_true_up(&payload.arguments);

        self.handle_tool_result(InboundTaskPayload {
            action: Some("tool_result".into()),
            agent_action: None,
            handoff_bundle: None,
            source: Some("agent".into()),
            session_id: Some(payload.session_id),
            turn_id: Some(payload.turn_id),
            transport: None,
            chat_id: Some(payload.chat_id),
            thread_id: None,
            sender_id: None,
            sender_username: None,
            message_kind: None,
            content: Some(report.to_string()),
            attachments: Vec::new(),
            command: None,
            callback_data: None,
            raw_transport_event: None,
            error: None,
            tool_name: Some(payload.tool_name),
            arguments: None,
            final_reply_to: Some(payload.final_reply_to),
            final_reply_role: Some(payload.final_reply_role),
            final_reply_guest_id: payload.final_reply_guest_id,
            ..Default::default()
        })
        .await
    }

    pub(super) async fn execute_memory_promote_candidate_tool(
        &mut self,
        payload: ToolExecutionPayload,
    ) -> Result<()> {
        let report = promotion_gate_report(&payload.arguments);

        self.handle_tool_result(InboundTaskPayload {
            action: Some("tool_result".into()),
            agent_action: None,
            handoff_bundle: None,
            source: Some("agent".into()),
            session_id: Some(payload.session_id),
            turn_id: Some(payload.turn_id),
            transport: None,
            chat_id: Some(payload.chat_id),
            thread_id: None,
            sender_id: None,
            sender_username: None,
            message_kind: None,
            content: Some(report.to_string()),
            attachments: Vec::new(),
            command: None,
            callback_data: None,
            raw_transport_event: None,
            error: None,
            tool_name: Some(payload.tool_name),
            arguments: None,
            final_reply_to: Some(payload.final_reply_to),
            final_reply_role: Some(payload.final_reply_role),
            final_reply_guest_id: payload.final_reply_guest_id,
            ..Default::default()
        })
        .await
    }

    pub(super) async fn execute_memory_status_tool(
        &mut self,
        payload: ToolExecutionPayload,
    ) -> Result<()> {
        let content = match &self.muninn_config {
            Some(cfg) => {
                let status = if self.muninn_available {
                    "connected"
                } else {
                    "unreachable"
                };
                format!(
                    "MuninnDB {status} — endpoint: {}, {} vault(s) configured.",
                    cfg.base_url,
                    cfg.vault_tokens.len()
                )
            }
            None => "MuninnDB not configured on this hotel.".into(),
        };
        self.handle_tool_result(InboundTaskPayload {
            action: Some("tool_result".into()),
            source: Some("agent".into()),
            session_id: Some(payload.session_id),
            turn_id: Some(payload.turn_id),
            chat_id: Some(payload.chat_id),
            content: Some(content),
            tool_name: Some(payload.tool_name),
            final_reply_to: Some(payload.final_reply_to),
            final_reply_role: Some(payload.final_reply_role),
            final_reply_guest_id: payload.final_reply_guest_id,
            ..Default::default()
        })
        .await
    }

    pub(super) async fn execute_memory_fix_tool(
        &mut self,
        payload: ToolExecutionPayload,
    ) -> Result<()> {
        let (content, tool_err) = match self
            .ipc_client
            .send_request(IpcRequest::RefreshMemoryConfig)
            .await
        {
            Ok(IpcResponse::MuninnStatus {
                available,
                endpoint,
            }) => {
                self.muninn_available = available;
                let msg = if available {
                    format!(
                        "MuninnDB probe succeeded — connected to {endpoint}. Memory tools re-enabled."
                    )
                } else {
                    format!(
                        "MuninnDB probe failed — {endpoint} is unreachable. Hotel will retry every 60s. Outage recorded in heal queue."
                    )
                };
                (msg, None)
            }
            Ok(_) => ("MuninnDB probe response unrecognized.".into(), None),
            Err(e) => {
                let err = TaskErrorPayload::transport_error(
                    "philote",
                    format!("memory.fix: IPC transport error — {e}"),
                );
                (err.display_message(), Some(err))
            }
        };
        self.handle_tool_result(InboundTaskPayload {
            action: Some("tool_result".into()),
            source: Some("agent".into()),
            session_id: Some(payload.session_id),
            turn_id: Some(payload.turn_id),
            chat_id: Some(payload.chat_id),
            content: Some(content),
            error: tool_err,
            tool_name: Some(payload.tool_name),
            final_reply_to: Some(payload.final_reply_to),
            final_reply_role: Some(payload.final_reply_role),
            final_reply_guest_id: payload.final_reply_guest_id,
            ..Default::default()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_life_observe_parser_handles_telegram_open_loop_request() {
        let parsed = super::parse_direct_life_observe_command(
            "Beacon, please use life.observe to record this open loop: \
             I need to schedule the rowing habit for weekly Saturdays. \
             Use label OpenLoop, source membrane:telegram, confidence 0.8",
        )
        .expect("explicit life.observe request should parse");

        assert_eq!(parsed.label, "OpenLoop");
        assert_eq!(
            parsed.claim_summary,
            "I need to schedule the rowing habit for weekly Saturdays"
        );
        assert_eq!(parsed.source_id, "membrane:telegram");
        assert_eq!(parsed.confidence, 0.8);
    }

    #[test]
    fn direct_life_observe_input_has_runner_shape() {
        let command = super::DirectLifeObserveCommand {
            label: "OpenLoop".into(),
            claim_summary: "Schedule rowing habit on weekly Saturdays".into(),
            source_id: "membrane:telegram".into(),
            confidence: 0.8,
        };

        let value = super::direct_life_observe_input(
            &command,
            "telegram:123:agent-beacon",
            "turn-1",
            "123",
            "agent-beacon",
        );

        assert_eq!(value["evidence"]["claim_ref"]["label"], "OpenLoop");
        assert_eq!(
            value["evidence"]["claim_summary"],
            "Schedule rowing habit on weekly Saturdays"
        );
        assert_eq!(
            value["evidence"]["source_refs"][0]["source_id"],
            "membrane:telegram"
        );
        assert_eq!(value["evidence"]["confidence"], 0.8);
        assert_eq!(
            value["evidence"]["metadata"]["route"],
            "philote_direct_life_observe"
        );
    }

    #[test]
    fn parses_memory_candidate_from_model_result() {
        let candidate = parse_memory_candidate(Some(&serde_json::json!({
            "concept": "ready-for-task-selection",
            "content": "The assistant confirmed readiness and offered the next work options.",
            "tags": ["status", "readiness"]
        })))
        .expect("memory candidate should parse");

        assert_eq!(candidate.concept, "ready-for-task-selection");
        assert_eq!(
            candidate.content,
            "The assistant confirmed readiness and offered the next work options."
        );
        assert_eq!(candidate.tags, vec!["status", "readiness"]);
    }
}
