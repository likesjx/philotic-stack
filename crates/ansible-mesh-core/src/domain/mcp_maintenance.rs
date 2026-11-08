//! Narrow approval maintenance. Never writes guest records or launch state.
use super::*;
use crate::mcp_endpoint::McpEndpointConfig;
use crate::mcp_route::McpRouteTarget;
use sha2::{Digest, Sha256};

/// Stable digest of the complete JSON configuration, including unknown fields.
pub fn endpoint_digest(value: &serde_json::Value) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(value)?)))
}

// Private fixture input only. It is not authenticated plan authority and is
// never accepted from IPC or an administrative command.
struct ReceiptFixture<'a> {
    plan: &'a crate::maintenance_plan::LoadedMaintenancePlan,
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
        self.extend_native_preapprovals_inner(
            endpoint,
            expected_owner,
            expected_digest,
            new_expiry,
            approval,
            now,
            None,
        )
    }

    fn extend_native_preapprovals_inner(
        &self,
        endpoint: &str,
        expected_owner: &str,
        expected_digest: &str,
        new_expiry: u64,
        approval: &str,
        now: u64,
        receipt: Option<ReceiptFixture<'_>>,
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
            anyhow::ensure!(
                receipt.is_none(),
                "receipt operation requires reconciliation"
            );
            // Explicit reconciliation after reading the current digest. No
            // durable write or version increment; permits re-enqueueing a lost
            // push. An old request digest still fails above.
            anyhow::ensure!(
                self.adapter.compare_exchange_nodes_before(
                    &[(old.clone(), old), (old_index.clone(), old_index)],
                    now,
                    new_expiry,
                )?,
                "endpoint changed concurrently"
            );
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
        let next_digest = endpoint_digest(&value)?;
        let mut updates = vec![(old, new), (old_index, new_index)];
        let mut authorize_until = None;
        if let Some(receipt) = receipt {
            let plan = receipt.plan;
            anyhow::ensure!(
                plan.endpoint() == endpoint
                    && plan.owner() == expected_owner
                    && plan.expected_endpoint_sha256() == expected_digest
                    && plan.rule_expires_at() == new_expiry
                    && plan.approval_reference() == approval,
                "plan operation mismatch"
            );
            let key = Self::config_key(&format!(
                "__native_maintenance_receipt__:{}",
                plan.operation_id()
            ));
            let old_receipt = self
                .adapter
                .get_node(&key)?
                .context("receipt slot missing")?;
            anyhow::ensure!(
                old_receipt.data
                    == serde_json::json!({"state":"empty", "operation":plan.operation_id(), "plan_digest":plan.exact_sha256()}),
                "receipt slot mismatch"
            );
            let mut new_receipt = old_receipt.clone();
            new_receipt.data = serde_json::json!({"state":"committed", "operation":plan.operation_id(),
                "plan_digest":plan.exact_sha256(), "endpoint":endpoint, "owner":expected_owner,
                "old_digest":expected_digest, "new_digest":next_digest, "expiry":new_expiry,
                "approval":approval, "authorize_until":plan.authorize_until()});
            updates.push((old_receipt, new_receipt));
            authorize_until = Some(plan.authorize_until());
        }
        anyhow::ensure!(
            match authorize_until {
                Some(deadline) => self.adapter.compare_exchange_nodes_before_authorized(
                    &updates, now, new_expiry, deadline
                )?,
                None => self
                    .adapter
                    .compare_exchange_nodes_before(&updates, now, new_expiry)?,
            },
            "endpoint changed concurrently"
        );
        Ok((next_digest, serde_json::from_value(value)?))
    }
}

