//! Explicit opt-in acceptance for the unexported capture Memgraph adapter.
//! This must run only against a disposable empty Memgraph on localhost:17687.
#[allow(dead_code)]
#[path = "../src/capture_commit.rs"]
mod capture_commit;
#[allow(dead_code)]
#[path = "../src/capture_memgraph.rs"]
mod capture_memgraph;
#[allow(dead_code)]
#[path = "../src/resolve_plan.rs"]
mod resolve_plan;

use ansible_mesh_core::privacy::{
    AuthenticatedAgent, ProcessingOperation, ProviderBoundary, ResourcePolicy,
    ServerAuthenticatedIdentity, authorize_processing,
};
use ansible_mesh_core::privacy_storage::{PendingCapture, PolicyStore, capture_payload_digest};
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use capture_commit::*;
use capture_memgraph::MemgraphLifeGraph;
use data_memorygraphrag::loop_action::{Action, FIELDS, LoopAction, apply as apply_loop_action};
use neo4rs::{ConfigBuilder, Graph, query};
use resolve_plan::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const URI: &str = "127.0.0.1:17687";
const OPT_IN_ENV: &str = "PHILOTIC_SYNTHETIC_CAPTURE_MEMGRAPH_ACCEPTANCE";

struct Session;
impl ServerAuthenticatedIdentity for Session {
    fn stable_agent_id(&self) -> &str {
        "creator"
    }
    fn roles(&self) -> BTreeSet<String> {
        BTreeSet::new()
    }
}
fn actor() -> AuthenticatedAgent {
    AuthenticatedAgent::from_server(&Session).unwrap()
}

struct Owner;
impl ServerAuthenticatedIdentity for Owner {
    fn stable_agent_id(&self) -> &str {
        "owner"
    }
    fn roles(&self) -> BTreeSet<String> {
        BTreeSet::new()
    }
}
fn owner() -> AuthenticatedAgent {
    AuthenticatedAgent::from_server(&Owner).unwrap()
}

struct Authority {
    store: Arc<PolicyStore>,
    lookup: Lookup,
}

impl PlanningAuthority for Authority {
    fn authorize_capture(&self, c: &Capture) -> std::result::Result<String, Denial> {
        let record = self
            .store
            .load_pending(&actor(), &c.event.producer, &c.event.event_id)
            .map_err(|_| Denial::AccessDenied)?;
        let snapshot = self
            .store
            .snapshot()
            .map_err(|_| Denial::MissingAuthority)?;
        authorize_processing(
            &snapshot,
            Some(&actor()),
            &record.sources,
            ProcessingOperation::SemanticResolution,
            ProviderBoundary::LocalTrusted,
        )
        .map_err(|_| Denial::AccessDenied)?;
        Ok(snapshot.revision().to_string())
    }
    fn resolve(&self, _: &RootAnchor) -> std::result::Result<Lookup, Denial> {
        Ok(self.lookup.clone())
    }
    fn can_read_root(&self, _: &RootId) -> bool {
        true
    }
    fn can_write(&self, _: &WriteTarget) -> bool {
        true
    }
}

#[async_trait]
impl CommitAuthority for Authority {
    async fn acquire(&self, c: &Capture, p: &CommitPayload) -> Result<CommitLease> {
        let lease = self.store.pin_commit()?;
        let stored = self
            .store
            .load_pending(&actor(), &c.event.producer, &c.event.event_id)?;
        let given = p.pending();
        if stored.payload != given.payload
            || stored.sources != given.sources
            || stored.recorded_by != given.recorded_by
            || stored.recorded_by != "creator"
        {
            bail!("unbound capture");
        }
        authorize_processing(
            &lease,
            Some(&actor()),
            &stored.sources,
            ProcessingOperation::SemanticResolution,
            ProviderBoundary::LocalTrusted,
        )
        .map_err(|_| anyhow!("source denied"))?;
        Ok(CommitLease {
            actor: actor(),
            revision: lease.revision().to_string(),
            hold: lease,
        })
    }
    fn validate_lease(&self, l: &CommitLease) -> Result<()> {
        if self.store.snapshot()?.revision().to_string() != l.revision {
            bail!("revision changed");
        }
        Ok(())
    }
}

async fn rows_count(graph: &Graph, cypher: &str) -> Result<i64> {
    let mut rows = graph.execute(query(cypher)).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow!("missing count row"))?;
    Ok(row.get("count")?)
}

