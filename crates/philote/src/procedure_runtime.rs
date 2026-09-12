//! Procedural graphs — runtime side (doc:procedural-graphs).
//!
//! P1 `procedure-run-ledger`: after a plan reaches a terminal verdict the turn
//! loop drains `SessionState::pending_procedure_run` here and appends it to the
//! hotel's `procedure_run` ledger. The send is best-effort: a hotel that
//! refuses or times out costs one ledger row and a warning, never the turn.

use super::*;
use ansible_mesh_core::procedure::ProcedureRunRecord;

/// Upper bound on how long a ledger append may hold the turn loop.
const RECORD_PROCEDURE_RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl AgentRuntime {
    /// Append one terminal plan evaluation to the hotel's procedure run
    /// ledger. Best-effort by design (see module docs).
    pub(super) async fn record_procedure_run(&mut self, session_id: &str, run: ProcedureRunRecord) {
        let run_json = match serde_json::to_value(&run) {
            Ok(v) => v,
            Err(err) => {
                warn!(
                    session_id = %session_id,
                    procedure_id = %run.procedure_id,
                    error = %err,
                    "procedure run not recorded: serialize failed"
                );
                return;
            }
        };
        let send = self
            .ipc_client
            .send_request(IpcRequest::RecordProcedureRun { run: run_json });
        match tokio::time::timeout(RECORD_PROCEDURE_RUN_TIMEOUT, send).await {
            Ok(Ok(IpcResponse::Standard { ok: true, .. })) => {
                info!(
                    session_id = %session_id,
                    procedure_id = %run.procedure_id,
                    graph_version = run.graph_version,
                    verdict = %run.verdict,
                    score = run.score,
                    tools = run.tool_sequence.len(),
                    "procedure run recorded"
                );
            }
            Ok(Ok(IpcResponse::Standard { code, message, .. })) => {
                warn!(
                    session_id = %session_id,
                    procedure_id = %run.procedure_id,
                    code = %code,
                    message = %message,
                    "procedure run refused by hotel"
                );
            }
            Ok(Ok(other)) => {
                warn!(
                    session_id = %session_id,
                    procedure_id = %run.procedure_id,
                    response = ?other,
                    "procedure run: unexpected hotel response"
                );
            }
            Ok(Err(err)) => {
                warn!(
                    session_id = %session_id,
                    procedure_id = %run.procedure_id,
                    error = %err,
                    "procedure run not recorded: IPC error"
                );
            }
            Err(_) => {
                warn!(
                    session_id = %session_id,
                    procedure_id = %run.procedure_id,
                    "procedure run not recorded: hotel did not answer within {:?}",
                    RECORD_PROCEDURE_RUN_TIMEOUT
                );
            }
        }
    }
}
