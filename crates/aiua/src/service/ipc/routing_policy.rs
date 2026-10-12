//! Routing policy: graph instances, rules, reflex preferences and evidence, routing pipeline rules, policy evaluation, router stats.
//!
//! Handler bodies moved verbatim from the `process_request` match in
//! `ipc/mod.rs` (IPC_DISPATCH_SPLIT); parameters keep their declared types.

use super::*;

impl IpcServer {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_register_graph_instance(
        graph_id: String,
        instance_id: String,
        graph: &GraphDomain,
    ) -> IpcResponse {
        use ansible_mesh_core::storage::GraphRunnerInstanceRecord;
        let record = GraphRunnerInstanceRecord {
            graph_id: graph_id.clone(),
            instance_id: instance_id.clone(),
            registered_at: unix_ts(),
        };
        match graph.upsert_graph_runner_instance(&record) {
            Ok(()) => {
                info!(
                    graph_id = %graph_id,
                    instance_id = %instance_id,
                    "Graph runner instance registered"
                );
                IpcResponse::GraphInstanceRegistered { graph_id }
            }
            Err(err) => {
                error!("Failed to register graph runner instance: {err}");
                IpcResponse::error("register_graph_instance", "STORAGE_ERROR", err.to_string())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_list_graph_instances(graph: &GraphDomain) -> IpcResponse {
        match graph.get_graph_runner_registry() {
            Ok(records) => {
                let instances: Vec<serde_json::Value> = records
                    .into_iter()
                    .map(|r| {
                        serde_json::json!({
                            "graph_id": r.graph_id,
                            "instance_id": r.instance_id,
                            "registered_at": r.registered_at,
                        })
                    })
                    .collect();
                IpcResponse::GraphInstanceList { instances }
            }
            Err(e) => IpcResponse::error("list_graph_instances", "STORAGE_ERROR", e.to_string()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_propose_rule(
        agent_id: String,
        description: String,
        rationale: String,
        graph: &GraphDomain,
    ) -> IpcResponse {
        use ansible_mesh_core::graph::RuleRecord;
        let rule_id = Uuid::new_v4().to_string();
        let record = RuleRecord {
            rule_id: rule_id.clone(),
            agent_id: agent_id.clone(),
            description,
            rationale,
            created_at: unix_ts(),
        };
        match graph.upsert_rule(&record) {
            Ok(()) => {
                info!(agent_id = %agent_id, rule_id = %rule_id, "Rule stored via IPC");
                IpcResponse::RuleProposed { rule_id }
            }
            Err(err) => {
                error!("Failed to store rule: {err}");
                IpcResponse::error("propose_rule", "STORAGE_ERROR", err.to_string())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_record_routing_policy_proposal(
        agent_id: String,
        problem: String,
        proposed_change: String,
        evidence: String,
        affected_stage: Option<String>,
        affected_capability: Option<String>,
        learned_reflex_preference_key: Option<String>,
        graph: &GraphDomain,
    ) -> IpcResponse {
        use ansible_mesh_core::graph::{
            RoutingPolicyDispositionRecord, RoutingPolicyEvaluationRecord, RoutingPolicyRecord,
        };
        let proposal_id = Uuid::new_v4().to_string();
        let created_at = unix_ts();
        let record = RoutingPolicyRecord {
            proposal_id: proposal_id.clone(),
            agent_id: agent_id.clone(),
            problem,
            proposed_change,
            evidence,
            affected_stage,
            affected_capability,
            learned_reflex_preference_key,
            operator_disposition: RoutingPolicyDispositionRecord {
                state: "approved".into(),
                reason: "Approved via operator-gated routing.policy.propose execution.".into(),
                decided_at: created_at,
            },
            evaluations: vec![RoutingPolicyEvaluationRecord {
                evaluation_kind: "operator_disposition".into(),
                decision: "approved".into(),
                reason: "routing.policy.propose executed after operator approval.".into(),
                created_at,
                source_tool: Some("routing.policy.propose".into()),
            }],
            created_at,
        };
        match graph.upsert_routing_policy(&record) {
            Ok(()) => {
                info!(
                    agent_id = %agent_id,
                    proposal_id = %proposal_id,
                    "Routing policy proposal stored via IPC"
                );
                IpcResponse::RoutingPolicyRecorded { proposal_id }
            }
            Err(err) => {
                error!("Failed to store routing policy proposal: {err}");
                IpcResponse::error(
                    "record_routing_policy_proposal",
                    "STORAGE_ERROR",
                    err.to_string(),
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_list_routing_policies(
        agent_id: String,
        graph: &GraphDomain,
    ) -> IpcResponse {
        match graph.list_routing_policies(&agent_id) {
            Ok(policies) => {
                let json_policies: Vec<serde_json::Value> = policies
                    .into_iter()
                    .map(|policy| serde_json::to_value(policy).unwrap_or(serde_json::Value::Null))
                    .collect();
                IpcResponse::RoutingPolicyList {
                    policies: json_policies,
                }
            }
            Err(err) => {
                error!("Failed to list routing policies: {err}");
                IpcResponse::error("list_routing_policies", "STORAGE_ERROR", err.to_string())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_list_rules(agent_id: String, graph: &GraphDomain) -> IpcResponse {
        match graph.list_rules(&agent_id) {
            Ok(rules) => {
                let json_rules: Vec<serde_json::Value> = rules
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "rule_id": r.rule_id,
                            "agent_id": r.agent_id,
                            "description": r.description,
                            "rationale": r.rationale,
                            "created_at": r.created_at,
                        })
                    })
                    .collect();
                IpcResponse::RuleList { rules: json_rules }
            }
            Err(err) => {
                error!("Failed to list rules: {err}");
                IpcResponse::error("list_rules", "STORAGE_ERROR", err.to_string())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_upsert_agent_reflex_preference(
        agent_id: String,
        preference_key: String,
        precedence: i32,
        reflexes_json: serde_json::Value,
        config_json: serde_json::Value,
    ) -> IpcResponse {
        use ansible_mesh_core::agent_graph_storage::AgentReflexPreference;
        let path = agent_graph_db_path(&agent_id);
        let result = (|| -> anyhow::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let storage = SqliteAgentGraphStorage::open(&agent_id, &path)?;
            storage.upsert_reflex_preference(&AgentReflexPreference {
                agent_id: agent_id.clone(),
                preference_key: preference_key.clone(),
                precedence,
                reflexes_json,
                config_json,
                updated_at: 0,
            })?;
            Ok(())
        })();
        match result {
            Ok(()) => IpcResponse::success(
                "agent_reflex_preference",
                Some(serde_json::json!({
                    "message": format!("Stored learned reflex preference '{preference_key}'.")
                })),
            ),
            Err(err) => {
                error!("Failed to store learned reflex preference: {err}");
                IpcResponse::error(
                    "upsert_agent_reflex_preference",
                    "STORAGE_ERROR",
                    err.to_string(),
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_get_agent_reflex_preferences(
        agent_id: String,
        preference_key: Option<String>,
    ) -> IpcResponse {
        let path = agent_graph_db_path(&agent_id);
        let result = (|| -> anyhow::Result<Vec<serde_json::Value>> {
            if !path.exists() {
                return Ok(vec![]);
            }
            let storage = SqliteAgentGraphStorage::open(&agent_id, &path)?;
            let preferences = if let Some(key) = preference_key {
                storage
                    .get_reflex_preference(&key)?
                    .map(|r| vec![r])
                    .unwrap_or_default()
            } else {
                storage.list_reflex_preferences()?
            };
            Ok(preferences
                .into_iter()
                .map(|p| {
                    serde_json::json!({
                        "agent_id": p.agent_id,
                        "preference_key": p.preference_key,
                        "precedence": p.precedence,
                        "reflexes": p.reflexes_json,
                        "config": p.config_json,
                        "updated_at": p.updated_at,
                    })
                })
                .collect())
        })();
        match result {
            Ok(rows) => IpcResponse::AgentReflexPreferences { rows },
            Err(err) => {
                error!("Failed to read reflex preferences: {err}");
                IpcResponse::error(
                    "get_agent_reflex_preferences",
                    "STORAGE_ERROR",
                    err.to_string(),
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_upsert_routing_pipeline_rule(
        agent_id: String,
        rule_id: String,
        rule_json: serde_json::Value,
    ) -> IpcResponse {
        use ansible_mesh_core::agent_graph_storage::RoutingPipelineRule;
        let path = agent_graph_db_path(&agent_id);
        let result = (|| -> anyhow::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let storage = SqliteAgentGraphStorage::open(&agent_id, &path)?;
            storage.upsert_pipeline_rule(&RoutingPipelineRule {
                agent_id: agent_id.clone(),
                rule_id: rule_id.clone(),
                rule_json,
                updated_at: 0,
            })?;
            Ok(())
        })();
        match result {
            Ok(()) => IpcResponse::success(
                "routing_pipeline_rule",
                Some(serde_json::json!({
                    "message": format!("Routing pipeline rule '{rule_id}' stored. Takes effect on the next inbound turn.")
                })),
            ),
            Err(err) => {
                error!("Failed to store routing pipeline rule: {err}");
                IpcResponse::error(
                    "upsert_routing_pipeline_rule",
                    "STORAGE_ERROR",
                    err.to_string(),
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_remove_routing_pipeline_rule(
        agent_id: String,
        rule_id: String,
    ) -> IpcResponse {
        let path = agent_graph_db_path(&agent_id);
        let result = (|| -> anyhow::Result<bool> {
            if !path.exists() {
                return Ok(false);
            }
            let storage = SqliteAgentGraphStorage::open(&agent_id, &path)?;
            storage.remove_pipeline_rule(&rule_id)
        })();
        match result {
            Ok(deleted) => IpcResponse::success(
                "routing_pipeline_rule",
                Some(serde_json::json!({
                    "message": if deleted {
                        format!("Routing pipeline rule '{rule_id}' removed.")
                    } else {
                        format!("Routing pipeline rule '{rule_id}' not found.")
                    }
                })),
            ),
            Err(err) => {
                error!("Failed to remove routing pipeline rule: {err}");
                IpcResponse::error(
                    "remove_routing_pipeline_rule",
                    "STORAGE_ERROR",
                    err.to_string(),
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_get_routing_pipeline_rules(
        agent_id: String,
        rule_id: Option<String>,
    ) -> IpcResponse {
        let path = agent_graph_db_path(&agent_id);
        let result = (|| -> anyhow::Result<Vec<serde_json::Value>> {
            if !path.exists() {
                return Ok(vec![]);
            }
            let storage = SqliteAgentGraphStorage::open(&agent_id, &path)?;
            let rules = if let Some(id) = rule_id {
                storage
                    .get_pipeline_rule(&id)?
                    .map(|r| vec![r])
                    .unwrap_or_default()
            } else {
                storage.list_pipeline_rules()?
            };
            Ok(rules
                .into_iter()
                .map(|r| {
                    serde_json::json!({
                        "agent_id": r.agent_id,
                        "rule_id": r.rule_id,
                        "rule": r.rule_json,
                        "updated_at": r.updated_at,
                    })
                })
                .collect())
        })();
        match result {
            Ok(pipeline_rules) => IpcResponse::RoutingPipelineRules { pipeline_rules },
            Err(err) => {
                error!("Failed to read routing pipeline rules: {err}");
                IpcResponse::error(
                    "get_routing_pipeline_rules",
                    "STORAGE_ERROR",
                    err.to_string(),
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_record_role_handoff_reflex_evidence(
        agent_id: String,
        role_name: String,
        legacy_trigger_class: Option<String>,
        source_turn: Option<String>,
        graph: &GraphDomain,
    ) -> IpcResponse {
        use ansible_mesh_core::agent_graph_storage::AgentReflexPreference;
        let path = agent_graph_db_path(&agent_id);
        let result = (|| -> anyhow::Result<serde_json::Value> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let storage = SqliteAgentGraphStorage::open(&agent_id, &path)?;
            let role_record = graph.get_role_incarnation(&agent_id, &role_name)?;
            let toolset_profile = role_record
                .as_ref()
                .map(|role| role.toolset_profile.clone());
            let toolset_record = toolset_profile
                .as_deref()
                .and_then(|profile_name| graph.get_toolset_profile(profile_name).ok().flatten());
            let preference_key = format!("same-self-role-handoff:{role_name}");
            let existing = storage.get_reflex_preference(&preference_key)?;
            let previous_count = existing
                .as_ref()
                .and_then(|pref| pref.config_json.get("success_count"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let success_count = previous_count + 1;
            let reinforced = success_count >= 2;
            let existing_precedence = existing.as_ref().map(|pref| pref.precedence).unwrap_or(70);
            let updated_at = existing.as_ref().map(|pref| pref.updated_at).unwrap_or(0);
            let existing_config = existing
                .as_ref()
                .map(|pref| pref.config_json.clone())
                .unwrap_or_default();
            let toolset_profile = toolset_profile.or_else(|| {
                existing_config
                    .get("toolset_profile")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            });
            let toolset_description = toolset_record
                .as_ref()
                .and_then(|profile| profile.description.clone())
                .or_else(|| {
                    existing_config
                        .get("toolset_description")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                });
            let allowed_skills = toolset_record
                .as_ref()
                .map(|profile| profile.allowed_skills.clone())
                .filter(|skills| !skills.is_empty())
                .or_else(|| {
                    existing_config
                        .get("allowed_skills")
                        .and_then(serde_json::Value::as_array)
                        .map(|skills| {
                            skills
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .filter(|skills| !skills.is_empty())
                })
                .unwrap_or_default();
            let role_identity_addendum = role_record
                .as_ref()
                .and_then(|role| role.role_identity_addendum.clone())
                .or_else(|| {
                    existing_config
                        .get("role_identity_addendum")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                });
            let role_manifest_excerpt = role_record
                .as_ref()
                .and_then(|role| role.role_manifest.as_deref())
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(|text| text.chars().take(180).collect::<String>())
                .or_else(|| {
                    existing_config
                        .get("role_manifest_excerpt")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                });
            let manifest_instructed = role_record
                .as_ref()
                .and_then(|role| role.role_manifest.as_ref())
                .is_some()
                || existing_config
                    .get("manifest_instructed")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
            let manifest_markers = {
                let mut sources = Vec::new();
                sources.push(role_name.as_str());
                if let Some(text) = role_identity_addendum.as_deref() {
                    sources.push(text);
                }
                if let Some(text) = role_manifest_excerpt.as_deref() {
                    sources.push(text);
                }
                let collected = collect_role_receptor_markers(&sources);
                if collected.is_empty() {
                    existing_config
                        .get("manifest_markers")
                        .and_then(serde_json::Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                } else {
                    collected
                }
            };
            let skill_markers = if !allowed_skills.is_empty() {
                collect_role_receptor_markers(
                    &allowed_skills
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                )
            } else {
                existing_config
                    .get("skill_markers")
                    .and_then(serde_json::Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            };
            let toolset_markers = {
                let mut sources = Vec::new();
                if let Some(text) = toolset_profile.as_deref() {
                    sources.push(text);
                }
                if let Some(text) = toolset_description.as_deref() {
                    sources.push(text);
                }
                let collected = collect_role_receptor_markers(&sources);
                if collected.is_empty() {
                    existing_config
                        .get("toolset_markers")
                        .and_then(serde_json::Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                } else {
                    collected
                }
            };

            storage.upsert_reflex_preference(&AgentReflexPreference {
                        agent_id: agent_id.clone(),
                        preference_key: preference_key.clone(),
                        precedence: existing_precedence,
                        reflexes_json: serde_json::json!({
                            "role_handoff_reflex": {
                                "target_role": role_name,
                                "trigger_class": legacy_trigger_class,
                                "source": "successful_same_self_handoff",
                                "tool_name": "handoff.to_role",
                            }
                        }),
                        config_json: serde_json::json!({
                            "reason": format!("remembered successful same-self handoff to role '{role_name}'"),
                            "role_name": role_name,
                            "trigger_class": legacy_trigger_class,
                            "source_tool": "handoff.to_role",
                            "source_turn": source_turn,
                            "toolset_profile": toolset_profile,
                            "toolset_description": toolset_description,
                            "allowed_skills": allowed_skills,
                            "role_identity_addendum": role_identity_addendum,
                            "role_manifest_excerpt": role_manifest_excerpt,
                            "manifest_markers": manifest_markers,
                            "skill_markers": skill_markers,
                            "toolset_markers": toolset_markers,
                            "workflow_skill": "handoff.to_role",
                            "manifest_instructed": manifest_instructed,
                            "success_count": success_count,
                            "habit_state": if reinforced { "reinforced" } else { "candidate" },
                        }),
                        updated_at,
                    })?;
            Ok(serde_json::json!({
                "preference_key": preference_key,
                "success_count": success_count,
                "habit_state": if reinforced { "reinforced" } else { "candidate" },
            }))
        })();
        match result {
            Ok(payload) => IpcResponse::success("role_handoff_reflex_evidence", Some(payload)),
            Err(err) => {
                error!("Failed to record role handoff reflex evidence: {err}");
                IpcResponse::error(
                    "record_role_handoff_reflex_evidence",
                    "STORAGE_ERROR",
                    err.to_string(),
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_append_routing_policy_evaluation(
        proposal_id: String,
        evaluation_kind: String,
        decision: String,
        reason: String,
        source_tool: Option<String>,
        graph: &GraphDomain,
    ) -> IpcResponse {
        match graph.append_routing_policy_evaluation(
            &proposal_id,
            ansible_mesh_core::graph::RoutingPolicyEvaluationRecord {
                evaluation_kind,
                decision,
                reason,
                created_at: unix_ts(),
                source_tool,
            },
        ) {
            Ok(true) => IpcResponse::success(
                "routing_policy_evaluation",
                Some(serde_json::json!({
                    "proposal_id": proposal_id,
                })),
            ),
            Ok(false) => IpcResponse::error(
                "routing_policy_evaluation",
                "NOT_FOUND",
                format!("unknown routing policy proposal '{}'", proposal_id),
            ),
            Err(err) => {
                error!("Failed to append routing policy evaluation: {err}");
                IpcResponse::error(
                    "routing_policy_evaluation",
                    "STORAGE_ERROR",
                    err.to_string(),
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_set_routing_policy_disposition(
        proposal_id: String,
        state: String,
        reason: String,
        source_tool: Option<String>,
        graph: &GraphDomain,
    ) -> IpcResponse {
        match graph.set_routing_policy_disposition(
            &proposal_id,
            state.clone(),
            reason.clone(),
            unix_ts(),
            source_tool,
        ) {
            Ok(true) => IpcResponse::success(
                "routing_policy_disposition",
                Some(serde_json::json!({
                    "proposal_id": proposal_id,
                    "state": state,
                    "reason": reason,
                })),
            ),
            Ok(false) => IpcResponse::error(
                "routing_policy_disposition",
                "NOT_FOUND",
                format!("unknown routing policy proposal '{}'", proposal_id),
            ),
            Err(err) => {
                error!("Failed to set routing policy disposition: {err}");
                IpcResponse::error(
                    "routing_policy_disposition",
                    "STORAGE_ERROR",
                    err.to_string(),
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_get_router_stats(window_secs: Option<u64>) -> IpcResponse {
        use ansible_mesh_core::router_trace::{RouterTraceStorage, SqliteRouterTraceStorage};
        use std::time::{SystemTime, UNIX_EPOCH};

        let trace_db_path = {
            let profile = std::env::var("PHILOTIC_PROFILE")
                .ok()
                .filter(|s| !s.is_empty());
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            match profile {
                Some(p) => format!("{home}/.philotic/{p}/router_traces.db"),
                None => format!("{home}/.philotic/router_traces.db"),
            }
        };

        let generated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        match SqliteRouterTraceStorage::open(&trace_db_path) {
            Ok(store) => match store.provider_stats(window_secs) {
                Ok(stats) => IpcResponse::RouterStats {
                    stats,
                    generated_at,
                },
                Err(e) => IpcResponse::error(
                    "router_stats",
                    "STATS_QUERY_FAILED",
                    &format!("failed to compute router stats: {e}"),
                ),
            },
            Err(e) => IpcResponse::error(
                "router_stats",
                "TRACE_DB_UNAVAILABLE",
                &format!("router trace DB not available at {trace_db_path}: {e}"),
            ),
        }
    }
}
