//! Narrow approval maintenance. Never writes guest records or launch state.
use super::*;
use crate::mcp_endpoint::McpEndpointConfig;
use crate::mcp_route::McpRouteTarget;
use sha2::{Digest, Sha256};

/// Stable digest of the complete JSON configuration, including unknown fields.
pub fn endpoint_digest(value: &serde_json::Value) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(value)?)))
}

impl GraphDomain {
    /// Compare-and-swap the endpoint and its approval index in one durable
    /// transaction. The caller must already have local OS-account administrative
    /// authority. Registered owner context is checked within the CAS snapshot;
    /// legacy registration is not independent per-agent authentication.
    pub fn extend_native_preapprovals(
        &self,
        endpoint: &str,
        expected_owner: &str,
        expected_digest: &str,
        new_expiry: u64,
        approval: &str,
        now: u64,
    ) -> Result<(String, McpEndpointConfig)> {
        anyhow::ensure!(
            !approval.trim().is_empty() && approval.len() <= 256,
            "invalid approval reference"
        );
        anyhow::ensure!(
            new_expiry > now && new_expiry <= now.saturating_add(86400),
            "expiry must be within 24 hours"
        );
        let key = Self::config_key(&format!("__mcp_endpoint__:{endpoint}"));
        let old = self.adapter.get_node(&key)?.context("endpoint missing")?;
        let raw = old
            .data
            .get("value")
            .and_then(|v| v.as_str())
            .context("endpoint malformed")?;
        let mut value: serde_json::Value = serde_json::from_str(raw)?;
        anyhow::ensure!(
            endpoint_digest(&value)? == expected_digest,
            "endpoint drift"
        );
        let config: McpEndpointConfig = serde_json::from_value(value.clone())?;
        anyhow::ensure!(config.owner_agent_id == expected_owner, "owner drift");
        anyhow::ensure!(
            config.endpoint_id == endpoint && !config.owner_agent_id.is_empty(),
            "endpoint identity mismatch"
        );
        anyhow::ensure!(
            config.preapproval_rules.len() == 2,
            "exactly two native approvals required"
        );
        let mut actions = std::collections::BTreeSet::new();
        let mut target = None;
        for rule in &config.preapproval_rules {
            anyhow::ensure!(
                matches!(
                    rule.action_pattern.as_str(),
                    "native-memory.remember" | "native-memory.recall"
                ) && actions.insert(rule.action_pattern.clone()),
                "invalid native action"
            );
            let Some(McpRouteTarget::NativeMemory {
                runner_id,
                target_node,
            }) = &rule.target
            else {
                anyhow::bail!("native destination required");
            };
            anyhow::ensure!(
                !runner_id.is_empty() && !target_node.is_empty(),
                "empty destination"
            );
            if let Some(previous) = &target {
                anyhow::ensure!(previous == &rule.target, "mixed destinations");
            }
            target = Some(rule.target.clone());
            anyhow::ensure!(
                rule.expires_at.is_some_and(|old| new_expiry >= old),
                "finite increasing expiry required"
            );
        }
        let index_key = Self::config_key(&format!("__mcp_preapproval__:{endpoint}"));
        let old_index = self
            .adapter
            .get_node(&index_key)?
            .context("approval index missing")?;
        let rules: serde_json::Value = serde_json::from_str(
            old_index
                .data
                .get("value")
                .and_then(|v| v.as_str())
                .context("approval index malformed")?,
        )?;
        anyhow::ensure!(rules == value["preapproval_rules"], "approval index drift");
        if config
            .preapproval_rules
            .iter()
            .all(|r| r.expires_at == Some(new_expiry) && r.approved_by_turn == approval)
        {
            // Explicit reconciliation after reading the current digest. No
            // durable write or version increment; permits re-enqueueing a lost
            // push. An old request digest still fails above.
            return Ok((expected_digest.to_owned(), config));
        }
        anyhow::ensure!(
            config
                .preapproval_rules
                .iter()
                .all(|r| r.expires_at.is_some_and(|old| new_expiry > old)),
            "inconsistent renewal"
        );
        for rule in value["preapproval_rules"]
            .as_array_mut()
            .context("rules malformed")?
        {
            rule["expires_at"] = new_expiry.into();
            rule["approved_at"] = now.into();
            rule["approved_by_turn"] = approval.into();
        }
        value["updated_at"] = now
            .max(
                config
                    .updated_at
                    .checked_add(1)
                    .context("version overflow")?,
            )
            .into();
        let mut new = old.clone();
        new.data["value"] = serde_json::to_string(&value)?.into();
        let mut new_index = old_index.clone();
        new_index.data["value"] = serde_json::to_string(&value["preapproval_rules"])?.into();
        anyhow::ensure!(
            self.adapter
                .compare_exchange_nodes(&[(old, new), (old_index, new_index)])?,
            "endpoint changed concurrently"
        );
        Ok((endpoint_digest(&value)?, serde_json::from_value(value)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_storage::SqliteGraphStorage;
    use std::sync::Barrier;

    fn seed(graph: &GraphDomain) -> serde_json::Value {
        let value = serde_json::json!({"endpoint_id":"pilot","owner_agent_id":"owner","port":19101,
        "exposure":"local","tools":[],"allow_unauthenticated":false,"updated_at":10,
        "preapproval_rules":[
            {"action_pattern":"native-memory.remember","target":{"kind":"native_memory","runner_id":"runner","target_node":"hotel"},"approved_by_turn":"old","approved_at":10,"expires_at":100},
            {"action_pattern":"native-memory.recall","target":{"kind":"native_memory","runner_id":"runner","target_node":"hotel"},"approved_by_turn":"old","approved_at":10,"expires_at":100}
        ]});
        save(graph, &value);
        value
    }
    fn save(graph: &GraphDomain, v: &serde_json::Value) {
        graph
            .set_config_value("__mcp_endpoint__:pilot", &v.to_string())
            .unwrap();
        graph
            .set_config_value(
                "__mcp_preapproval__:pilot",
                &v["preapproval_rules"].to_string(),
            )
            .unwrap();
    }
    fn graph() -> GraphDomain {
        GraphDomain::new(Arc::new(
            SqliteGraphStorage::open(":memory:").unwrap().adapter(),
        ))
    }
    #[test]
    fn native_maintenance_preserves_unknown_fields_and_rejects_retry_drift() {
        let graph = graph();
        let mut value = seed(&graph);
        value["future_field"] = "preserved".into();
        save(&graph, &value);
        let digest = endpoint_digest(&value).unwrap();
        let (next, _) = graph
            .extend_native_preapprovals("pilot", "owner", &digest, 200, "approval", 20)
            .unwrap();
        assert_ne!(digest, next);
        assert!(graph
            .extend_native_preapprovals("pilot", "owner", &digest, 200, "approval", 20)
            .is_err());
        let actual: serde_json::Value = serde_json::from_str(
            &graph
                .get_config_value("__mcp_endpoint__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(actual["future_field"], "preserved");
        assert_eq!(actual["preapproval_rules"][0]["expires_at"], 200);
    }
    #[test]
    fn native_maintenance_negative_controls_leave_both_records_unchanged() {
        for case in 0..8 {
            let graph = graph();
            let mut value = seed(&graph);
            match case {
                0 => value["preapproval_rules"][0]["action_pattern"] = "*".into(),
                1 => value["preapproval_rules"][0]["target"] = serde_json::Value::Null,
                2 => value["preapproval_rules"][0]["expires_at"] = serde_json::Value::Null,
                3 => {
                    value["preapproval_rules"][1]["action_pattern"] =
                        "native-memory.remember".into()
                }
                4 => value["preapproval_rules"][1]["target"]["runner_id"] = "other".into(),
                _ => {}
            }
            save(&graph, &value);
            if case == 5 {
                graph
                    .set_config_value("__mcp_preapproval__:pilot", "[]")
                    .unwrap();
            }
            let before = graph.get_config_value("__mcp_preapproval__:pilot").unwrap();
            assert!(graph
                .extend_native_preapprovals(
                    "pilot",
                    "owner",
                    &endpoint_digest(&value).unwrap(),
                    if case == 6 {
                        100
                    } else if case == 7 {
                        100000
                    } else {
                        200
                    },
                    "approval",
                    20
                )
                .is_err());
            assert_eq!(
                graph
                    .get_config_value("__mcp_endpoint__:pilot")
                    .unwrap()
                    .unwrap(),
                value.to_string()
            );
            assert_eq!(
                graph.get_config_value("__mcp_preapproval__:pilot").unwrap(),
                before
            );
        }
    }
    #[test]
    fn native_maintenance_concurrent_cas_has_exactly_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hotel.db");
        let first = GraphDomain::new(Arc::new(
            SqliteGraphStorage::open(path.to_str().unwrap())
                .unwrap()
                .adapter(),
        ));
        let value = seed(&first);
        let second = GraphDomain::new(Arc::new(
            SqliteGraphStorage::open(path.to_str().unwrap())
                .unwrap()
                .adapter(),
        ));
        let barrier = Arc::new(Barrier::new(2));
        let workers: Vec<_> = [first.clone(), second]
            .into_iter()
            .enumerate()
            .map(|(i, g)| {
                let b = barrier.clone();
                let digest = endpoint_digest(&value).unwrap();
                std::thread::spawn(move || {
                    b.wait();
                    g.extend_native_preapprovals(
                        "pilot",
                        "owner",
                        &digest,
                        200 + i as u64,
                        "approval",
                        20,
                    )
                    .is_ok()
                })
            })
            .collect();
        assert_eq!(
            workers
                .into_iter()
                .filter_map(|w| w.join().ok())
                .filter(|v| *v)
                .count(),
            1
        );
        let saved: serde_json::Value = serde_json::from_str(
            &first
                .get_config_value("__mcp_endpoint__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let index: serde_json::Value = serde_json::from_str(
            &first
                .get_config_value("__mcp_preapproval__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(saved["preapproval_rules"], index);
    }
    #[test]
    fn native_maintenance_storage_failure_rolls_back_first_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hotel.db");
        let graph = GraphDomain::new(Arc::new(
            SqliteGraphStorage::open(path.to_str().unwrap())
                .unwrap()
                .adapter(),
        ));
        let value = seed(&graph);
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch("CREATE TRIGGER reject_index BEFORE UPDATE ON graph_nodes WHEN NEW.node_key='config:__mcp_preapproval__:pilot' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
        assert!(graph
            .extend_native_preapprovals(
                "pilot",
                "owner",
                &endpoint_digest(&value).unwrap(),
                200,
                "approval",
                20
            )
            .is_err());
        assert_eq!(
            graph
                .get_config_value("__mcp_endpoint__:pilot")
                .unwrap()
                .unwrap(),
            value.to_string()
        );
    }
    #[test]
    fn native_maintenance_owner_drift_is_checked_inside_snapshot() {
        let graph = graph();
        let mut value = seed(&graph);
        value["owner_agent_id"] = "new-owner".into();
        save(&graph, &value);
        // Even a current digest cannot let the previously authorized owner
        // mutate a snapshot belonging to a new owner.
        assert!(graph
            .extend_native_preapprovals(
                "pilot",
                "owner",
                &endpoint_digest(&value).unwrap(),
                200,
                "approval",
                20
            )
            .is_err());
        assert_eq!(
            graph
                .get_config_value("__mcp_endpoint__:pilot")
                .unwrap()
                .unwrap(),
            value.to_string()
        );
    }
}
