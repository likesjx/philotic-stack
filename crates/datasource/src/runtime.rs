use crate::controller::{
    CONTRACT_ERROR_MARKER, DatasourceProvider, DatasourceTask, ProviderOutput, ProviderRegistry,
};
use anyhow::Result;
use philotic_client::{
    GuestIdentity, IpcRequest, IpcResponse, PhiloticClient, ReturnRoute, TaskErrorPayload,
    is_ipc_disconnect,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

/// Backstop deadline for a single `DatasourceProvider::invoke`.
///
/// THREE DEADLINES GOVERN ONE LIFEGRAPH CALL, AND THEY MUST STAY ORDERED:
///
/// ```text
///   Memgraph per-query   15s  (data-memorygraphrag: MEMGRAPH_QUERY_TIMEOUT_SECS)
///     <  batch budget    60s  (data-memorygraphrag: OBSERVE_BATCH_BUDGET_SECS)
///       <  this          75s
///         <  caller      90s  (philote/turn_loop.rs: WAITING_TOOL_SECS)
/// ```
///
/// Each must be strictly larger than the one it contains, or the containing
/// deadline fires first and the inner bound never gets to do its job. In
/// particular, dropping BELOW the batch budget would abandon healthy batches
/// mid-write: a cancelled future does not un-write what Memgraph already
/// committed, so every such kill manufactures an orphaned-write divergence
/// (which philote then has to reconcile via `orphaned_tool_write`). Staying
/// under the caller's watchdog is what lets the runner report a real failure
/// instead of the caller inferring one from silence.
///
/// The four constants live in three crates. Changing any one of them without
/// checking the others breaks the chain silently — there is no compile-time
/// link between them.
const PROVIDER_INVOKE_TIMEOUT_SECS: u64 = 75;

/// How many read-only datasource tasks may run concurrently.
///
/// Sized to Memgraph's `bolt_num_workers=2` on the vps. Raising it past the
/// server's worker count does not buy throughput — it re-creates the Bolt
/// saturation that PRs #275/#277 fixed, where connection churn stalled batches
/// until the caller's watchdog evicted them. If that setting changes, this
/// should change with it.
const CONCURRENT_READ_SLOTS: usize = 2;

/// Capabilities that only read, and may therefore run off the critical path.
///
/// Conservative allowlist rather than a "not a write" denylist: a capability
/// nobody has classified stays on the sequential path, so the failure mode of
/// forgetting to update this is lost concurrency, never a lost write ordering
/// guarantee. Keeping writes sequential is what preserves read-after-write
/// consistency without per-session locking.
pub fn is_read_only_capability(kind: &str) -> bool {
    matches!(
        kind,
        "life.recall"
            | "life.recall.stats"
            | "life.view.node"
            | "life.view.neighborhood"
            | "life.patch.list"
            | "life.list"
            | "life.ontology"
    )
    // life.patch.apply is a WRITE (vocabulary mutation) — stays sequential.
}

/// How often a still-running provider tells its caller it is alive.
///
/// Must divide the caller's phase watchdog several times over, so a single
/// dropped or delayed ping cannot starve the caller into evicting a healthy
/// turn. At 20s against philote's 90s `WAITING_TOOL_SECS` a turn survives two
/// consecutive lost pings.
const TOOL_PROGRESS_PING_SECS: u64 = 20;

pub type ProviderFactory = dyn Fn() -> Vec<Arc<dyn DatasourceProvider>> + Send + Sync;

pub struct DatasourceGuestConfig {
    pub guest_id: &'static str,
    pub role: &'static str,
    pub providers: Box<ProviderFactory>,
}

#[derive(Debug, Clone)]
struct ReplyRoute {
    return_route: ReturnRoute,
    chat_id: String,
}

impl ReplyRoute {
    fn from_task(task: &Value) -> Self {
        let local_node_id =
            std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string());

        let return_route = ReturnRoute::from_task(task, local_node_id, "agent");

        Self {
            return_route,
            chat_id: task
                .get("chat_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }
    }
}

pub async fn run_datasource_controller(config: DatasourceGuestConfig) -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();

    // State provenance up front. A deployed guest running code nobody merged is
    // otherwise invisible — see `philotic_client::build_sha`.
    info!(
        guest_id = config.guest_id,
        role = config.role,
        build_sha = philotic_client::build_sha(),
        "starting datasource guest controller"
    );

    let identity = GuestIdentity {
        guest_id: config.guest_id.into(),
        role: config.role.into(),
        supported_tools: Vec::new(),
    };

    let mut ipc_client = PhiloticClient::connect(identity).await?;
    ipc_client
        .send_request(IpcRequest::SubscribeInbox {
            role: config.role.into(),
        })
        .await?;

    // Separate connection for OUTBOUND frames.
    //
    // `PhiloticClient` wraps ONE UnixStream and the reply protocol has no
    // per-message correlation id — replies are matched by FIFO order (see the
    // DEF-045 note on `PhiloticClient`). Emitting concurrently over the receive
    // connection would therefore interleave frames and mis-attribute replies,
    // which is why a read task cannot simply borrow the main client.
    //
    // A second connection under its own mutex gives every emitter a serialized,
    // correctly-ordered pipe while leaving the receive loop free to keep
    // accepting work. It registers under a DISTINCT guest id on purpose: the
    // hotel drains a guest's parked-inbound queue on Register, so re-registering
    // the runner's own id here would steal tasks from the receive connection.
    // It never subscribes to an inbox and so is never delivered work.
    let emitter: Arc<tokio::sync::Mutex<PhiloticClient>> = Arc::new(tokio::sync::Mutex::new(
        PhiloticClient::connect(GuestIdentity {
            guest_id: format!("{}:emitter", config.guest_id),
            role: config.role.into(),
            supported_tools: Vec::new(),
        })
        .await?,
    ));

    // Read-only `life.*` work runs off the critical path so a long write (a
    // 25-item observe batch can legitimately take tens of seconds) no longer
    // head-of-line blocks every other agent's sub-second recall on this runner.
    //
    // Bounded, not free: Memgraph on the vps runs `bolt_num_workers=2`, and
    // unbounded concurrency here would recreate by a different route the very
    // saturation that PRs #275/#277 fixed — the connection-pool churn that
    // stalled batches until the watchdog evicted them.
    //
    // WRITES DELIBERATELY STAY INLINE AND SEQUENTIAL. That preserves
    // read-after-write ordering by construction, with no per-session locking and
    // no dependence on every caller supplying a session_id.
    let read_slots = Arc::new(tokio::sync::Semaphore::new(CONCURRENT_READ_SLOTS));

    info!(role = config.role, "listening for datasource tasks");

    loop {
        match tokio::time::timeout(Duration::from_secs(5), ipc_client.recv_task()).await {
            Ok(Ok(IpcResponse::InboundTask {
                source_node,
                task_id,
                task_json,
            })) => {
                info!(
                    guest_id = config.guest_id,
                    task_id = %task_id,
                    source_node,
                    "received datasource task"
                );

                let task_value = match serde_json::from_str::<Value>(&task_json) {
                    Ok(task) => task,
                    Err(err) => {
                        warn!("failed to parse inbound datasource task JSON: {err}");
                        continue;
                    }
                };

                let reply = ReplyRoute::from_task(&task_value);
                let controller_task = match DatasourceTask::from_value(&task_value) {
                    Ok(task) => task,
                    Err(err) => {
                        emit_failure(
                            &mut ipc_client,
                            &reply,
                            None,
                            None,
                            format!("uninterpretable datasource task: {err}"),
                        )
                        .await?;
                        continue;
                    }
                };

                let providers = ProviderRegistry::new((config.providers)());
                let provider = match providers.resolve(&controller_task) {
                    Ok(provider) => provider,
                    Err(err) => {
                        emit_failure(
                            &mut ipc_client,
                            &reply,
                            Some(controller_task.kind.as_str()),
                            None,
                            format!("no datasource provider available: {err}"),
                        )
                        .await?;
                        continue;
                    }
                };

                info!(
                    capability = controller_task.kind.as_str(),
                    provider = provider.id(),
                    "dispatching datasource task"
                );

                // Read-only work leaves the critical path; writes stay inline.
                if is_read_only_capability(controller_task.kind.as_str()) {
                    let emitter = Arc::clone(&emitter);
                    let slots = Arc::clone(&read_slots);
                    let provider = Arc::clone(&provider);
                    tokio::spawn(async move {
                        // Acquiring the permit inside the task means the receive
                        // loop never blocks on a full slot set — excess reads
                        // queue here instead of stalling the inbox.
                        let _permit = match slots.acquire_owned().await {
                            Ok(permit) => permit,
                            Err(_) => return, // semaphore closed: shutting down
                        };
                        let outcome = tokio::time::timeout(
                            Duration::from_secs(PROVIDER_INVOKE_TIMEOUT_SECS),
                            provider.invoke(&controller_task),
                        )
                        .await
                        .unwrap_or_else(|_| {
                            Err(anyhow::anyhow!(
                                "read provider exceeded the {PROVIDER_INVOKE_TIMEOUT_SECS}s \
                                 runtime deadline and was abandoned"
                            ))
                        });
                        let mut client = emitter.lock().await;
                        match outcome {
                            Ok(output) => {
                                if let Err(err) = emit_success_response(
                                    &mut client,
                                    &reply,
                                    &controller_task,
                                    provider.id(),
                                    output,
                                )
                                .await
                                {
                                    warn!("failed to emit read response: {err}");
                                }
                            }
                            Err(err) => {
                                let chained = format!("{err:#}");
                                error!("datasource read invocation failed: {chained}");
                                let sub_kind = if chained.contains(CONTRACT_ERROR_MARKER) {
                                    Some("invalid_request")
                                } else {
                                    None
                                };
                                if let Err(err) = emit_failure_with_sub_kind(
                                    &mut client,
                                    &reply,
                                    Some(controller_task.kind.as_str()),
                                    Some(provider.id()),
                                    format!("provider failed: {chained}"),
                                    sub_kind,
                                )
                                .await
                                {
                                    warn!("failed to emit read failure: {err}");
                                }
                            }
                        }
                    });
                    continue;
                }

                // Run the provider under a backstop deadline WHILE emitting
                // periodic progress pings.
                //
                // The deadline: this loop awaits `invoke` INLINE and is the only
                // consumer of the inbox, so an unbounded provider call does not
                // just strand its own caller — it head-of-line blocks every
                // other agent's datasource traffic on this runner for as long as
                // it hangs. Bounding it means a wedged provider costs one task,
                // not the queue. Ordering matters and the constants live in
                // three crates — see PROVIDER_INVOKE_TIMEOUT_SECS.
                //
                // The pings: a caller's phase watchdog otherwise cannot tell a
                // slow-but-healthy tool from a dead one, so the only previous
                // way to survive being slow was a bespoke per-tool constant
                // (delegate.whisper's 660s). A ping says "still working" and
                // refreshes the caller's PHASE timer only — philote keeps the
                // total-active ceiling in a separate clock that pings cannot
                // touch, so a genuinely wedged tool is still evicted on time.
                //
                // Deliberately NOT spawned: select! keeps the one-task-at-a-time
                // ordering of this loop intact, and needs no change to the
                // DatasourceProvider trait.
                let invoke_result = {
                    let invoke_fut = provider.invoke(&controller_task);
                    tokio::pin!(invoke_fut);
                    let deadline =
                        tokio::time::sleep(Duration::from_secs(PROVIDER_INVOKE_TIMEOUT_SECS));
                    tokio::pin!(deadline);
                    let mut ticker =
                        tokio::time::interval(Duration::from_secs(TOOL_PROGRESS_PING_SECS));
                    ticker.tick().await; // interval fires immediately; drop that one
                    loop {
                        tokio::select! {
                            output = &mut invoke_fut => break output,
                            _ = &mut deadline => break Err(anyhow::anyhow!(
                                "provider exceeded the {PROVIDER_INVOKE_TIMEOUT_SECS}s runtime \
                                 deadline and was abandoned; any writes it completed before this \
                                 point are durable and were NOT rolled back"
                            )),
                            _ = ticker.tick() => {
                                emit_tool_progress(
                                    &mut ipc_client,
                                    &reply,
                                    &controller_task,
                                    provider.id(),
                                )
                                .await;
                            }
                        }
                    }
                };
                match invoke_result {
                    Ok(output) => {
                        let change = match &output {
                            ProviderOutput::ResultSet(data) => {
                                data.get("change_notification").cloned()
                            }
                            _ => None,
                        };
                        emit_success_response(
                            &mut ipc_client,
                            &reply,
                            &controller_task,
                            provider.id(),
                            output,
                        )
                        .await?;
                        if let Some(change) = change {
                            forward_change_notification(
                                &mut ipc_client,
                                &controller_task,
                                provider.id(),
                                change,
                            )
                            .await;
                        }
                    }
                    Err(err) => {
                        // Use the alternate/chain Display ({err:#}) so anyhow's
                        // .context() cause chain (e.g. the serde field/type mismatch
                        // that produced a parse failure) reaches the log and the
                        // caller instead of being swallowed by the top-level message.
                        let chained = format!("{err:#}");
                        error!("datasource provider invocation failed: {chained}");
                        // Provider handlers mark pre-write, model-fixable
                        // contract/parameter failures with CONTRACT_ERROR_MARKER
                        // (see data-memorygraphrag's handle_observe). Tag those as
                        // "invalid_request" so philote can tell a contract failure
                        // (worth one bounded model retry) apart from an infra/
                        // transport failure, without guessing from free-text.
                        let sub_kind = if chained.contains(CONTRACT_ERROR_MARKER) {
                            Some("invalid_request")
                        } else {
                            None
                        };
                        emit_failure_with_sub_kind(
                            &mut ipc_client,
                            &reply,
                            Some(controller_task.kind.as_str()),
                            Some(provider.id()),
                            format!("provider failed: {chained}"),
                            sub_kind,
                        )
                        .await?;
                    }
                }
            }
            Ok(Ok(other)) => {
                info!(
                    ?other,
                    "received non-task IPC while running datasource guest"
                );
            }
            Ok(Err(err)) => {
                if is_ipc_disconnect(&err) {
                    info!(
                        guest_id = config.guest_id,
                        "hotel IPC disconnected; datasource guest exiting"
                    );
                    return Ok(());
                }
                warn!("datasource guest IPC receive error: {err}");
            }
            Err(_) => {}
        }
    }
}

/// Fire-and-forget fan-out of a provider-attached `change_notification` to
/// the observer role named by `PHILOTIC_DATASOURCE_CHANGE_OBSERVER_ROLE`
/// (lifegraph-change-push seam). Unset (the default) emits nothing, so hotels
/// without an edge surface see zero extra traffic. The change ping is sent
/// AFTER the requester's reply and delivery failures are logged and
/// swallowed — a change notification must never fail the write it describes.
async fn forward_change_notification(
    ipc_client: &mut PhiloticClient,
    task: &DatasourceTask,
    provider_id: &str,
    change: Value,
) {
    let observer_role = std::env::var("PHILOTIC_DATASOURCE_CHANGE_OBSERVER_ROLE")
        .unwrap_or_default()
        .trim()
        .to_string();
    if observer_role.is_empty() {
        return;
    }
    let local_node_id =
        std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string());
    let result = ipc_client
        .send_request(IpcRequest::EmitTask {
            target_node: local_node_id,
            target_role: observer_role,
            target_guest_id: None,
            task_json: json!({
                "action": "datasource_change",
                "capability": task.kind.as_str(),
                "provider": provider_id,
                "change": change,
            })
            .to_string(),
        })
        .await;
    if let Err(err) = result {
        warn!("change-notification forward failed (ignored): {err}");
    }
}