async fn connect_disposable_graph() -> Result<Graph> {
    if std::env::var(OPT_IN_ENV).as_deref() != Ok("1") {
        bail!("{OPT_IN_ENV}=1 is required");
    }
    if std::env::var("PHILOTIC_MEMGRAPH_URI").is_ok()
        || std::env::var("PHILOTIC_MEMGRAPH_USER").is_ok()
        || std::env::var("PHILOTIC_MEMGRAPH_PASSWORD").is_ok()
        || std::env::var("PHILOTIC_MEMGRAPH_DB").is_ok()
    {
        bail!("production Memgraph environment variables must be unset");
    }
    let graph = Graph::connect(
        ConfigBuilder::default()
            .uri(URI)
            .user("")
            .password("")
            .max_connections(1)
            .build()?,
    )?;
    graph
        .run(query(
            "SET SESSION TRANSACTION ISOLATION LEVEL SNAPSHOT ISOLATION",
        ))
        .await?;
    if rows_count(&graph, "MATCH (n) RETURN count(n) AS count").await? != 0 {
        bail!("refusing non-empty Memgraph fixture");
    }
    Ok(graph)
}

async fn install_schema(graph: &Graph) -> Result<()> {
    for statement in [
        "CREATE CONSTRAINT ON (n:OpenLoop) ASSERT n.id IS UNIQUE",
        "CREATE CONSTRAINT ON (n:Goal) ASSERT n.id IS UNIQUE",
        "CREATE CONSTRAINT ON (n:Event) ASSERT n.id IS UNIQUE",
        "CREATE CONSTRAINT ON (n:LifeRootBinding) ASSERT n.key IS UNIQUE",
        "CREATE CONSTRAINT ON (n:LifeCaptureReceipt) ASSERT n.key IS UNIQUE",
        "CREATE CONSTRAINT ON (n:LifeCaptureEvidence) ASSERT n.key IS UNIQUE",
        "CREATE CONSTRAINT ON (n:LifeCaptureExtension) ASSERT n.key IS UNIQUE",
        "CREATE (:LifeCaptureSchema {key:'resolve-before-create-v1',version:1,backfill_complete:true})",
        "CREATE (:Role {id:'life:role:chief-of-staff',name:'Chief of Staff',validation_state:'confirmed',source_policy_manifest:'[]'})",
    ] {
        graph.run(query(statement)).await?;
    }
    Ok(())
}

fn setup_policy() -> Result<Arc<PolicyStore>> {
    let dir =
        std::env::temp_dir().join(format!("synthetic-capture-memgraph-{}", ulid::Ulid::new()));
    std::fs::create_dir(&dir)?;
    let store = Arc::new(PolicyStore::open(dir.join("policy.db"))?);
    store.insert_policy(
        &owner(),
        "source",
        &ResourcePolicy::private("owner".into(), "creator".into()),
    )?;
    Ok(store)
}

fn synthetic_capture(
    store: &PolicyStore,
    event: &str,
    value: &str,
    meaning: Meaning,
    role_id: &str,
) -> Result<(Capture, CommitPayload)> {
    let raw = json!({
        "summary": "Synthetic open loop",
        "details": "Synthetic operator detail",
        "source_ids": ["source"],
        "evidence_ids": ["source"],
        "role_id": role_id,
    })
    .to_string();
    store.enqueue(
        &actor(),
        PendingCapture {
            producer: "synthetic",
            event_id: event,
            payload: &raw,
            sources: &["source".into()],
            expected_revision: store.snapshot()?.revision(),
        },
    )?;
    let c = Capture {
        event: EventKey {
            producer: "synthetic".into(),
            event_id: event.into(),
        },
        evidence_ids: BTreeSet::from(["source".into()]),
        payload_digest: capture_payload_digest(&raw),
        anchor: RootAnchor::VerifiedKey {
            namespace: "synthetic".into(),
            value: value.into(),
        },
        root_type: RootType::OpenLoop,
        meaning,
        semantic_candidates: BTreeSet::new(),
    };
    let p = CommitPayload::from_pending(store.load_pending(&actor(), "synthetic", event)?)?;
    Ok((c, p))
}

