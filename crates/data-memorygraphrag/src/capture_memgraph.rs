//! Existing Memgraph adapter. Unexported; never initializes schema or connects
//! a runtime route. Caller supplies an already authenticated graph connection.
use super::capture_commit::*;
use super::resolve_plan::*;
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use neo4rs::{Graph, Operation, Query, Row, Txn, query};
use std::collections::BTreeSet;

pub struct MemgraphLifeGraph(pub Graph);
struct MemgraphTransaction(Option<Txn>);

fn storage_field(row: &Row) -> neo4rs::Result<Option<(&'static str, String)>> {
    let key: String = row.get("storage info")?;
    let field = match key.as_str() {
        "storage_mode" => "storage_mode",
        "global_isolation_level" => "global_isolation_level",
        "session_isolation_level" => "session_isolation_level",
        _ => return Ok(None),
    };
    let value: String = row.get("value")?;
    Ok(Some((field, value)))
}

fn verify_capture_storage_info(rows: &[Row]) -> neo4rs::Result<()> {
    let mut storage_mode = None;
    let mut global_isolation_level = None;
    let mut session_isolation_level = None;
    for row in rows {
        let Some((field, value)) = storage_field(row)? else {
            continue;
        };
        let slot = match field {
            "storage_mode" => &mut storage_mode,
            "global_isolation_level" => &mut global_isolation_level,
            "session_isolation_level" => &mut session_isolation_level,
            _ => unreachable!("storage_field only returns allowlisted fields"),
        };
        if slot.replace(value).is_some() {
            return Err(neo4rs::Error::InvalidConfig);
        }
    }
    let storage_mode = storage_mode.ok_or(neo4rs::Error::InvalidConfig)?;
    let global_isolation_level = global_isolation_level.ok_or(neo4rs::Error::InvalidConfig)?;
    let session_isolation_level = session_isolation_level.ok_or(neo4rs::Error::InvalidConfig)?;
    if !matches!(
        storage_mode.as_str(),
        "IN_MEMORY_TRANSACTIONAL" | "ON_DISK_TRANSACTIONAL"
    ) {
        return Err(neo4rs::Error::InvalidConfig);
    }
    if global_isolation_level != "SNAPSHOT_ISOLATION"
        || session_isolation_level != "SNAPSHOT_ISOLATION"
    {
        return Err(neo4rs::Error::InvalidConfig);
    }
    Ok(())
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
            self.0
                .start_txn_with_verified_session_as(
                    Operation::Write,
                    query("SET SESSION TRANSACTION ISOLATION LEVEL SNAPSHOT ISOLATION"),
                    query("SHOW STORAGE INFO"),
                    verify_capture_storage_info,
                )
                .await?,
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
        let ready = self.rows(query("MATCH (s:LifeCaptureSchema {key:$key}) RETURN s.version AS version, s.backfill_complete AS ready")
            .param("key", "resolve-before-create-v1")).await?;
        let catalog_ready = ready.len() == 1
            && ready[0].get::<i64>("version")? == 1
            && ready[0].get::<bool>("ready")?;
        Ok(SchemaState {
            transactional: true,
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
            if let Some(role_id) = &w.role_id {
                let rows = self
                    .rows(
                        query(
                            "MATCH (r {id:$root}), (role:Role {id:$role}) \
                             MERGE (r)-[s:SCOPED_TO]->(role) \
                             ON CREATE SET s.source_policy_manifest=$sources \
                             RETURN role.id AS role",
                        )
                        .param("root", root.as_str())
                        .param("role", role_id.clone())
                        .param("sources", w.sources_json.clone()),
                    )
                    .await?;
                if rows.len() != 1 {
                    bail!("role anchor missing/ambiguous");
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, value: neo4rs::BoltType) -> Row {
        Row::new(
            neo4rs::BoltList {
                value: vec!["storage info".into(), "value".into()],
            },
            neo4rs::BoltList {
                value: vec![key.into(), value],
            },
        )
    }

    fn good_rows() -> Vec<Row> {
        vec![
            row("storage_mode", "IN_MEMORY_TRANSACTIONAL".into()),
            row("global_isolation_level", "SNAPSHOT_ISOLATION".into()),
            row("session_isolation_level", "SNAPSHOT_ISOLATION".into()),
            row("vertex_count", 11_i64.into()),
        ]
    }

    #[test]
    fn capture_storage_metadata_accepts_snapshot_transactional_mode() {
        assert!(verify_capture_storage_info(&good_rows()).is_ok());
    }

    #[test]
    fn capture_storage_metadata_fails_closed_on_missing_duplicate_or_bad_values() {
        assert!(verify_capture_storage_info(&[]).is_err());
        assert!(
            verify_capture_storage_info(&good_rows()[..2]).is_err(),
            "missing session metadata must deny"
        );
        let mut duplicate = good_rows();
        duplicate.push(row("session_isolation_level", "SNAPSHOT_ISOLATION".into()));
        assert!(
            verify_capture_storage_info(&duplicate).is_err(),
            "duplicate allowlisted metadata must deny"
        );
        assert!(
            verify_capture_storage_info(&[
                row("storage_mode", "IN_MEMORY_TRANSACTIONAL".into()),
                row("global_isolation_level", "SNAPSHOT_ISOLATION".into()),
                row("session_isolation_level", "READ_COMMITTED".into()),
            ])
            .is_err(),
            "non-snapshot session metadata must deny"
        );
        assert!(
            verify_capture_storage_info(&[
                row("storage_mode", "IN_MEMORY_ANALYTICAL".into()),
                row("global_isolation_level", "SNAPSHOT_ISOLATION".into()),
                row("session_isolation_level", "SNAPSHOT_ISOLATION".into()),
            ])
            .is_err(),
            "non-transactional storage must deny"
        );
    }

    #[test]
    fn capture_storage_metadata_fails_closed_on_malformed_or_null_rows() {
        let malformed = Row::new(
            neo4rs::BoltList {
                value: vec!["name".into(), "value".into()],
            },
            neo4rs::BoltList {
                value: vec!["storage_mode".into(), "IN_MEMORY_TRANSACTIONAL".into()],
            },
        );
        assert!(verify_capture_storage_info(&[malformed]).is_err());
        assert!(
            verify_capture_storage_info(&[
                row("storage_mode", "IN_MEMORY_TRANSACTIONAL".into()),
                row("global_isolation_level", "SNAPSHOT_ISOLATION".into()),
                row(
                    "session_isolation_level",
                    neo4rs::BoltType::Null(neo4rs::BoltNull)
                ),
            ])
            .is_err()
        );
    }
}