async fn emit_success_response(
    ipc_client: &mut PhiloticClient,
    reply: &ReplyRoute,
    task: &DatasourceTask,
    provider_id: &str,
    output: ProviderOutput,
) -> Result<()> {
    let result_json = match output {
        ProviderOutput::ResultSet(value) => json!({"status": "success", "data": value}),
        ProviderOutput::PartitionCreated { graph_id } => {
            json!({"status": "created", "graph_id": graph_id})
        }
        ProviderOutput::Acknowledge => json!({"status": "acknowledged"}),
    };

    ipc_client
        .send_request(IpcRequest::EmitTask {
            target_node: reply.return_route.node.clone(),
            target_role: reply.return_route.role.clone(),
            target_guest_id: reply.return_route.guest_id.clone(),
            task_json: json!({
                "action": "datasource_response",
                "capability": task.kind.as_str(),
                "tool_name": task.kind.as_str(),
                "provider": provider_id,
                "return_route": reply.return_route.as_json(),
                "reply_guest_id": reply.return_route.guest_id,
                "session_id": reply.return_route.session_id,
                "turn_id": reply.return_route.turn_id,
                "chat_id": reply.chat_id,
                "result": result_json,
            })
            .to_string(),
        })
        .await?;

    Ok(())
}

/// Tell the caller a still-running provider is alive, so its phase watchdog
/// does not evict a healthy turn mid-tool.
///
/// Travels the SAME reply route as `datasource_response` — no new IpcRequest
/// variant and no hotel change, which also keeps it clear of the documented
/// `IpcResponse` untagged-ordering hazard.
///
/// Best-effort by design: a ping that cannot be sent is dropped, never
/// propagated. Failing the task because a keepalive failed would let a
/// diagnostic signal break the very work it is reporting on.
async fn emit_tool_progress(
    ipc_client: &mut PhiloticClient,
    reply: &ReplyRoute,
    task: &DatasourceTask,
    provider_id: &str,
) {
    let request = IpcRequest::EmitTask {
        target_node: reply.return_route.node.clone(),
        target_role: reply.return_route.role.clone(),
        target_guest_id: reply.return_route.guest_id.clone(),
        task_json: json!({
            "action": "tool_progress",
            "capability": task.kind.as_str(),
            "tool_name": task.kind.as_str(),
            "provider": provider_id,
            "return_route": reply.return_route.as_json(),
            "reply_guest_id": reply.return_route.guest_id,
            "session_id": reply.return_route.session_id,
            "turn_id": reply.return_route.turn_id,
            "chat_id": reply.chat_id,
        })
        .to_string(),
    };
    if let Err(err) = ipc_client.send_request(request).await {
        warn!(
            capability = task.kind.as_str(),
            "tool progress ping failed (best-effort): {err}"
        );
    }
}

