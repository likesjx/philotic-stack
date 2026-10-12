//! Existing Memgraph adapter. Unexported; never initializes schema or connects
//! a runtime route. Caller supplies an already authenticated graph connection.
use super::capture_commit::*;
use super::resolve_plan::*;
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use neo4rs::{Graph, Query, Row, Txn, query};
use std::collections::BTreeSet;

pub struct MemgraphLifeGraph(pub Graph);
struct MemgraphTransaction(Option<Txn>);
fn info_value(rows: &[Row], field: &str) -> Option<String> {
    for row in rows {
        if let Ok(value) = row.get::<String>(field) {
            return Some(value);
        }
        for column in ["storage info", "name", "key"] {
            if row.get::<String>(column).is_ok_and(|key| key == field)
                && let Ok(value) = row.get::<String>("value")
            {
                return Some(value);
            }
        }
    }
    None
}
impl MemgraphTransaction {
    async fn rows(&mut self, q: Query) -> Result<Vec<Row>> {
        let tx = self
            .0
            .as_mut()
            .ok_or_else(|| anyhow!("closed transaction"))?;
        let mut stream = tx.execute(q).await?;
        let mut rows = Vec::new();
        while let Some(row) = stream.next(&mut *tx).await? {
            rows.push(row);
        }
        Ok(rows)
    }
    async fn run(&mut self, q: Query) -> Result<()> {
        self.rows(q).await?;
        Ok(())
    }
}
#[async_trait]
impl AtomicLifeGraph for MemgraphLifeGraph {
    async fn begin(&self) -> Result<Box<dyn GraphTransaction>> {
        Ok(Box::new(MemgraphTransaction(Some(
            self.0.start_txn().await?,
        ))))
    }
}
#[async_trait]
impl GraphTransaction for MemgraphTransaction {
    async fn schema(&mut self) -> Result<SchemaState> {
        let mut unique = BTreeSet::new();
        for row in self.rows(query("SHOW CONSTRAINT INFO")).await? {
            let kind: String = row.get("constraint type")?;
            if kind.eq_ignore_ascii_case("unique") {
                let label: String = row.get("label")?;
                let props = row
                    .get::<Vec<String>>("properties")
                    .or_else(|_| row.get::<String>("properties").map(|p| vec![p]))?;
                if props.len() == 1 {
                    unique.insert((label, props[0].clone()));
                }
            }
        }
        let info = self
            .rows(query("SHOW STORAGE INFO ON CURRENT DATABASE"))
            .await?;
        let mode = info_value(&info, "storage_mode");
        let isolation = info_value(&info, "storage_isolation_level");
        // Require the session's explicit snapshot setting too. Unknown/null
        // session metadata denies; database defaults cannot rule out overrides.
        let session = self.rows(query("SHOW STORAGE INFO")).await?;
        let session_isolation = info_value(&session, "session_isolation_level");
        let transactional = matches!(
            mode.as_deref(),
            Some("IN_MEMORY_TRANSACTIONAL" | "ON_DISK_TRANSACTIONAL")
        ) && isolation.as_deref() == Some("SNAPSHOT_ISOLATION")
            && session_isolation.as_deref() == Some("SNAPSHOT_ISOLATION");
        let ready = self.rows(query("MATCH (s:LifeCaptureSchema {key:$key}) RETURN s.version AS version, s.backfill_complete AS ready")
            .param("key", "resolve-before-create-v1")).await?;
        let catalog_ready = ready.len() == 1
            && ready[0].get::<i64>("version")? == 1
            && ready[0].get::<bool>("ready")?;
        Ok(SchemaState {
            transactional,
            catalog_ready,
            unique,
        })
    }
    async fn receipt(&mut self, key: &str) -> Result<Option<StoredReceipt>> {
        let rows = self.rows(query("MATCH (r:LifeCaptureReceipt {key:$key}) RETURN r.identity AS identity, r.root_id AS root, r.kind AS kind")
            .param("key", key)).await?;
        if rows.len() > 1 {
            bail!("ambiguous receipt");
        }
        rows.first()
            .map(|r| {
                let id: String = r.get("root")?;
                Ok(StoredReceipt {
                    identity: r.get("identity")?,
                    root: if id.is_empty() {
                        None
                    } else {
                        Some(RootId::parse(&id).map_err(|_| anyhow!("invalid stored root"))?)
                    },
                    kind: r.get("kind")?,
                })
            })
            .transpose()
    }
    async fn resolve(&mut self, anchor: &RootAnchor) -> Result<Lookup> {
        let (rows, absent) = match anchor {
            RootAnchor::ExactId(id) => (self.rows(query("MATCH (r {id:$id}) RETURN r.id AS root").param("id",id.as_str())).await?, Lookup::Absent),
            RootAnchor::VerifiedKey {namespace,value} => (self.rows(query("MATCH (b:LifeRootBinding {key:$key}) RETURN b.root_id AS root").param("key",opaque_key(namespace,value))).await?, Lookup::Absent),
            RootAnchor::ApprovedAlias {namespace,value} => (self.rows(query("MATCH (b:LifeRootAlias {key:$key}) WHERE b.approved=true RETURN b.root_id AS root").param("key",opaque_key(namespace,value))).await?, Lookup::Unverified),
            RootAnchor::Missing => return Ok(Lookup::Unverified),
        };
        let mut roots = BTreeSet::new();
        for row in &rows {
            let id: String = row.get("root")?;
            roots.insert(RootId::parse(&id).map_err(|_| anyhow!("invalid binding root"))?);
        }
        Ok(if rows.is_empty() {
            absent
        } else if rows.len() == 1 {
            Lookup::Unique(roots.into_iter().next().unwrap())
        } else {
            Lookup::Ambiguous(roots)
        })
    }
    async fn roots(&mut self, id: &RootId) -> Result<Vec<RootRecord>> {
        let rows = self
            .rows(query("MATCH (r {id:$id}) RETURN labels(r) AS labels").param("id", id.as_str()))
            .await?;
        let mut records = Vec::new();
        for row in rows {
            let labels: Vec<String> = row.get("labels")?;
            let kinds: Vec<_> = labels
                .into_iter()
                .filter(|l| matches!(l.as_str(), "Goal" | "OpenLoop" | "Event"))
                .collect();
            if kinds.len() != 1 {
                bail!("invalid canonical root labels");
            }
            records.push(RootRecord {
                id: id.clone(),
                label: kinds[0].clone(),
            });
        }
        Ok(records)
    }
    async fn write(&mut self, w: &GraphWrite) -> Result<CommitResult> {
        let (root, kind) = match &w.disposition {
            Disposition::NewRoot {
                proposed_id,
                binding,
                ..
            } => {
                // Labels come exclusively from the closed ontology enum. Never
                // interpolate user text into Cypher.
                if !matches!(w.root_label.as_str(), "Goal" | "OpenLoop" | "Event") {
                    bail!("invalid ontology label");
                }
                self.run(query(&format!("CREATE (r:{} {{id:$id,claim_summary:$summary,validation_state:'proposed',source_policy_manifest:$sources}}) CREATE (b:LifeRootBinding {{key:$key,root_id:$id,root_type:$label}})",w.root_label))
                    .param("id",proposed_id.as_str()).param("summary",w.summary.clone()).param("sources",w.sources_json.clone())
                    .param("key",opaque_key(&binding.namespace,&binding.value)).param("label",w.root_label.clone())).await?;
                (Some(proposed_id.clone()), "new")
            }
            Disposition::Same { root } => (Some(root.clone()), "same"),
            Disposition::Extend { root } => {
                let rows=self.rows(query("MATCH (r {id:$id}) CREATE (x:LifeCaptureExtension {key:$key,details:$details,source_policy_manifest:$sources}) CREATE (r)-[:HAS_EXTENSION]->(x) RETURN x.key AS key")
                    .param("id",root.as_str()).param("key",w.event_key.clone()).param("details",w.details.clone().ok_or_else(||anyhow!("missing details"))?).param("sources",w.sources_json.clone())).await?;
                if rows.len() != 1 {
                    bail!("extension target changed");
                }
                (Some(root.clone()), "extension")
            }
            Disposition::Review { .. } => (None, "review"),
        };
        if let Some(root) = &root {
            for id in &w.evidence_ids {
                let target = if kind == "extension" {
                    "MATCH (r:LifeCaptureExtension {key:$target})"
                } else {
                    "MATCH (r {id:$target})"
                };
                let rows=self.rows(query(&format!("{target} CREATE (e:LifeCaptureEvidence {{key:$key,evidence_id:$evidence,payload_digest:$digest,source_policy_manifest:$sources,actor:$actor}}) CREATE (r)-[:HAS_EVIDENCE]->(e) RETURN e.key AS key"))
                    .param("target",if kind=="extension" {w.event_key.clone()} else {root.as_str().into()})
                    .param("key",opaque_key(&w.event_key,id)).param("evidence",id.clone()).param("digest",w.digest.clone()).param("sources",w.sources_json.clone()).param("actor",w.actor.clone())).await?;
                if rows.len() != 1 {
                    bail!("evidence target changed");
                }
            }
        }
        self.run(query("CREATE (r:LifeCaptureReceipt {key:$key,identity:$identity,root_id:$root,kind:$kind,policy_revision:$revision,actor:$actor,payload_digest:$digest,source_policy_manifest:$sources})")
            .param("key",w.event_key.clone()).param("identity",w.identity.clone()).param("root",root.as_ref().map(|r|r.as_str()).unwrap_or(""))
            .param("kind",kind).param("revision",w.revision.clone()).param("actor",w.actor.clone()).param("digest",w.digest.clone()).param("sources",w.sources_json.clone())).await?;
        Ok(CommitResult {
            root,
            kind: kind.into(),
            replay: false,
        })
    }
    async fn commit(mut self: Box<Self>) -> Result<()> {
        self.0
            .take()
            .ok_or_else(|| anyhow!("closed transaction"))?
            .commit()
            .await?;
        Ok(())
    }
    async fn rollback(mut self: Box<Self>) -> Result<()> {
        self.0
            .take()
            .ok_or_else(|| anyhow!("closed transaction"))?
            .rollback()
            .await?;
        Ok(())
    }
}
