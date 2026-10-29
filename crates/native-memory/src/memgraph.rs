//! Memgraph source-observation adapter. No schema creation or credential discovery.
//! The caller supplies a locally authenticated Bolt connection. All operation
//! data, receipts and nonce records share ONE verified Memgraph transaction.
use super::*;
use neo4rs::{Graph, Operation, Query, Row, Txn, query};
use std::collections::BTreeSet;

pub const LABELS: [&str; 3] = [
    "NativeMemoryObservation",
    "NativeMemoryReceipt",
    "NativeMemoryNonce",
];

pub struct MemgraphRunner {
    graph: Graph,
    destination: String,
    runner: String,
}
fn hex(value: &[u8]) -> String {
    value.iter().map(|b| format!("{b:02x}")).collect()
}
fn key(value: &impl Serialize) -> Result<String> {
    Ok(hex(&digest(value)?))
}
/// Metadata is checked on the same pooled connection that begins the transaction.
fn metadata([storage, constraints]: &[Vec<Row>; 2]) -> neo4rs::Result<()> {
    let mut fields = BTreeMap::new();
    for row in storage {
        let name: String = row.get("storage info")?;
        if matches!(
            name.as_str(),
            "storage_mode" | "global_isolation_level" | "session_isolation_level"
        ) && fields.insert(name, row.get::<String>("value")?).is_some()
        {
            return Err(neo4rs::Error::InvalidConfig);
        }
    }
    if !matches!(
        fields.get("storage_mode").map(String::as_str),
        Some("IN_MEMORY_TRANSACTIONAL" | "ON_DISK_TRANSACTIONAL")
    ) || fields.get("global_isolation_level").map(String::as_str) != Some("SNAPSHOT_ISOLATION")
        || fields.get("session_isolation_level").map(String::as_str) != Some("SNAPSHOT_ISOLATION")
    {
        return Err(neo4rs::Error::InvalidConfig);
    }
    let mut unique = BTreeSet::new();
    for row in constraints {
        let kind: String = row.get("constraint type")?;
        let label: String = row.get("label")?;
        let props = row
            .get::<Vec<String>>("properties")
            .or_else(|_| row.get::<String>("properties").map(|p| vec![p]))?;
        if kind == "unique" && props == ["key"] {
            unique.insert(label);
        }
    }
    if LABELS.iter().any(|label| !unique.contains(*label)) {
        return Err(neo4rs::Error::InvalidConfig);
    }
    Ok(())
}
async fn rows(tx: &mut Txn, q: Query) -> Result<Vec<Row>> {
    let mut stream = tx.execute(q).await.map_err(|_| Error::Unavailable)?;
    let mut result = Vec::new();
    while let Some(row) = stream
        .next(&mut *tx)
        .await
        .map_err(|_| Error::Unavailable)?
    {
        result.push(row);
    }
    Ok(result)
}
async fn body(tx: &mut Txn, label: &str, key: &str) -> Result<Option<String>> {
    // Labels are internal constants only, never caller input.
    let found = rows(
        tx,
        query(&format!(
            "MATCH (n:{label} {{key:$key}}) RETURN n.body AS body"
        ))
        .param("key", key),
    )
    .await?;
    if found.len() > 1 {
        return Err(Error::Unavailable);
    }
    found
        .first()
        .map(|r| r.get("body").map_err(|_| Error::Unavailable))
        .transpose()
}
impl MemgraphRunner {
    pub fn new(graph: Graph, destination: String, runner: String) -> Self {
        Self {
            graph,
            destination,
            runner,
        }
    }
    pub async fn execute_current(
        &self,
        e: &Envelope,
        mut current: impl FnMut() -> Result<(Policy, u64)>,
        cancelled: &AtomicBool,
    ) -> Result<Response> {
        if cancelled.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        let (policy, now) = current()?;
        verify(e, &policy, &self.destination, &self.runner, now)?;
        let mut tx = self
            .graph
            .start_txn_with_verified_metadata(
                Operation::Write,
                query("SET SESSION TRANSACTION ISOLATION LEVEL SNAPSHOT ISOLATION"),
                [query("SHOW STORAGE INFO"), query("SHOW CONSTRAINT INFO")],
                metadata,
            )
            .await
            .map_err(|_| Error::Unavailable)?
            .0;
        let outcome = self.stage(&mut tx, e, now).await;
        let outcome = outcome.and_then(|response| {
            let (policy, now) = current()?;
            verify(e, &policy, &self.destination, &self.runner, now)?;
            if cancelled.load(Ordering::SeqCst) {
                return Err(Error::Cancelled);
            }
            Ok(response)
        });
        match outcome {
            Ok(response) => {
                // A lost COMMIT response is ambiguous; never report committed on error.
                tx.commit().await.map_err(|_| Error::Uncertain)?;
                Ok(response)
            }
            Err(error) => {
                let _ = tx.rollback().await;
                Err(error)
            }
        }
    }
    async fn stage(&self, tx: &mut Txn, e: &Envelope, now: u64) -> Result<Response> {
        let r = &e.request;
        let nonce_key = key(&(e.claims.issuer.as_str(), e.claims.nonce.as_str()))?;
        let nonce_digest = hex(&digest(&e.claims)?);
        let prior_nonce = body(tx, LABELS[2], &nonce_key).await?;
        if prior_nonce.as_ref().is_some_and(|old| old != &nonce_digest) {
            return Err(Error::Replay);
        }
        let event_key = key(&(r.owner.as_str(), r.namespace.as_str(), r.event.as_str()))?;
        let operation_digest = hex(&digest(&(e.claims.caller.as_str(), r))?);
        let prior = body(tx, LABELS[1], &event_key).await?;
        let receipt = if let Some(prior) = prior {
            let (old, receipt): (String, Receipt) =
                serde_json::from_str(&prior).map_err(|_| Error::Unavailable)?;
            if old != operation_digest {
                return Err(Error::Conflict);
            }
            receipt
        } else {
            if let Command::Remember {
                id,
                subject,
                content,
                source,
                source_version,
                lineage,
            } = &r.command
            {
                let observation_key = key(&(r.owner.as_str(), r.namespace.as_str(), id.as_str()))?;
                if body(tx, LABELS[0], &observation_key).await?.is_some() {
                    return Err(Error::Conflict);
                }
                let observation = Observation {
                    id: id.clone(),
                    owner: r.owner.clone(),
                    subject: subject.clone(),
                    authenticated_writer: e.claims.caller.clone(),
                    content: content.clone(),
                    source: source.clone(),
                    source_version: source_version.clone(),
                    lineage: lineage.clone(),
                    kind: "observation".into(),
                };
                rows(tx, query("CREATE (n:NativeMemoryObservation {key:$key, owner:$owner, namespace:$namespace, id:$id, content:$content, body:$body})")
                    .param("key", observation_key).param("owner", r.owner.clone()).param("namespace", r.namespace.clone()).param("id", id.clone()).param("content", content.clone()).param("body", serde_json::to_string(&observation).map_err(|_| Error::Invalid)?)).await?;
            }
            let receipt = Receipt {
                event: r.event.clone(),
                status: "committed".into(),
                operation: r.command.operation().into(),
                committed_at: now,
            };
            rows(
                tx,
                query("CREATE (n:NativeMemoryReceipt {key:$key, body:$body})")
                    .param("key", event_key)
                    .param(
                        "body",
                        serde_json::to_string(&(operation_digest, &receipt))
                            .map_err(|_| Error::Invalid)?,
                    ),
            )
            .await?;
            receipt
        };
        if prior_nonce.is_none() {
            rows(
                tx,
                query("CREATE (n:NativeMemoryNonce {key:$key, body:$body})")
                    .param("key", nonce_key)
                    .param("body", nonce_digest),
            )
            .await?;
        }
        let mut observations = Vec::new();
        if let Command::Recall {
            query: needle,
            limit,
        } = &r.command
        {
            let result = rows(tx, query("MATCH (n:NativeMemoryObservation) WHERE n.owner=$owner AND n.namespace=$namespace AND toLower(n.content) CONTAINS toLower($needle) RETURN n.body AS body ORDER BY n.id LIMIT $limit")
                .param("owner", r.owner.clone()).param("namespace", r.namespace.clone()).param("needle", needle.clone()).param("limit", i64::from(*limit))).await?;
            for row in result {
                let body: String = row.get("body").map_err(|_| Error::Unavailable)?;
                observations.push(serde_json::from_str(&body).map_err(|_| Error::Unavailable)?);
            }
        }
        Ok(Response {
            receipt,
            observations,
        })
    }
}

/// Backend credentials stay in this private local file; never serialized onto
/// the router, included in Debug, or fetched from ambient provider configuration.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemgraphConfig {
    uri: String,
    username: String,
    password: String,
}
impl MemgraphConfig {
    pub fn load(path: &Path) -> Result<Self> {
        load_private_json(path)
    }
    pub async fn connect(self, destination: String, runner: String) -> Result<MemgraphRunner> {
        let graph =
            Graph::new(self.uri, self.username, self.password).map_err(|_| Error::Unavailable)?;
        Ok(MemgraphRunner::new(graph, destination, runner))
    }
}
