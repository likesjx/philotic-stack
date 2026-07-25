//! Isolation repro driver for the open `life.observe.batch` defect
//! (docs/HANDOFF-2026-07-14-lifegraph-batch.md, LIFE_GRAPH_ACTIVE proposal S1).
//!
//! Drives multi-item `life.observe.batch` AND the same fixtures as single
//! `life.observe` calls directly at the runner over hotel IPC — bypassing
//! philote, the model, the WaitingTool watchdog, and cross-hotel noise — and
//! reports each item's write/reject/error individually. The fixture set
//! deliberately mixes:
//!   - a control Signal (the shape the plain smoke driver proves works)
//!   - flight-itinerary Event/Commitment/OpenLoop shapes reconstructing the
//!     observations that historically never landed (long summaries, unicode
//!     arrows, timezone-rich text, nested metadata)
//!   - one agenda-edge observation (NextAction -ADVANCES-> Goal, S2) whose
//!     edge may report target_missing on a live graph — that is not a write
//!     failure.
//!
//! If the control lands and a flight fixture doesn't, the fault is
//! content-specific (label/validation/property shape), not batch machinery;
//! if singles land and the batch doesn't, it's the batch path. One run
//! settles it.
//!
//! IPC plumbing mirrors examples/life_graph_ipc_smoke_driver.rs.
//!
//! Env:
//!   PHILOTIC_HOTEL_SOCKET  (default /tmp/philotic-aiua.sock)
//!   PHILOTIC_TARGET_NODE / PHILOTIC_NODE_ID / PHILOTIC_REPLY_NODE
//!   LIFE_GRAPH_ISOLATION_SKIP_SINGLES=1  — batch phase only

use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::{Duration, timeout};
use uuid::Uuid;

const DRIVER_GUEST_ID: &str = "life-graph-batch-isolation-driver";
const DRIVER_ROLE: &str = "life-graph.batch.isolation.reply";

struct Fixture {
    name: &'static str,
    label: &'static str,
    claim_summary: String,
    metadata: Value,
    edges: Vec<Value>,
}

fn fixtures() -> Vec<Fixture> {
    vec![
        Fixture {
            name: "control-signal",
            label: "Signal",
            claim_summary: "Batch isolation control: known-good Signal shape".into(),
            metadata: json!({ "isolation": true, "kind": "control" }),
            edges: vec![],
        },
        Fixture {
            name: "flight-event",
            label: "Event",
            claim_summary: "Flight DL0158 ATL→CDG departs 2026-08-02 21:05 EDT, arrives \
                            2026-08-03 11:40 CEST; connection AF0548 CDG→BKO departs 15:55 \
                            CEST, arrives Bamako–Sénou 20:10 GMT. Confirmation #GH7X2Q, \
                            seat 34K (aisle), checked bags: 2."
                .into(),
            metadata: json!({
                "isolation": true,
                "kind": "flight_itinerary",
                "segments": [
                    { "carrier": "DL", "number": "0158", "from": "ATL", "to": "CDG" },
                    { "carrier": "AF", "number": "0548", "from": "CDG", "to": "BKO" }
                ],
                "confirmation": "GH7X2Q"
            }),
            edges: vec![],
        },
        Fixture {
            name: "trip-commitment",
            label: "Commitment",
            claim_summary: "Committed to the Mali trip: on the ground in Bamako Aug 3–17, \
                            2026 — travel docs, vaccinations, and family logistics must be \
                            settled before departure."
                .into(),
            metadata: json!({ "isolation": true, "kind": "trip_commitment" }),
            edges: vec![],
        },
        Fixture {
            name: "trip-openloop",
            label: "OpenLoop",
            claim_summary: "Open: confirm yellow-fever certificate validity and malaria \
                            prophylaxis prescription before the Bamako departure (needs \
                            travel-clinic appointment)."
                .into(),
            metadata: json!({ "isolation": true, "kind": "trip_openloop" }),
            edges: vec![],
        },
        Fixture {
            name: "agenda-edge-nextaction",
            label: "NextAction",
            claim_summary: "Book travel-clinic appointment for Mali pre-departure \
                            requirements."
                .into(),
            metadata: json!({ "isolation": true, "kind": "agenda_edge" }),
            // Deliberately targets a node that may not exist on a live graph:
            // the WRITE must still succeed with the edge reported
            // target_missing. An error here is a real S2 regression.
            edges: vec![json!({
                "rel_type": "ADVANCES",
                "target_id": "life:goal:mali-trip-ready"
            })],
        },
    ]
}

