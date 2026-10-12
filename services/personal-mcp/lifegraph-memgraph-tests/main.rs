//! Disposable-container acceptance transport only, not a production listener.
//! Fixed loopback endpoint, no credentials, bounded line protocol and queries.
use anyhow::{Result, anyhow, bail};
use neo4rs::{ConfigBuilder, Graph, Query, Txn, query};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const NODES: &str = "MATCH (n) WHERE n.id IN $ids RETURN n.id AS id, labels(n) AS labels, n.claim_summary AS summary, n.source_policy_manifest AS sources LIMIT 2049";
const EDGES: &str = "MATCH (a {id:$from})-[r]->(b {id:$to}) WHERE type(r)=$relation RETURN a.id AS from, b.id AS to, type(r) AS relation, r.source_policy_manifest AS sources LIMIT 2";
const ROOT: &str = "MATCH (b:LifeRootBinding {key:$key}) RETURN b.root_id AS root LIMIT 2";
const ALIAS: &str = "MATCH (b:LifeRootAlias {key:$key}) WHERE b.approved=true RETURN b.root_id AS root LIMIT 2";

async fn read(tx: &mut Txn, q: Query, source: &str) -> Result<Value> {
    let mut stream = tx.execute(q).await?;
    let mut rows = Vec::new();
    while let Some(row) = stream.next(&mut *tx).await? {
        if rows.len() >= 2049 { bail!("row limit"); }
        rows.push(match source {
            NODES => json!({"id":row.get::<String>("id")?, "labels":row.get::<Vec<String>>("labels")?,
                "summary":row.get::<String>("summary")?, "sources":row.get::<String>("sources")?}),
            EDGES => json!({"from":row.get::<String>("from")?, "to":row.get::<String>("to")?,
                "relation":row.get::<String>("relation")?, "sources":row.get::<String>("sources")?}),
            ROOT | ALIAS => json!({"root":row.get::<String>("root")?}),
            _ => bail!("query unavailable"),
        });
    }
    Ok(json!(rows))
}
async fn operation(graph: &Graph, transaction: &mut Option<Txn>, input: &Value) -> Result<Value> {
    match input["op"].as_str().unwrap_or("") {
        "begin" => {
            if transaction.is_some() { bail!("transaction already open"); }
            *transaction = Some(graph.start_txn().await?); Ok(json!(true))
        }
        "commit" => { transaction.take().ok_or_else(|| anyhow!("no transaction"))?.commit().await?; Ok(json!(true)) }
        "rollback" => { if let Some(tx) = transaction.take() { tx.rollback().await?; } Ok(json!(true)) }
        "query" => {
            let source = input["query"].as_str().ok_or_else(|| anyhow!("query"))?;
            if ![NODES, EDGES, ROOT, ALIAS].contains(&source) { bail!("query unavailable"); }
            let mut q = query(source);
            if source == NODES {
                let ids = input["params"]["ids"].as_array().ok_or_else(|| anyhow!("ids"))?;
                if ids.len() > 2048 { bail!("ids limit"); }
                q = q.param("ids", ids.iter().map(|v| v.as_str().map(str::to_owned).ok_or_else(|| anyhow!("id"))).collect::<Result<Vec<_>>>()?);
            } else {
                for key in if source == EDGES { vec!["from", "to", "relation"] } else { vec!["key"] } {
                    q = q.param(key, input["params"][key].as_str().ok_or_else(|| anyhow!("parameter"))?.to_owned());
                }
            }
            read(transaction.as_mut().ok_or_else(|| anyhow!("no transaction"))?, q, source).await
        }
        "change" => {
            let q = match input["change"].as_str().unwrap_or("") {
                "summary" => "MATCH (n:Goal {id:'goal:synthetic-root'}) SET n.claim_summary='changed synthetic goal'",
                "privateSource" => "MATCH (n:Goal {id:'goal:synthetic-root'}) SET n.source_policy_manifest='[\"private:synthetic\"]'",
                "ambiguousAlias" => "CREATE (:LifeRootAlias {key:'synthetic-alias',approved:true,root_id:'goal:synthetic-other'})",
                _ => bail!("fixture change unavailable"),
            };
            graph.run(query(q)).await?; Ok(json!(true))
        }
        _ => bail!("operation unavailable"),
    }
}
#[tokio::main]
async fn main() -> Result<()> {
    // The test runner executes this inside the network namespace of its
    // explicitly named disposable Memgraph container, never on the host.
    let config = ConfigBuilder::default().uri("127.0.0.1:7687").user("synthetic")
        .password("").max_connections(1).build()?;
    let graph = Graph::connect(config)?;
    graph.run(query("SET SESSION TRANSACTION ISOLATION LEVEL SNAPSHOT ISOLATION")).await?;
    let mut info = graph.execute(query("SHOW STORAGE INFO")).await?;
    let mut transactional = false;
    let mut snapshot = false;
    while let Some(row) = info.next().await? {
        if let (Ok(key), Ok(value)) = (row.get::<String>("storage info"), row.get::<String>("value")) {
            if key == "storage_mode" { transactional = ["IN_MEMORY_TRANSACTIONAL", "ON_DISK_TRANSACTIONAL"].contains(&value.as_str()); }
            if key == "session_isolation_level" { snapshot = value == "SNAPSHOT_ISOLATION"; }
        }
    }
    if !transactional || !snapshot { bail!("synthetic isolation unavailable"); }
    drop(info);
    let mut transaction = None;
    let mut input = BufReader::new(tokio::io::stdin());
    let mut output = tokio::io::stdout();
    loop {
        let mut line = Vec::new();
        let count = (&mut input).take(262145).read_until(b'\n', &mut line).await?;
        if count == 0 { break; }
        if line.len() > 262144 || line.last() != Some(&b'\n') { bail!("input limit"); }
        let value: Value = serde_json::from_slice(&line)?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), operation(&graph, &mut transaction, &value)).await;
        let response = match result {
            Ok(Ok(value)) => json!({"result":value}),
            _ => { if let Some(tx) = transaction.take() { let _ = tx.rollback().await; } json!({"error":"synthetic transport unavailable"}) }
        };
        let encoded = serde_json::to_vec(&response)?;
        if encoded.len() > 262144 { bail!("output limit"); }
        output.write_all(&encoded).await?; output.write_all(b"\n").await?; output.flush().await?;
    }
    if let Some(tx) = transaction.take() { tx.rollback().await?; }
    Ok(())
}
