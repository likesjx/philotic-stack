//! Native runner definitions are connection-scoped; persisted endpoint policy
//! is the authority. A hotel restart cannot resurrect a disconnected runner.
use super::*;
use ansible_mesh_core::mcp_endpoint::{McpEndpointConfig, McpInboundTransform};
use ansible_mesh_core::mcp_route::{McpAuthScheme, McpRouteTarget};
use philotic_client::{RunnerToolAdvertisement, RunnerToolDescriptor};

/// Telemetry is a narrow projection, never a copy of an IPC envelope. Opaque
/// refs correlate events without putting arbitrary identity strings in logs.
pub(super) struct NativeToolObservation<'a> {
    pub phase: &'static str,
    pub allowed: bool,
    pub endpoint: &'a str,
    pub exposed_tool: &'a str,
    pub caller: &'a str,
    pub session: &'a str,
    pub turn: &'a str,
    pub operation: Option<&'a str>,
    pub elapsed: std::time::Duration,
}

impl NativeToolObservation<'_> {
    fn metadata(&self) -> serde_json::Value {
        let reference = |value: &str| blake3::hash(value.as_bytes()).to_hex()[..32].to_owned();
        serde_json::json!({"event":"native_graph_transaction","phase":self.phase,
            "policy_outcome":if self.allowed {"allowed"}else{"denied"},
            "policy_code":if self.allowed {"ADMITTED"}else{"DENIED"},
            "endpoint_ref":reference(self.endpoint),"tool_ref":reference(self.exposed_tool),
            "caller_ref":reference(self.caller),"session_ref":reference(self.session),"turn_ref":reference(self.turn),
            "operation":self.operation,"admission_duration_us":self.elapsed.as_micros().min(u64::MAX as u128) as u64})
    }
    pub(super) fn emit(self) {
        info!(native_graph_transaction = %self.metadata(), "Native graph policy transaction");
    }
}

#[cfg(test)]
mod tests;