fn observe_input(fixture: &Fixture, run: &str, phase: &str) -> Value {
    let slug = fixture.name.replace('-', "_");
    json!({
        "observation_id": format!("obs-isolation-{phase}-{slug}-{run}"),
        "evidence": {
            "packet_id": format!("pkt-isolation-{phase}-{slug}-{run}"),
            "claim_ref": {
                "id": format!("life:isolation:{phase}:{slug}:{run}"),
                "label": fixture.label
            },
            "claim_summary": fixture.claim_summary,
            "source_refs": [
                {
                    "source_id": DRIVER_GUEST_ID,
                    "source_kind": "runtime_observation",
                    "reliability": { "score": 0.95, "basis": "direct_observation" }
                }
            ],
            "passage_refs": [],
            "confidence": 0.9,
            "validation_state": "proposed",
            "source_reliability": 0.95,
            "conflict_ids": [],
            "adjudication_status": "not_needed",
            "metadata": fixture.metadata
        },
        "proposed_graph_refs": [],
        "edges": fixture.edges
    })
}

#[derive(Debug)]
struct ItemReport {
    name: String,
    phase: &'static str,
    status: String,
    detail: String,
}

impl ItemReport {
    fn landed(&self) -> bool {
        self.status == "proposed"
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let target_node = std::env::var("PHILOTIC_TARGET_NODE")
        .or_else(|_| std::env::var("PHILOTIC_NODE_ID"))
        .unwrap_or_else(|_| "local-aiua-01".to_string());
    let reply_node = std::env::var("PHILOTIC_REPLY_NODE").unwrap_or_else(|_| target_node.clone());
    let run = Uuid::new_v4().simple().to_string();
    println!("target_node={target_node} reply_node={reply_node} run={run}");

    let mut client = SmokeIpc::connect(GuestIdentity {
        guest_id: DRIVER_GUEST_ID.into(),
        role: DRIVER_ROLE.into(),
        supported_tools: Vec::new(),
    })
    .await
    .context("failed to connect batch isolation driver")?;
    subscribe_reply_inbox(&mut client).await?;

    let mut reports: Vec<ItemReport> = Vec::new();

    // ── Phase A: each fixture as a single life.observe ────────────────────
    if std::env::var("LIFE_GRAPH_ISOLATION_SKIP_SINGLES").as_deref() != Ok("1") {
        for fixture in fixtures() {
            let payload = execute_life_tool(
                &mut client,
                &target_node,
                &reply_node,
                "life.observe",
                observe_input(&fixture, &run, "single"),
            )
            .await;
            reports.push(single_report(fixture.name, payload));
            // Report as we go — a wedge mid-run should still show progress.
            let last = reports.last().unwrap();
            println!(
                "single  {:<26} status={:<12} {}",
                last.name, last.status, last.detail
            );
        }
    }

    // ── Phase B: all fixtures in ONE life.observe.batch (fresh ids) ───────
    let batch_fixtures = fixtures();
    let observations: Vec<Value> = batch_fixtures
        .iter()
        .map(|f| observe_input(f, &run, "batch"))
        .collect();
    let batch_payload = execute_life_tool(
        &mut client,
        &target_node,
        &reply_node,
        "life.observe.batch",
        json!({ "observations": observations }),
    )
    .await;

    match batch_payload {
        Ok(payload) => {
            let data = &payload["result"]["data"];
            println!(
                "batch   envelope status={} requested={} succeeded={} failed={}",
                data["status"].as_str().unwrap_or("?"),
                data["requested"],
                data["succeeded"],
                data["failed"],
            );
            let empty = Vec::new();
            let results = data["results"].as_array().unwrap_or(&empty);
            for (idx, fixture) in batch_fixtures.iter().enumerate() {
                let item = results
                    .iter()
                    .find(|r| r["index"].as_u64() == Some(idx as u64))
                    .map(|r| r["result"].clone())
                    .unwrap_or_else(|| json!({ "status": "missing_from_results" }));
                let status = item["status"].as_str().unwrap_or("unknown").to_string();
                let detail = item_detail(&item);
                println!("batch   {:<26} status={:<12} {}", fixture.name, status, detail);
                reports.push(ItemReport {
                    name: fixture.name.to_string(),
                    phase: "batch",
                    status,
                    detail,
                });
            }
        }
        Err(err) => {
            println!("batch   TRANSPORT/ENVELOPE FAILURE: {err:#}");
            for fixture in &batch_fixtures {
                reports.push(ItemReport {
                    name: fixture.name.to_string(),
                    phase: "batch",
                    status: "transport_error".into(),
                    detail: format!("{err:#}"),
                });
            }
        }
    }

    // ── Verdict ───────────────────────────────────────────────────────────
    println!("\n=== isolation verdict (run={run}) ===");
    let mut failures = 0usize;
    for report in &reports {
        let mark = if report.landed() { "PASS" } else { "FAIL" };
        if !report.landed() {
            failures += 1;
        }
        println!(
            "{mark}  {:<7} {:<26} status={:<14} {}",
            report.phase, report.name, report.status, report.detail
        );
    }
    let single_control_ok = reports
        .iter()
        .any(|r| r.phase == "single" && r.name == "control-signal" && r.landed());
    let single_flight_ok = reports
        .iter()
        .any(|r| r.phase == "single" && r.name == "flight-event" && r.landed());
    if single_control_ok && !single_flight_ok {
        println!("=> fault is CONTENT-SPECIFIC (control lands, flight shape doesn't)");
    }
    let batch_any_fail = reports.iter().any(|r| r.phase == "batch" && !r.landed());
    let singles_all_ok = reports
        .iter()
        .filter(|r| r.phase == "single")
        .all(ItemReport::landed);
    if singles_all_ok && batch_any_fail {
        println!("=> fault is in the BATCH PATH (all singles land, batch items don't)");
    }

    if failures > 0 {
        bail!("{failures} fixture write(s) did not land — see verdict above");
    }
    println!("all fixtures landed in both phases — defect not reproduced on this build");
    Ok(())
}

fn single_report(name: &str, payload: Result<Value>) -> ItemReport {
    match payload {
        Ok(payload) => {
            let data = &payload["result"]["data"];
            ItemReport {
                name: name.to_string(),
                phase: "single",
                status: data["status"].as_str().unwrap_or("unknown").to_string(),
                detail: item_detail(data),
            }
        }
        Err(err) => ItemReport {
            name: name.to_string(),
            phase: "single",
            status: "transport_error".into(),
            detail: format!("{err:#}"),
        },
    }
}

/// Compact per-item detail: node id, embed status, edge outcomes, any error.
fn item_detail(item: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(node_id) = item["node_id"].as_str() {
        parts.push(format!("node_id={node_id}"));
    }
    if let Some(embed) = item["embed_status"].as_str() {
        parts.push(format!("embed={embed}"));
    }
    if let Some(edges) = item.get("edges").filter(|e| !e.is_null()) {
        parts.push(format!("edges={edges}"));
    }
    if let Some(error) = item.get("error").filter(|e| !e.is_null()) {
        parts.push(format!("error={error}"));
    }
    parts.join(" ")
}

async fn subscribe_reply_inbox(client: &mut SmokeIpc) -> Result<()> {
    let subscribe = client
        .send_request(IpcRequest::SubscribeInbox {
            role: DRIVER_ROLE.into(),
        })
        .await
        .context("failed to subscribe batch isolation inbox")?;
    match subscribe {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        other => bail!("unexpected subscribe response: {other:?}"),
    }
}

async fn execute_life_tool(
    client: &mut SmokeIpc,
    target_node: &str,
    reply_node: &str,
    tool_name: &str,
    arguments: Value,
) -> Result<Value> {
    let run_id = Uuid::new_v4().simple().to_string();
    let session_id = format!("isolation:life-graph:{tool_name}:{run_id}");
    let turn_id = format!("isolation-turn-{run_id}");

    let response = client
        .send_request(IpcRequest::EmitTask {
            target_node: target_node.to_string(),
            target_role: "life-graph-runner".into(),
            target_guest_id: std::env::var("PHILOTIC_TARGET_GUEST_ID")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            task_json: json!({
                "action": "execute_tool",
                "tool_name": tool_name,
                "arguments": arguments,
                "session_id": session_id,
                "turn_id": turn_id,
                "chat_id": "isolation-chat",
                "agent_id": DRIVER_GUEST_ID,
                "reply_to": reply_node,
                "reply_role": DRIVER_ROLE,
            })
            .to_string(),
        })
        .await
        .with_context(|| format!("{tool_name}: send EmitTask"))?;

    match response {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("{tool_name}: unexpected emit response: {other:?}"),
    }

    let payload = timeout(Duration::from_secs(60), async {
        loop {
            let reply = client.recv_inbound_task().await?;
            let IpcResponse::InboundTask { task_json, .. } = reply else {
                bail!("{tool_name}: unexpected reply envelope: {reply:?}");
            };
            let payload: Value = serde_json::from_str(&task_json)
                .context("failed to decode datasource_response json")?;

            if payload["action"].as_str() != Some("datasource_response") {
                continue;
            }
            let matches_this_run = payload["turn_id"].as_str() == Some(&turn_id)
                || payload["capability"].as_str() == Some(tool_name);
            if !matches_this_run {
                continue;
            }
            if payload.get("error").is_some() && !payload["error"].is_null() {
                if payload["turn_id"].as_str() == Some(&turn_id) {
                    bail!("{tool_name}: datasource returned error: {}", payload["error"]);
                }
                continue;
            }
            if payload["capability"].as_str() == Some(tool_name) {
                if payload["result"]["status"].as_str() != Some("success") {
                    bail!("{tool_name}: expected result.status=success, got {payload}");
                }
                return Ok(payload);
            }
        }
    })
    .await
    .with_context(|| format!("{tool_name}: timed out waiting for datasource_response"))??;

    Ok(payload)
}

struct SmokeIpc {
    stream: UnixStream,
    read_buf: Vec<u8>,
}

impl SmokeIpc {
    async fn connect(identity: GuestIdentity) -> Result<Self> {
        let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
            .unwrap_or_else(|_| "/tmp/philotic-aiua.sock".to_string());
        let stream = UnixStream::connect(&socket_path)
            .await
            .with_context(|| format!("failed to connect hotel IPC socket at {socket_path}"))?;
        let mut client = Self {
            stream,
            read_buf: Vec::new(),
        };
        match client.send_request(IpcRequest::Register(identity)).await? {
            IpcResponse::Standard { ok: true, .. } => Ok(client),
            other => bail!("hotel rejected isolation driver registration: {other:?}"),
        }
    }