async fn emit_failure(
    ipc_client: &mut PhiloticClient,
    reply: &ReplyRoute,
    capability: Option<&str>,
    provider: Option<&str>,
    message: String,
) -> Result<()> {
    emit_failure_with_sub_kind(ipc_client, reply, capability, provider, message, None).await
}

/// Same as [`emit_failure`], but lets the caller tag the failure with a
/// `sub_kind` (e.g. `"invalid_request"` for parameter/parse contract
/// failures) so downstream consumers like philote can distinguish a
/// malformed-parameters failure — worth one bounded model self-correction
/// retry — from a transport/routing failure, without string-matching
/// `message`.
async fn emit_failure_with_sub_kind(
    ipc_client: &mut PhiloticClient,
    reply: &ReplyRoute,
    capability: Option<&str>,
    provider: Option<&str>,
    message: String,
    sub_kind: Option<&str>,
) -> Result<()> {
    let mut payload =
        TaskErrorPayload::provider_failure("datasource_controller", capability, provider, message);
    payload.sub_kind = sub_kind.map(str::to_string);

    ipc_client
        .send_request(IpcRequest::EmitTask {
            target_node: reply.return_route.node.clone(),
            target_role: reply.return_route.role.clone(),
            target_guest_id: reply.return_route.guest_id.clone(),
            task_json: json!({
                "action": "datasource_response",
                "capability": capability.unwrap_or("unknown"),
                "tool_name": capability.unwrap_or("unknown"),
                "provider": provider.unwrap_or("unknown"),
                "return_route": reply.return_route.as_json(),
                "reply_guest_id": reply.return_route.guest_id,
                "session_id": reply.return_route.session_id,
                "turn_id": reply.return_route.turn_id,
                "chat_id": reply.chat_id,
                "error": payload,
            })
            .to_string(),
        })
        .await?;

    Ok(())
}