/// Private synthetic-only execution sequence for the real domain path. Its
/// owner and plan expectations are fixture inputs, not administrator
/// authentication or evidence that other writers and activators are stopped.
/// An existing receipt, including an empty one, always requires reconciliation.
#[cfg(all(test, unix))]
pub(crate) fn execute_offline_maintenance_fixture(
    db_path: &std::path::Path,
    plan_path: &std::path::Path,
    uid: u32,
    gid: u32,
    expected: &crate::maintenance_plan::PlanExpectation<'_>,
) -> Result<()> {
    let plan =
        crate::maintenance_plan::read_maintenance_plan_with_owner(plan_path, uid, gid, expected)
            .map_err(|_| anyhow::anyhow!("protected fixture plan refused"))?;
    let storage =
        crate::sqlite_storage::SqliteGraphStorage::open_existing_maintenance_fixture(db_path)?;
    let graph = GraphDomain::new(Arc::new(storage.adapter()));
    let key = GraphDomain::config_key(&format!(
        "__native_maintenance_receipt__:{}",
        plan.operation_id()
    ));
    let slot = GraphNode {
        node_key: key,
        kind: NODE_KIND_CONFIG.into(),
        label: None,
        data: serde_json::json!({"state":"empty", "operation":plan.operation_id(), "plan_digest":plan.exact_sha256()}),
    };
    anyhow::ensure!(
        graph
            .adapter
            .compare_exchange_optional_nodes(&[(None, slot)])?,
        "existing receipt requires reconciliation"
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("fixture clock unavailable")?
        .as_secs();
    graph.extend_native_preapprovals_inner(
        plan.endpoint(),
        plan.owner(),
        plan.expected_endpoint_sha256(),
        plan.rule_expires_at(),
        plan.approval_reference(),
        now,
        Some(ReceiptFixture { plan: &plan }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_storage::SqliteGraphStorage;
    use std::sync::Barrier;

    fn wall_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
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
    #[cfg(unix)]
    fn fixture_plan(
        digest: &str,
        now: u64,
        authorize_until: u64,
        rule_expiry: u64,
        whitespace: bool,
    ) -> crate::maintenance_plan::LoadedMaintenancePlan {
        let op = "01234567-89ab-4cde-8fab-0123456789ab";
        let generation = "b".repeat(64);
        let mut bytes = serde_json::to_vec(&serde_json::json!({"version":1,"action":"extend_native_preapprovals","operation_id":op,"operator":"admin","hotel":"hotel","database_id":"hotel-db","endpoint":"pilot","owner":"owner","expected_endpoint_sha256":digest,"generation_sha256":generation,"approval_reference":"approval","issued_at":now,"authorize_until":authorize_until,"rule_expires_at":rule_expiry})).unwrap();
        if whitespace {
            bytes.push(b' ');
        }
        let hash = hex::encode(Sha256::digest(&bytes));
        let expected = crate::maintenance_plan::PlanExpectation {
            exact_sha256: &hash,
            operator: "admin",
            hotel: "hotel",
            database_id: "hotel-db",
            endpoint: "pilot",
            owner: "owner",
            generation_sha256: &generation,
        };
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = tempfile::Builder::new()
            .prefix("maintenance-plan-domain-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o750)).unwrap();
        let path = dir.path().join("plan.json");
        std::fs::write(&path, &bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        crate::maintenance_plan::read_maintenance_plan_with_owner(
            &path,
            meta.uid(),
            meta.gid(),
            &expected,
        )
        .unwrap()
    }

    #[cfg(unix)]
    fn fixture_command_plan_file(
        digest: &str,
        now: u64,
    ) -> (tempfile::TempDir, std::path::PathBuf, String, u32, u32) {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = tempfile::Builder::new()
            .prefix("maintenance-command-plan-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o750)).unwrap();
        let path = dir.path().join("plan.json");
        let bytes = serde_json::to_vec(&serde_json::json!({
            "version":1,"action":"extend_native_preapprovals",
            "operation_id":"01234567-89ab-4cde-8fab-0123456789ab",
            "operator":"admin","hotel":"hotel","database_id":"hotel-db",
            "endpoint":"pilot","owner":"owner","expected_endpoint_sha256":digest,
            "generation_sha256":"b".repeat(64),"approval_reference":"approval",
            "issued_at":now,"authorize_until":now+100,"rule_expires_at":now+200
        }))
        .unwrap();
        std::fs::write(&path, &bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        (
            dir,
            path,
            hex::encode(Sha256::digest(&bytes)),
            meta.uid(),
            meta.gid(),
        )
    }

    #[cfg(unix)]
    #[test]
    fn fixture_command_rejects_changed_plan_before_open_then_commits_once() {
        use std::os::unix::fs::PermissionsExt;
        let dbdir = tempfile::tempdir().unwrap();
        let db = dbdir.path().canonicalize().unwrap().join("existing.db");
        let storage = SqliteGraphStorage::open(&db).unwrap();
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let original = seed(&graph);
        let digest = endpoint_digest(&original).unwrap();
        drop(graph);
        drop(storage);
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
        let now = wall_now();
        let (_dir, path, hash, uid, gid) = fixture_command_plan_file(&digest, now);
        let generation = "b".repeat(64);
        let expected = crate::maintenance_plan::PlanExpectation {
            exact_sha256: &hash,
            operator: "admin",
            hotel: "hotel",
            database_id: "hotel-db",
            endpoint: "pilot",
            owner: "owner",
            generation_sha256: &generation,
        };
        let mut wrong_hash = hash.clone();
        wrong_hash.replace_range(0..1, if hash.starts_with('a') { "b" } else { "a" });
        let wrong = crate::maintenance_plan::PlanExpectation {
            exact_sha256: &wrong_hash,
            ..expected
        };
        let absent = dbdir.path().join("must-not-be-created.db");
        let err =
            execute_offline_maintenance_fixture(&absent, &path, uid, gid, &wrong).unwrap_err();
        assert!(format!("{err:#}").contains("protected fixture plan refused"));
        assert!(!absent.exists());
        let bytes = std::fs::read(&path).unwrap();
        let mut changed = bytes.clone();
        changed.push(b' ');
        std::fs::write(&path, &changed).unwrap();
        let err = execute_offline_maintenance_fixture(&db, &path, uid, gid, &expected).unwrap_err();
        assert!(format!("{err:#}").contains("protected fixture plan refused"));
        let before = GraphDomain::new(Arc::new(
            SqliteGraphStorage::open_existing_maintenance_fixture(&db)
                .unwrap()
                .adapter(),
        ));
        assert_eq!(
            before
                .get_config_value("__mcp_endpoint__:pilot")
                .unwrap()
                .unwrap(),
            original.to_string()
        );
        let key = GraphDomain::config_key(
            "__native_maintenance_receipt__:01234567-89ab-4cde-8fab-0123456789ab",
        );
        assert!(before.adapter.get_node(&key).unwrap().is_none());
        drop(before);
        std::fs::write(&path, &bytes).unwrap();
        execute_offline_maintenance_fixture(&db, &path, uid, gid, &expected).unwrap();
        let err = execute_offline_maintenance_fixture(&db, &path, uid, gid, &expected).unwrap_err();
        assert!(format!("{err:#}").contains("existing receipt requires reconciliation"));
        let after = GraphDomain::new(Arc::new(
            SqliteGraphStorage::open_existing_maintenance_fixture(&db)
                .unwrap()
                .adapter(),
        ));
        let receipt = after.adapter.get_node(&key).unwrap().unwrap().data;
        assert_eq!(receipt["state"], "committed");
        assert_eq!(receipt["plan_digest"], hash);
        let endpoint: serde_json::Value = serde_json::from_str(
            &after
                .get_config_value("__mcp_endpoint__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            endpoint_digest(&endpoint).unwrap(),
            receipt["new_digest"].as_str().unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn fixture_command_interruption_leaves_receipt_and_cannot_overwrite_later_endpoint_revocation()
    {
        use std::{os::unix::fs::PermissionsExt, time::Duration};
        let dbdir = tempfile::tempdir().unwrap();
        let db = dbdir.path().canonicalize().unwrap().join("existing.db");
        let storage = SqliteGraphStorage::open(&db).unwrap();
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let original = seed(&graph);
        let digest = endpoint_digest(&original).unwrap();
        drop(graph);
        drop(storage);
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
        let now = wall_now();
        let (_dir, path, hash, uid, gid) = fixture_command_plan_file(&digest, now);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (proceed_tx, proceed_rx) = std::sync::mpsc::channel();
        let db_worker = db.clone();
        let path_worker = path.clone();
        let hash_worker = hash.clone();
        let worker = std::thread::spawn(move || {
            crate::sqlite_storage::install_native_expiry_test_hook(ready_tx, proceed_rx);
            let generation = "b".repeat(64);
            let expected = crate::maintenance_plan::PlanExpectation {
                exact_sha256: &hash_worker,
                operator: "admin",
                hotel: "hotel",
                database_id: "hotel-db",
                endpoint: "pilot",
                owner: "owner",
                generation_sha256: &generation,
            };
            execute_offline_maintenance_fixture(&db_worker, &path_worker, uid, gid, &expected)
        });
        ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        drop(proceed_tx); // synthetic interruption before commit
        assert!(worker.join().unwrap().is_err());
        let graph = GraphDomain::new(Arc::new(
            SqliteGraphStorage::open_existing_maintenance_fixture(&db)
                .unwrap()
                .adapter(),
        ));
        let key = GraphDomain::config_key(
            "__native_maintenance_receipt__:01234567-89ab-4cde-8fab-0123456789ab",
        );
        assert_eq!(
            graph.adapter.get_node(&key).unwrap().unwrap().data["state"],
            "empty"
        );
        let mut revoked = original.clone();
        revoked["preapproval_rules"] = serde_json::json!([]);
        save(&graph, &revoked);
        drop(graph);
        let generation = "b".repeat(64);
        let expected = crate::maintenance_plan::PlanExpectation {
            exact_sha256: &hash,
            operator: "admin",
            hotel: "hotel",
            database_id: "hotel-db",
            endpoint: "pilot",
            owner: "owner",
            generation_sha256: &generation,
        };
        let err = execute_offline_maintenance_fixture(&db, &path, uid, gid, &expected).unwrap_err();
        assert!(format!("{err:#}").contains("existing receipt requires reconciliation"));
        let reopened = GraphDomain::new(Arc::new(
            SqliteGraphStorage::open_existing_maintenance_fixture(&db)
                .unwrap()
                .adapter(),
        ));
        let current: serde_json::Value = serde_json::from_str(
            &reopened
                .get_config_value("__mcp_endpoint__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(current, revoked);
        assert_eq!(
            reopened.adapter.get_node(&key).unwrap().unwrap().data["state"],
            "empty"
        );
    }
    #[cfg(unix)]
    #[test]
    fn fixture_receipt_is_bound_to_actual_domain_update_and_recovers_after_reopen() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("existing.db");
        let seeded = SqliteGraphStorage::open(&path).unwrap();
        let graph = GraphDomain::new(Arc::new(seeded.adapter()));
        let original = seed(&graph);
        let digest = endpoint_digest(&original).unwrap();
        drop(graph);
        drop(seeded);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let storage = SqliteGraphStorage::open_existing_maintenance_fixture(&path).unwrap();
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let operation = "01234567-89ab-4cde-8fab-0123456789ab";
        let now = wall_now();
        let plan = fixture_plan(&digest, now, now + 100, now + 200, false);
        let key = GraphDomain::config_key(&format!("__native_maintenance_receipt__:{operation}"));
        let slot = GraphNode {
            node_key: key.clone(),
            kind: NODE_KIND_CONFIG.into(),
            label: None,
            data: serde_json::json!({"state":"empty", "operation":operation, "plan_digest":plan.exact_sha256()}),
        };
        assert!(graph
            .adapter
            .compare_exchange_optional_nodes(&[(None, slot.clone())])
            .unwrap());
        assert!(!graph
            .adapter
            .compare_exchange_optional_nodes(&[(None, slot)])
            .unwrap());
        let wrong_plan = fixture_plan(&digest, now, now + 100, now + 200, true);
        assert!(graph
            .extend_native_preapprovals_inner(
                "pilot",
                "owner",
                &digest,
                now + 200,
                "approval",
                now,
                Some(ReceiptFixture { plan: &wrong_plan })
            )
            .is_err());
        let _ = graph
            .extend_native_preapprovals_inner(
                "pilot",
                "owner",
                &digest,
                now + 200,
                "approval",
                now,
                Some(ReceiptFixture { plan: &plan }),
            )
            .unwrap();
        drop(graph);
        drop(storage);
        // A successful response is deliberately ignored; this is not a COMMIT failure.
        let reopened = SqliteGraphStorage::open_existing_maintenance_fixture(&path).unwrap();
        let graph = GraphDomain::new(Arc::new(reopened.adapter()));
        let receipt = graph.adapter.get_node(&key).unwrap().unwrap().data;
        assert_eq!(receipt["state"], "committed");
        assert_eq!(receipt["operation"], operation);
        assert_eq!(receipt["plan_digest"], plan.exact_sha256());
        assert_eq!(receipt["old_digest"], digest);
        assert_eq!(receipt["endpoint"], "pilot");
        assert_eq!(receipt["owner"], "owner");
        assert_eq!(receipt["expiry"], now + 200);
        assert_eq!(receipt["approval"], "approval");
        assert_eq!(receipt["authorize_until"], now + 100);
        let next = receipt["new_digest"].as_str().unwrap().to_owned();
        assert!(
            next.len() == 64
                && next
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        let current: serde_json::Value = serde_json::from_str(
            &graph
                .get_config_value("__mcp_endpoint__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let index: serde_json::Value = serde_json::from_str(
            &graph
                .get_config_value("__mcp_preapproval__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(endpoint_digest(&current).unwrap(), next);
        assert_eq!(current["preapproval_rules"], index);
        assert!(graph
            .extend_native_preapprovals_inner(
                "pilot",
                "owner",
                &next,
                now + 200,
                "approval",
                now,
                Some(ReceiptFixture { plan: &plan })
            )
            .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn fixture_receipt_write_failure_rolls_back_actual_domain_update() {
        let storage = SqliteGraphStorage::open_in_memory().unwrap();
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let original = seed(&graph);
        let operation = "01234567-89ab-4cde-8fab-0123456789ab";
        let now = wall_now();
        let plan = fixture_plan(
            &endpoint_digest(&original).unwrap(),
            now,
            now + 100,
            now + 200,
            false,
        );
        let key = GraphDomain::config_key(&format!("__native_maintenance_receipt__:{operation}"));
        let slot = GraphNode {
            node_key: key.clone(),
            kind: NODE_KIND_CONFIG.into(),
            label: None,
            data: serde_json::json!({"state":"empty", "operation":operation, "plan_digest":plan.exact_sha256()}),
        };
        assert!(graph
            .adapter
            .compare_exchange_optional_nodes(&[(None, slot.clone())])
            .unwrap());
        storage.raw_conn().lock().unwrap().execute_batch(&format!("CREATE TEMP TRIGGER fail_receipt BEFORE UPDATE ON graph_nodes WHEN NEW.node_key = '{key}' BEGIN SELECT RAISE(ABORT, 'synthetic receipt failure'); END;")).unwrap();
        let err = graph
            .extend_native_preapprovals_inner(
                "pilot",
                "owner",
                &endpoint_digest(&original).unwrap(),
                now + 200,
                "approval",
                now,
                Some(ReceiptFixture { plan: &plan }),
            )
            .unwrap_err();
        assert!(format!("{err:#}").contains("synthetic receipt failure"));
        let endpoint: serde_json::Value = serde_json::from_str(
            &graph
                .get_config_value("__mcp_endpoint__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let index: serde_json::Value = serde_json::from_str(
            &graph
                .get_config_value("__mcp_preapproval__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(endpoint, original);
        assert_eq!(index, original["preapproval_rules"]);
        assert_eq!(graph.adapter.get_node(&key).unwrap(), Some(slot));
    }

    #[cfg(unix)]
    #[test]
    fn fixture_authorization_expires_while_waiting_for_sqlite_lock() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::{mpsc, Mutex, OnceLock};
        use std::time::{Duration, SystemTime, UNIX_EPOCH};
        static BUSY: OnceLock<Mutex<Option<mpsc::Sender<()>>>> = OnceLock::new();
        fn busy(attempt: i32) -> bool {
            if attempt == 0 {
                if let Some(tx) = BUSY.get().unwrap().lock().unwrap().take() {
                    let _ = tx.send(());
                }
            }
            std::thread::sleep(Duration::from_millis(10));
            attempt < 1200
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("existing.db");
        let storage = SqliteGraphStorage::open(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let original = seed(&graph);
        let digest = endpoint_digest(&original).unwrap();
        let now = wall_now();
        let deadline = now + 4;
        let expiry = now + 200;
        let plan = fixture_plan(&digest, now, deadline, expiry, false);
        let key = GraphDomain::config_key(&format!(
            "__native_maintenance_receipt__:{}",
            plan.operation_id()
        ));
        let slot = GraphNode {
            node_key: key.clone(),
            kind: NODE_KIND_CONFIG.into(),
            label: None,
            data: serde_json::json!({"state":"empty", "operation":plan.operation_id(), "plan_digest":plan.exact_sha256()}),
        };
        assert!(graph
            .adapter
            .compare_exchange_optional_nodes(&[(None, slot.clone())])
            .unwrap());
        let (send, ready) = mpsc::channel();
        *BUSY.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(send);
        storage
            .raw_conn()
            .lock()
            .unwrap()
            .busy_handler(Some(busy))
            .unwrap();
        let blocker = rusqlite::Connection::open(&path).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let worker = std::thread::spawn(move || {
            graph.extend_native_preapprovals_inner(
                "pilot",
                "owner",
                &digest,
                expiry,
                "approval",
                now,
                Some(ReceiptFixture { plan: &plan }),
            )
        });
        ready.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(wall_now() < deadline);
        while SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            <= deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        blocker.execute_batch("ROLLBACK").unwrap();
        let err = worker.join().unwrap().unwrap_err();
        assert!(format!("{err:#}").contains("administrator authorization expired"));
        let reopened = SqliteGraphStorage::open_existing_maintenance_fixture(&path).unwrap();
        let graph = GraphDomain::new(Arc::new(reopened.adapter()));
        assert_eq!(graph.adapter.get_node(&key).unwrap(), Some(slot));
        let endpoint: serde_json::Value = serde_json::from_str(
            &graph
                .get_config_value("__mcp_endpoint__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let index: serde_json::Value = serde_json::from_str(
            &graph
                .get_config_value("__mcp_preapproval__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(endpoint, original);
        assert_eq!(index, original["preapproval_rules"]);
    }

    #[cfg(unix)]
    #[test]
    fn fixture_authorization_expires_before_commit_and_rolls_back_receipt() {
        use std::time::Duration;
        let storage = SqliteGraphStorage::open_in_memory().unwrap();
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let original = seed(&graph);
        let digest = endpoint_digest(&original).unwrap();
        let now = wall_now();
        let deadline = now + 4;
        let expiry = now + 200;
        let plan = fixture_plan(&digest, now, deadline, expiry, false);
        let key = GraphDomain::config_key(&format!(
            "__native_maintenance_receipt__:{}",
            plan.operation_id()
        ));
        let slot = GraphNode {
            node_key: key.clone(),
            kind: NODE_KIND_CONFIG.into(),
            label: None,
            data: serde_json::json!({"state":"empty", "operation":plan.operation_id(), "plan_digest":plan.exact_sha256()}),
        };
        assert!(graph
            .adapter
            .compare_exchange_optional_nodes(&[(None, slot.clone())])
            .unwrap());
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (proceed_tx, proceed_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            crate::sqlite_storage::install_native_expiry_test_hook(ready_tx, proceed_rx);
            graph.extend_native_preapprovals_inner(
                "pilot",
                "owner",
                &digest,
                expiry,
                "approval",
                now,
                Some(ReceiptFixture { plan: &plan }),
            )
        });
        ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(wall_now() < deadline);
        while wall_now() <= deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        proceed_tx.send(()).unwrap();
        let err = worker.join().unwrap().unwrap_err();
        assert!(format!("{err:#}").contains("administrator authorization expired"));
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        assert_eq!(graph.adapter.get_node(&key).unwrap(), Some(slot));
        let endpoint: serde_json::Value = serde_json::from_str(
            &graph
                .get_config_value("__mcp_endpoint__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let index: serde_json::Value = serde_json::from_str(
            &graph
                .get_config_value("__mcp_preapproval__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(endpoint, original);
        assert_eq!(index, original["preapproval_rules"]);
    }

    #[test]
    fn native_maintenance_preserves_unknown_fields_and_rejects_retry_drift() {
        let graph = graph();
        let now = wall_now();
        let mut value = seed(&graph);
        value["future_field"] = "preserved".into();
        save(&graph, &value);
        let digest = endpoint_digest(&value).unwrap();
        let (next, _) = graph
            .extend_native_preapprovals("pilot", "owner", &digest, now + 200, "approval", now)
            .unwrap();
        assert_ne!(digest, next);
        assert!(graph
            .extend_native_preapprovals("pilot", "owner", &digest, now + 200, "approval", now)
            .is_err());
        let actual: serde_json::Value = serde_json::from_str(
            &graph
                .get_config_value("__mcp_endpoint__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(actual["future_field"], "preserved");
        assert_eq!(actual["preapproval_rules"][0]["expires_at"], now + 200);
    }
    #[test]
    fn native_maintenance_negative_controls_leave_both_records_unchanged() {
        for case in 0..8 {
            let now = wall_now();
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
                        now + 86401
                    } else {
                        now + 200
                    },
                    "approval",
                    now
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
        let now = wall_now();
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
                        now + 200 + i as u64,
                        "approval",
                        now,
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
    // The actual SQLite lock wait must not allow an expired rule update.
    #[test]
    fn native_maintenance_expiry_during_locked_wait_refuses_without_writes() {
        use std::sync::{mpsc, Mutex, OnceLock};
        use std::time::{Duration, SystemTime, UNIX_EPOCH};
        static BUSY_SIGNAL: OnceLock<Mutex<Option<mpsc::Sender<()>>>> = OnceLock::new();
        fn busy(attempt: i32) -> bool {
            if attempt == 0 {
                if let Some(tx) = BUSY_SIGNAL.get().unwrap().lock().unwrap().take() {
                    let _ = tx.send(());
                }
            }
            std::thread::sleep(Duration::from_millis(10));
            attempt < 1200
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hotel.db");
        let storage = SqliteGraphStorage::open(&path).unwrap();
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let value = seed(&graph);
        let digest = endpoint_digest(&value).unwrap();
        let (started, ready) = mpsc::channel();
        *BUSY_SIGNAL.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(started);
        storage
            .raw_conn()
            .lock()
            .unwrap()
            .busy_handler(Some(busy))
            .unwrap();
        let blocker = rusqlite::Connection::open(&path).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let expiry = now + 4;
        let worker = std::thread::spawn(move || {
            graph.extend_native_preapprovals("pilot", "owner", &digest, expiry, "synthetic", now)
        });
        // A SQLite busy callback proves the CAS attempted its write transaction
        // while the other connection held the lock.
        ready.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                < expiry
        );
        while SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            <= expiry
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        blocker.execute_batch("ROLLBACK").unwrap();
        assert!(worker.join().unwrap().is_err());
        let reopened =
            GraphDomain::new(Arc::new(SqliteGraphStorage::open(&path).unwrap().adapter()));
        let value: serde_json::Value = serde_json::from_str(
            &reopened
                .get_config_value("__mcp_endpoint__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(value["preapproval_rules"][0]["expires_at"], 100);
        let index: serde_json::Value = serde_json::from_str(
            &reopened
                .get_config_value("__mcp_preapproval__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(index, value["preapproval_rules"]);
        assert!(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                > expiry
        );
    }

    #[test]
    fn native_maintenance_expiry_before_commit_rolls_back_both_records() {
        use std::time::{Duration, SystemTime, UNIX_EPOCH};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hotel.db");
        let graph = GraphDomain::new(Arc::new(SqliteGraphStorage::open(&path).unwrap().adapter()));
        let value = seed(&graph);
        let digest = endpoint_digest(&value).unwrap();
        let now = wall_now();
        let expiry = now + 4;
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (proceed_tx, proceed_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            crate::sqlite_storage::install_native_expiry_test_hook(ready_tx, proceed_rx);
            graph.extend_native_preapprovals("pilot", "owner", &digest, expiry, "synthetic", now)
        });
        ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                < expiry
        );
        while SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            <= expiry
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        proceed_tx.send(()).unwrap();
        assert!(worker.join().unwrap().is_err());
        let reopened =
            GraphDomain::new(Arc::new(SqliteGraphStorage::open(&path).unwrap().adapter()));
        let actual: serde_json::Value = serde_json::from_str(
            &reopened
                .get_config_value("__mcp_endpoint__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let index: serde_json::Value = serde_json::from_str(
            &reopened
                .get_config_value("__mcp_preapproval__:pilot")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(actual, value);
        assert_eq!(index, value["preapproval_rules"]);
    }

    #[test]
    fn native_maintenance_replay_validates_without_writes() {
        let storage = SqliteGraphStorage::open_in_memory().unwrap();
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let value = seed(&graph);
        let now = wall_now();
        let expiry = now + 200;
        let digest = endpoint_digest(&value).unwrap();
        let (next, _) = graph
            .extend_native_preapprovals("pilot", "owner", &digest, expiry, "approval", now)
            .unwrap();
        storage.raw_conn().lock().unwrap().execute_batch("CREATE TRIGGER reject_replay BEFORE UPDATE ON graph_nodes BEGIN SELECT RAISE(ABORT, 'replay wrote'); END;").unwrap();
        let (replayed, _) = graph
            .extend_native_preapprovals("pilot", "owner", &next, expiry, "approval", now)
            .unwrap();
        assert_eq!(replayed, next);
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
        let now = wall_now();
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch("CREATE TRIGGER reject_index BEFORE UPDATE ON graph_nodes WHEN NEW.node_key='config:__mcp_preapproval__:pilot' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
        assert!(graph
            .extend_native_preapprovals(
                "pilot",
                "owner",
                &endpoint_digest(&value).unwrap(),
                now + 200,
                "approval",
                now
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
        let now = wall_now();
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
                now + 200,
                "approval",
                now
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
