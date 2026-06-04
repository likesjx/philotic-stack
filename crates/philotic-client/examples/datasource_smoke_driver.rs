use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::{Value, json};
use tokio::time::{Duration, timeout};

const DRIVER_GUEST_ID: &str = "datasource-smoke-driver";
const DRIVER_ROLE: &str = "datasource.smoke.reply";

#[tokio::main]
async fn main() -> Result<()> {
    let target_node =
        std::env::var("PHILOTIC_TARGET_NODE").unwrap_or_else(|_| "local-aiua-01".to_string());
    let reply_node = std::env::var("PHILOTIC_REPLY_NODE").unwrap_or_else(|_| target_node.clone());
    let graph_id = format!("datasource-smoke-{}", uuid::Uuid::new_v4());
    let smoke_goal_id = format!("smoke_memgraph_goal_{}", uuid::Uuid::new_v4().simple());

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: DRIVER_GUEST_ID.into(),
        role: DRIVER_ROLE.into(),
        supported_tools: Vec::new(),
    })
    .await
    .context("failed to connect datasource smoke driver")?;

    let subscribe = client
        .send_request(IpcRequest::SubscribeInbox {
            role: DRIVER_ROLE.into(),
        })
        .await
        .context("failed to subscribe datasource smoke driver inbox")?;
    match subscribe {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("unexpected subscribe response: {other:?}"),
    }

    let payload = execute_tool(
        &mut client,
        &target_node,
        &reply_node,
        "graph.create",
        json!({
            "graph_id": graph_id,
        }),
    )
    .await?;

    let returned_graph_id = payload["result"]["graph_id"]
        .as_str()
        .context("datasource_response missing result.graph_id")?;
    if returned_graph_id != graph_id {
        bail!("expected graph_id {graph_id}, got {returned_graph_id}");
    }

    println!("datasource graph.create ok  graph_id={returned_graph_id}");

    let list_payload = execute_tool(
        &mut client,
        &target_node,
        &reply_node,
        "graph.list",
        json!({
            "graph_id": graph_id,
        }),
    )
    .await?;
    assert_graph_list_present(&list_payload)?;
    println!("datasource graph.list ok");

    let merge_payload = execute_tool(
        &mut client,
        &target_node,
        &reply_node,
        "graph.query",
        json!({
            "graph_id": graph_id,
            "query": format!(
                "MERGE (r:Role {{id: 'human'}}) \
                 SET r.name = 'Human' \
                 MERGE (g:Goal {{id: '{smoke_goal_id}'}}) \
                 SET g.name = 'Datasource Memgraph Smoke' \
                 MERGE (r)-[rel:HAS_GOAL {{id: 'human-{smoke_goal_id}'}}]->(g) \
                 RETURN r, rel, g"
            ),
        }),
    )
    .await?;
    assert_rows_present(&merge_payload, "graph.query merge")?;

    let read_payload = execute_tool(
        &mut client,
        &target_node,
        &reply_node,
        "graph.query",
        json!({
            "graph_id": graph_id,
            "query": format!(
                "MATCH (r:Role {{id: 'human'}})-[rel:HAS_GOAL]->(g:Goal {{id: '{smoke_goal_id}'}}) \
                 RETURN r, rel, g"
            ),
        }),
    )
    .await?;
    assert_rows_present(&read_payload, "graph.query read")?;

    println!("datasource graph.query ok   goal_id={smoke_goal_id}");

    let schema_payload = execute_tool(
        &mut client,
        &target_node,
        &reply_node,
        "graph.schema",
        json!({
            "graph_id": graph_id,
        }),
    )
    .await?;
    assert_schema_contains(&schema_payload, "Role", "HAS_GOAL")?;
    assert_schema_contains(&schema_payload, "Goal", "HAS_GOAL")?;
    println!("datasource graph.schema ok");
    Ok(())
}

async fn execute_tool(
    client: &mut PhiloticClient,
    target_node: &str,
    reply_node: &str,
    tool_name: &str,
    arguments: Value,
) -> Result<Value> {
    let response = client
        .send_request(IpcRequest::EmitTask {
            target_node: target_node.to_string(),
            target_role: "graph-datasource".into(),
            target_guest_id: None,
            task_json: json!({
                "action": "execute_tool",
                "tool_name": tool_name,
                "arguments": arguments,
                "session_id": format!("smoke:datasource:{tool_name}"),
                "turn_id": format!("smoke-turn-datasource-{}", tool_name.replace('.', "-")),
                "chat_id": "smoke-chat",
                "agent_id": DRIVER_GUEST_ID,
                "reply_to": reply_node,
                "reply_role": DRIVER_ROLE,
            })
            .to_string(),
        })
        .await?;

    match response {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("{tool_name}: unexpected emit response: {other:?}"),
    }

    let reply = timeout(Duration::from_secs(15), async {
        loop {
            match client.recv_task().await? {
                IpcResponse::Standard { ok: true, .. } => continue,
                response => return Ok::<IpcResponse, anyhow::Error>(response),
            }
        }
    })
    .await
    .with_context(|| format!("{tool_name}: timed out waiting for datasource_response"))??;
    let IpcResponse::InboundTask { task_json, .. } = reply else {
        bail!("{tool_name}: unexpected reply envelope: {reply:?}");
    };
    let payload: Value =
        serde_json::from_str(&task_json).context("failed to decode datasource_response json")?;

    if payload["action"].as_str() != Some("datasource_response") {
        bail!("expected datasource_response, got {payload}");
    }
    if payload.get("error").is_some() {
        bail!("datasource returned error: {}", payload["error"]);
    }
    if payload["capability"].as_str() != Some(tool_name) {
        bail!("expected {tool_name} capability, got {payload}");
    }

    Ok(payload)
}

fn assert_rows_present(payload: &Value, label: &str) -> Result<()> {
    let rows = payload["result"]["data"]["rows"]
        .as_array()
        .with_context(|| format!("{label}: result missing data.rows array"))?;
    if rows.is_empty() {
        bail!("{label}: expected at least one returned row, got {payload}");
    }

    Ok(())
}

fn assert_graph_list_present(payload: &Value) -> Result<()> {
    let graphs = payload["result"]["data"]
        .as_array()
        .context("graph.list: result missing data array")?;
    if graphs.is_empty() {
        bail!("graph.list: expected at least one graph entry, got {payload}");
    }

    Ok(())
}

fn assert_schema_contains(payload: &Value, label: &str, relationship_type: &str) -> Result<()> {
    let data = &payload["result"]["data"];
    let labels = data["labels"]
        .as_array()
        .context("graph.schema: result missing data.labels array")?;
    if !labels.iter().any(|value| value.as_str() == Some(label)) {
        bail!("graph.schema: expected label {label}, got {payload}");
    }

    let relationship_types = data["relationship_types"]
        .as_array()
        .context("graph.schema: result missing data.relationship_types array")?;
    if !relationship_types
        .iter()
        .any(|value| value.as_str() == Some(relationship_type))
    {
        bail!("graph.schema: expected relationship type {relationship_type}, got {payload}");
    }

    Ok(())
}