impl IpcServer {
    pub(super) async fn project_runner_definitions(
        inboxes: &InboxRegistry,
        assembly: &mut serde_json::Value,
        node: &str,
    ) {
        let guard = inboxes.lock().await;
        let Some(tools) = assembly["tools_for_model"].as_array().cloned() else {
            return;
        };
        for (index, tool) in tools.iter().enumerate() {
            let Some(name) = tool["tool_name"].as_str() else {
                continue;
            };
            let route = &assembly["execution_routes"][name];
            let Some(runner_id) = route["runner_id"].as_str() else {
                continue;
            };
            // Reuse the hotel's already-selected authorized route. Publication
            // never adds a tool to the model's allowed set or selects an owner.
            if route["target_node"] != node {
                continue;
            }
            let Some(runner) = guard.get("life-graph-runner").and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.guest_id == runner_id && !entry.tx.is_closed())
            }) else {
                continue;
            };
            let Some(descriptor) = runner
                .runner_tools
                .as_ref()
                .and_then(|ad| ad.tools.iter().find(|descriptor| descriptor.name == name))
            else {
                continue;
            };
            assembly["tools_for_model"][index]["description"] =
                serde_json::json!(descriptor.description);
            assembly["tools_for_model"][index]["input_schema"] = descriptor.input_schema.clone();
            assembly["execution_routes"][name]["descriptor_version"] = descriptor.version.into();
            assembly["execution_routes"][name]["target_role"] = "life-graph-runner".into();
        }
    }
    pub(super) async fn advertise_runner_tools(
        graph: &GraphDomain,
        local_node_id: &str,
        inboxes: &InboxRegistry,
        conn_id: Uuid,
        identity: Option<&GuestIdentity>,
        advertisement: RunnerToolAdvertisement,
    ) -> IpcResponse {
        let apply = async {
            advertisement.validate().map_err(anyhow::Error::msg)?;
            let identity = identity.ok_or_else(|| anyhow::anyhow!("unregistered runner"))?;
            let hotel = Self::local_hotel_name(graph, local_node_id)
                .ok_or_else(|| anyhow::anyhow!("unknown hotel"))?;
            let guest = graph
                .get_guest(&hotel, &identity.guest_id)?
                .ok_or_else(|| anyhow::anyhow!("runner is not a hotel-managed guest"))?;
            // This first increment enrolls LifeGraph's read surface only. The
            // generic graph datasource and raw Cypher remain internal.
            anyhow::ensure!(
                guest.is_active
                    && guest.role == identity.role
                    && identity.role == "life-graph-runner",
                "runner role is not enrolled"
            );
            anyhow::ensure!(
                advertisement
                    .tools
                    .iter()
                    .all(|tool| tool.name.starts_with("life.")
                        && tool.read_only
                        && identity.supported_tools.contains(&tool.name)),
                "tool outside runner's enrolled scope"
            );
            let mut guard = inboxes.lock().await;
            let subscriber = guard
                .get_mut(&identity.role)
                .and_then(|entries| {
                    entries.iter_mut().find(|entry| {
                        entry.conn_id == conn_id
                            && entry.guest_id == identity.guest_id
                            && !entry.tx.is_closed()
                    })
                })
                .ok_or_else(|| anyhow::anyhow!("superseded runner connection"))?;
            if let Some(previous) = &subscriber.runner_tools {
                if previous == &advertisement {
                    return Ok(());
                }
                anyhow::ensure!(
                    advertisement.revision > previous.revision,
                    "stale or conflicting advertisement"
                );
                for tool in &advertisement.tools {
                    if let Some(old) = previous.tools.iter().find(|old| old.name == tool.name) {
                        anyhow::ensure!(
                            old == tool || tool.version > old.version,
                            "changed descriptor requires a new tool version"
                        );
                    }
                }
            }
            // Persist contract history, never connection liveness. The same
            // version cannot silently acquire different semantics on restart.
            let key = format!("__runner_tool_versions__:{}", identity.guest_id);
            let mut history: std::collections::BTreeMap<String, RunnerToolDescriptor> = graph
                .get_config_value(&key)?
                .map(|raw| serde_json::from_str(&raw))
                .transpose()?
                .unwrap_or_default();
            for tool in &advertisement.tools {
                if let Some(old) = history.get(&tool.name) {
                    anyhow::ensure!(
                        tool.version >= old.version && (tool.version != old.version || tool == old),
                        "persisted tool version is stale or conflicting"
                    );
                }
                history.insert(tool.name.clone(), tool.clone());
            }
            graph.set_config_value(&key, &serde_json::to_string(&history)?)?;
            subscriber.runner_tools = Some(advertisement);
            Ok::<_, anyhow::Error>(())
        }
        .await;
        match apply {
            Ok(()) => IpcResponse::success("runner_tools", None),
            Err(error) => IpcResponse::error(
                "runner_tools",
                "RUNNER_ADVERTISEMENT_DENIED",
                error.to_string(),
            ),
        }
    }

    pub(super) async fn resolve_runner_tool(
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        identity: Option<&GuestIdentity>,
        endpoint_id: &str,
        exposed_name: &str,
        caller_id: &str,
    ) -> anyhow::Result<(
        RunnerToolDescriptor,
        RoleSubscriber,
        McpEndpointConfig,
        String,
    )> {
        let membrane = format!("mcp-membrane-{endpoint_id}");
        let identity = identity.ok_or_else(|| anyhow::anyhow!("unregistered membrane"))?;
        anyhow::ensure!(
            (identity.role == "mcp-membrane" && identity.guest_id == membrane)
                || (identity.role == "mcp-catalog"
                    && identity.guest_id == format!("{membrane}:catalog")),
            "endpoint identity mismatch"
        );
        let lease = leases
            .lock()
            .await
            .inspect(&membrane)
            .ok_or_else(|| anyhow::anyhow!("membrane lease missing"))?;
        anyhow::ensure!(
            lease.owner_guest_id == membrane
                && lease.is_active()
                && lease.lease_expires_at > unix_ts(),
            "membrane lease expired"
        );
        let membrane_conn = {
            let guard = inboxes.lock().await;
            guard
                .get("mcp-membrane")
                .and_then(|entries| {
                    entries
                        .iter()
                        .find(|entry| entry.guest_id == membrane && !entry.tx.is_closed())
                })
                .map(|entry| entry.conn_id)
                .ok_or_else(|| anyhow::anyhow!("membrane disconnected"))?
        };
        anyhow::ensure!(
            leases
                .lock()
                .await
                .active_leases_for_connection(membrane_conn)
                .iter()
                .any(|lease| lease.lease_scope == membrane
                    && lease.is_active()
                    && lease.lease_expires_at > unix_ts()),
            "membrane connection superseded"
        );
        let config: McpEndpointConfig = serde_json::from_str(
            &graph
                .get_config_value(&format!("__mcp_endpoint__:{endpoint_id}"))?
                .ok_or_else(|| anyhow::anyhow!("endpoint unavailable"))?,
        )?;
        let spec = config
            .tools
            .iter()
            .find(|spec| spec.name == exposed_name)
            .ok_or_else(|| anyhow::anyhow!("tool not granted to endpoint"))?;
        match config.effective_auth(spec) {
            McpAuthScheme::BearerToken { grants } => anyhow::ensure!(
                grants.iter().any(|grant| grant.token_id == caller_id
                    && grant.expires_at.is_none_or(|expiry| unix_ts() <= expiry)
                    && grant.scopes.iter().any(|scope| scope == exposed_name)),
                "caller grant missing, expired, or outside tool scope"
            ),
            McpAuthScheme::None => anyhow::bail!("native runner requires bearer authentication"),
        }
        let McpInboundTransform::FieldMap {
            action,
            target:
                McpRouteTarget::RunnerTool {
                    runner_id,
                    tool_name,
                    version,
                },
            mappings,
        } = &spec.inbound_transform
        else {
            anyhow::bail!("not a native runner binding")
        };
        anyhow::ensure!(
            action == tool_name && mappings.is_empty(),
            "native contract cannot be transformed"
        );
        let guest = graph
            .get_guest(&lease.authority_hotel, runner_id)?
            .ok_or_else(|| anyhow::anyhow!("runner enrollment removed"))?;
        anyhow::ensure!(
            guest.is_active && guest.role == "life-graph-runner",
            "runner enrollment disabled"
        );
        anyhow::ensure!(
            config
                .preapproval_rules
                .iter()
                .any(|rule| rule.action_pattern == *tool_name
                    && rule.target.as_ref()
                        == Some(match &spec.inbound_transform {
                            McpInboundTransform::FieldMap { target, .. } => target,
                            _ => unreachable!(),
                        })
                    && rule.expires_at.is_some_and(|expiry| unix_ts() <= expiry)),
            "native runner requires finite target-bound preapproval"
        );
        let guard = inboxes.lock().await;
        let runner = guard
            .get("life-graph-runner")
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.guest_id == *runner_id && !entry.tx.is_closed())
            })
            .ok_or_else(|| anyhow::anyhow!("runner disconnected"))?;
        let descriptor = runner
            .runner_tools
            .as_ref()
            .and_then(|ad| {
                ad.tools
                    .iter()
                    .find(|tool| tool.name == *tool_name && tool.version == *version)
            })
            .ok_or_else(|| anyhow::anyhow!("runner descriptor missing or stale"))?;
        // Return a single descriptor, never the runner's full catalog.
        Ok((descriptor.clone(), runner.clone(), config, membrane))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn invoke_runner_tool(
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        identity: Option<&GuestIdentity>,
        conn_id: Uuid,
        node: &str,
        endpoint_id: &str,
        exposed_name: &str,
        version: u32,
        arguments: serde_json::Value,
        caller_id: &str,
        session_id: &str,
        turn_id: &str,
    ) -> IpcResponse {
        let started = std::time::Instant::now();
        let mut operation = None;
        let dispatch = async {
            let (descriptor, runner, config, membrane) = Self::resolve_runner_tool(
                graph,
                inboxes,
                leases,
                identity,
                endpoint_id,
                exposed_name,
                caller_id,
            )
            .await?;
            operation = Some(descriptor.name.clone());
            anyhow::ensure!(
                identity.is_some_and(|id| id.role == "mcp-membrane" && id.guest_id == membrane),
                "catalog connection cannot invoke"
            );
            anyhow::ensure!(
                leases
                    .lock()
                    .await
                    .active_leases_for_connection(conn_id)
                    .iter()
                    .any(|lease| lease.lease_scope == membrane
                        && lease.is_active()
                        && lease.lease_expires_at > unix_ts()),
                "connection does not hold membrane lease"
            );
            anyhow::ensure!(
                descriptor.version == version && !session_id.is_empty() && !turn_id.is_empty(),
                "stale version or missing correlation"
            );
            let spec = config
                .tools
                .iter()
                .find(|spec| spec.name == exposed_name)
                .unwrap();
            match config.effective_auth(spec) {
                McpAuthScheme::BearerToken { grants } => anyhow::ensure!(
                    grants.iter().any(|grant| grant.token_id == caller_id
                        && grant.expires_at.is_none_or(|expiry| unix_ts() <= expiry)
                        && grant.scopes.iter().any(|scope| scope == exposed_name)),
                    "caller grant missing, expired, or outside tool scope"
                ),
                // External native calls always need an explicit bearer grant.
                McpAuthScheme::None => {
                    anyhow::bail!("native runner requires bearer authentication")
                }
            }
            anyhow::ensure!(
                config
                    .preapproval_rules
                    .iter()
                    .any(|rule| rule.action_pattern == descriptor.name
                        && rule.target.as_ref()
                            == match &spec.inbound_transform {
                                McpInboundTransform::FieldMap { target, .. } => Some(target),
                                _ => None,
                            }
                        && rule.expires_at.is_some_and(|expiry| unix_ts() <= expiry)),
                "native runner requires finite target-bound preapproval"
            );
            descriptor
                .validate_arguments(&arguments)
                .map_err(anyhow::Error::msg)?;
            let payload = serde_json::json!({"action":"execute_tool", "tool_name":descriptor.name,
                "arguments":arguments,"identity":{"agent_id":format!("mcp:{caller_id}")},
                "session_id":session_id,"turn_id":turn_id,
                "reply_to":node,"reply_role":"mcp-membrane","reply_guest_id":membrane});
            // Keep the registry locked through queue admission. Replacement or
            // disconnect cannot redirect this call to an older incarnation.
            let guard = inboxes.lock().await;
            anyhow::ensure!(
                guard
                    .get("mcp-membrane")
                    .is_some_and(
                        |entries| entries.iter().any(|entry| entry.conn_id == conn_id
                            && entry.guest_id == membrane
                            && !entry.tx.is_closed())
                    ),
                "membrane connection superseded"
            );
            let latest = graph
                .get_config_value(&format!("__mcp_endpoint__:{endpoint_id}"))?
                .ok_or_else(|| anyhow::anyhow!("endpoint revoked before admission"))?;
            anyhow::ensure!(
                serde_json::from_str::<McpEndpointConfig>(&latest)? == config,
                "endpoint changed before admission"
            );
            anyhow::ensure!(
                guard
                    .get("life-graph-runner")
                    .is_some_and(|entries| entries
                        .iter()
                        .any(|entry| entry.conn_id == runner.conn_id
                            && entry.runner_tools == runner.runner_tools
                            && !entry.tx.is_closed())),
                "runner replaced before admission"
            );
            runner
                .tx
                .send(IpcResponse::InboundTask {
                    source_node: node.into(),
                    task_id: Uuid::new_v4(),
                    task_json: payload.to_string(),
                })
                .map_err(|_| anyhow::anyhow!("runner delivery failed"))?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        NativeToolObservation {
            phase: "invoke",
            allowed: dispatch.is_ok(),
            endpoint: endpoint_id,
            exposed_tool: exposed_name,
            caller: caller_id,
            session: session_id,
            turn: turn_id,
            operation: operation.as_deref(),
            elapsed: started.elapsed(),
        }
        .emit();
        match dispatch {
            Ok(()) => IpcResponse::success("runner_tool", None),
            Err(error) => {
                IpcResponse::error("runner_tool", "RUNNER_TOOL_DENIED", error.to_string())
            }
        }
    }
}