    async fn send_request(&mut self, request: IpcRequest) -> Result<IpcResponse> {
        self.write_frame(&request).await?;
        loop {
            let response = self.read_response().await?;
            if matches!(
                response,
                IpcResponse::InboundTask { .. }
                    | IpcResponse::ApartmentUpdate { .. }
                    | IpcResponse::GracefulShutdown { .. }
                    | IpcResponse::MemoryConfig(_)
                    | IpcResponse::MuninnStatus { .. }
                    | IpcResponse::NetworkState { .. }
            ) {
                continue;
            }
            return Ok(response);
        }
    }

    async fn recv_inbound_task(&mut self) -> Result<IpcResponse> {
        loop {
            let response = self.read_response().await?;
            match response {
                IpcResponse::InboundTask { .. } => return Ok(response),
                IpcResponse::Standard { ok: true, .. }
                | IpcResponse::ApartmentUpdate { .. }
                | IpcResponse::GracefulShutdown { .. }
                | IpcResponse::MemoryConfig(_)
                | IpcResponse::MuninnStatus { .. }
                | IpcResponse::NetworkState { .. } => continue,
                other => bail!("unexpected non-task response while waiting for reply: {other:?}"),
            }
        }
    }

    async fn write_frame<T: serde::Serialize>(&mut self, value: &T) -> Result<()> {
        let payload = serde_json::to_vec(value).context("failed to serialize IPC frame")?;
        let len = payload.len() as u32;
        self.stream
            .write_all(&len.to_be_bytes())
            .await
            .context("failed to write IPC frame header")?;
        self.stream
            .write_all(&payload)
            .await
            .context("failed to write IPC frame payload")?;
        Ok(())
    }

    async fn read_response(&mut self) -> Result<IpcResponse> {
        loop {
            if self.read_buf.len() >= 4 {
                let len = u32::from_be_bytes([
                    self.read_buf[0],
                    self.read_buf[1],
                    self.read_buf[2],
                    self.read_buf[3],
                ]) as usize;
                let frame_len = 4 + len;
                if self.read_buf.len() >= frame_len {
                    let payload = self.read_buf[4..frame_len].to_vec();
                    self.read_buf.drain(..frame_len);
                    return serde_json::from_slice(&payload)
                        .context("failed to decode IPC response frame");
                }
            }

            let mut chunk = [0_u8; 8192];
            let n = self
                .stream
                .read(&mut chunk)
                .await
                .context("failed to read IPC frame")?;
            if n == 0 {
                bail!("IPC stream closed while waiting for frame");
            }
            self.read_buf.extend_from_slice(&chunk[..n]);
        }
    }
}