async fn snapshot(graph: &Graph, id: &str) -> Result<BTreeMap<String, Value>> {
    let columns = FIELDS
        .iter()
        .map(|k| format!("n.{k} AS {k}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut rows = graph
        .execute(query(&format!("MATCH (n {{id:$id}}) RETURN {columns}")).param("id", id))
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow!("missing snapshot"))?;
    Ok(FIELDS
        .iter()
        .map(|k| {
            let value = if *k == "loop_action_revision" {
                row.get::<i64>(k).map(|n| json!(n)).unwrap_or(Value::Null)
            } else {
                row.get::<String>(k)
                    .map(Value::String)
                    .unwrap_or(Value::Null)
            };
            ((*k).into(), value)
        })
        .collect())
}

fn close_command(id: &str, before: BTreeMap<String, Value>, request_id: &str) -> LoopAction {
    LoopAction {
        id: id.into(),
        actor: "edge:synthetic-operator".into(),
        request_id: request_id.into(),
        action: Action::Close,
        before,
        note: "synthetic operator marked done".into(),
    }
}

async fn cleanup(graph: &Graph) {
    let _ = graph.run(query("MATCH (n) DETACH DELETE n")).await;
}

#[tokio::test]
#[ignore = "requires empty disposable Memgraph on 127.0.0.1:17687 and PHILOTIC_SYNTHETIC_CAPTURE_MEMGRAPH_ACCEPTANCE=1"]
async fn capture_memgraph_then_explicit_operator_close_acceptance() -> Result<()> {
    let graph = connect_disposable_graph().await?;
    let result = async {
        install_schema(&graph).await?;
        let store = setup_policy()?;
        let adapter = MemgraphLifeGraph(graph.clone());
        let role = "life:role:chief-of-staff";
        let (capture, payload) =
            synthetic_capture(&store, "open", "claim", Meaning::Distinct, role)?;
        let authority = Authority {
            store: store.clone(),
            lookup: Lookup::Absent,
        };
        let planned = plan(&capture, Some(&authority))
            .map_err(|e| anyhow!("planning denied: {e:?}"))?;
        let created = commit_capture(&adapter, Some(&authority), &capture, &planned, &payload)
            .await?;
        let root = created.root.clone().ok_or_else(|| anyhow!("missing root"))?;
        assert_eq!(created.kind, "new");
        assert!(!created.replay);

        let replay_authority = Authority {
            store: store.clone(),
            lookup: Lookup::Unique(root.clone()),
        };
        let replay = commit_capture(&adapter, Some(&replay_authority), &capture, &planned, &payload)
            .await?;
        assert!(replay.replay);
        assert_eq!(replay.root, Some(root.clone()));

        let mut rows = graph.execute(query("MATCH (:OpenLoop {id:$root})-[:SCOPED_TO]->(:Role {id:$role}) RETURN count(*) AS count")
            .param("root", root.as_str()).param("role", role)).await?;
        assert_eq!(rows.next().await?.unwrap().get::<i64>("count")?, 1);

        let before_close = snapshot(&graph, root.as_str()).await?;
        let close = close_command(root.as_str(), before_close.clone(), &"c".repeat(32));
        let receipt = apply_loop_action(&graph, &close, "2026-10-13T00:00:00Z").await?;
        assert_eq!(receipt["status"], "saved");
        let stale = close_command(root.as_str(), before_close, &"d".repeat(32));
        assert_eq!(
            apply_loop_action(&graph, &stale, "2026-10-13T00:01:00Z").await?["status"],
            "conflict"
        );
        let closed = snapshot(&graph, root.as_str()).await?;
        assert_eq!(closed["status"], json!("resolved"));

        let before_missing = rows_count(&graph, "MATCH (n) RETURN count(n) AS count").await?;
        let (missing, missing_payload) = synthetic_capture(
            &store,
            "missing-role",
            "missing-role-claim",
            Meaning::Distinct,
            "life:role:not-seeded",
        )?;
        let missing_authority = Authority {
            store,
            lookup: Lookup::Absent,
        };
        let missing_plan = plan(&missing, Some(&missing_authority))
            .map_err(|e| anyhow!("planning denied: {e:?}"))?;
        assert!(
            commit_capture(
                &adapter,
                Some(&missing_authority),
                &missing,
                &missing_plan,
                &missing_payload
            )
            .await
            .is_err()
        );
        assert_eq!(
            rows_count(&graph, "MATCH (n) RETURN count(n) AS count").await?,
            before_missing
        );
        Ok(())
    }
    .await;
    cleanup(&graph).await;
    result
}
