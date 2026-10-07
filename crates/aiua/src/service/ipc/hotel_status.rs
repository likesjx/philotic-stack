//! Hotel status: status snapshot, memory report, cortex read, best place to run, logs.
//!
//! Handler bodies moved verbatim from the `process_request` match in
//! `ipc/mod.rs` (IPC_DISPATCH_SPLIT); parameters keep their declared types.

use super::*;

impl IpcServer {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_get_hotel_status(
        local_node_id: &str,
        graph: &GraphDomain,
        registry: &Arc<RwLock<NodeRegistry>>,
    ) -> IpcResponse {
        let hotel_name =
            Self::local_hotel_name(graph, local_node_id).unwrap_or_else(|| "unknown".to_string());

        let guests: Vec<serde_json::Value> = graph
            .list_guests(&hotel_name, false)
            .unwrap_or_default()
            .into_iter()
            .map(|g| {
                serde_json::json!({
                    "guest_id": g.guest_id,
                    "role": g.role,
                    "active": g.is_active,
                })
            })
            .collect();

        let agents: Vec<serde_json::Value> = graph
            .list_agent_identities()
            .unwrap_or_default()
            .into_iter()
            .map(|id| {
                serde_json::json!({
                    "agent_id": id.agent_id,
                    "persona_name": id.persona_name,
                })
            })
            .collect();

        let reg = registry.read().await;
        let mesh_peers: Vec<serde_json::Value> = reg
            .remote_hotel_states()
            .map(|state| {
                let peer_guests: Vec<serde_json::Value> = state
                    .guests
                    .iter()
                    .map(|g| {
                        serde_json::json!({
                            "guest_id": g.guest_id,
                            "role": g.role,
                            "active": g.active,
                        })
                    })
                    .collect();
                let peer_agents: Vec<serde_json::Value> = state
                    .agents
                    .iter()
                    .map(|a| {
                        serde_json::json!({
                            "agent_id": a.agent_id,
                            "persona_name": a.persona_name,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "hotel_name": state.hotel_name,
                    "node_id": state.node_id,
                    "guests": peer_guests,
                    "agents": peer_agents,
                })
            })
            .collect();
        drop(reg);

        IpcResponse::success(
            "hotel_status",
            Some(serde_json::json!({
                "hotel_name": hotel_name,
                "node_id": local_node_id,
                "guests": guests,
                "agents": agents,
                "mesh_peers": mesh_peers,
            })),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_get_memory_report(graph: &GraphDomain) -> IpcResponse {
        // Proposal S6a: honest-sourcing memory-health report. Read-only;
        // sources recall effectiveness from the session-event ledger and
        // marks the multi-node/replication fields `unavailable`.
        let report = crate::memory_report::assemble_live_memory_report(graph);
        IpcResponse::success("memory_report", serde_json::to_value(&report).ok())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_read_cortex(
        vault: Option<String>,
        id: Option<String>,
        offset: u32,
        local_node_id: &str,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        if !current_identity.as_ref().is_some_and(|identity| {
            identity.role == "management" && identity.guest_id == "philotic-web-cortex"
        }) {
            return IpcResponse::error(
                "cortex",
                "FORBIDDEN",
                "Cortex reads require the operator management adapter",
            );
        }
        match tokio::time::timeout(
            std::time::Duration::from_secs(35),
            crate::cortex_viewer::read(graph, local_node_id, vault, id, offset),
        )
        .await
        {
            Ok(Ok(data)) => IpcResponse::success("cortex", Some(data)),
            _ => {
                tracing::warn!("Cortex read failed or exceeded deadline");
                IpcResponse::error(
                    "cortex",
                    "UNAVAILABLE",
                    "Cortex read unavailable; check hotel configuration and connectivity",
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_best_place_to_run(
        agent_id: Option<String>,
        role_name: Option<String>,
        tool_name: Option<String>,
        required_markers: Vec<String>,
        prefer_locality: bool,
        local_node_id: &str,
        graph: &GraphDomain,
        registry: &Arc<RwLock<NodeRegistry>>,
    ) -> IpcResponse {
        match Self::best_place_to_run_view(
            registry,
            graph,
            local_node_id,
            agent_id.as_deref(),
            role_name.as_deref(),
            tool_name.as_deref(),
            &required_markers,
            prefer_locality,
        )
        .await
        {
            Ok(view) => IpcResponse::success("best_place_to_run", Some(view)),
            Err(err) => IpcResponse::error("best_place_to_run", "PLACEMENT_ERROR", err.to_string()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_get_hotel_logs(lines: u32) -> IpcResponse {
        // Compute log path using the same PHILOTIC_PROFILE convention as the DB path.
        let log_path = std::env::var("PHILOTIC_PROFILE")
            .ok()
            .filter(|s| !s.is_empty())
            .and_then(|profile| {
                std::env::var("HOME").ok().map(|home| {
                    std::path::PathBuf::from(home)
                        .join(".philotic")
                        .join(profile)
                        .join("aiua.log")
                })
            })
            .or_else(|| {
                std::env::var("HOME").ok().map(|home| {
                    std::path::PathBuf::from(home)
                        .join(".philotic")
                        .join("aiua.log")
                })
            });

        match log_path.and_then(|p| std::fs::read_to_string(&p).ok()) {
            Some(content) => {
                let all_lines: Vec<&str> = content.lines().collect();
                let start = all_lines.len().saturating_sub(lines as usize);
                let tail = all_lines[start..].join("\n");
                IpcResponse::success("hotel_logs", Some(serde_json::json!({ "log": tail })))
            }
            None => IpcResponse::error(
                "hotel_logs",
                "LOG_NOT_FOUND",
                "Hotel log file not found — check ~/.philotic/aiua.log",
            ),
        }
    }
}
