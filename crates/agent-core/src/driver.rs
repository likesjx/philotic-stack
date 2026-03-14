/// Shared driver interface for Philotic agent runtimes.
///
/// Both the persona runtime ([`crate::runtime::AgentRuntime`]) and the subagent
/// worker runtime (`WorkerRuntime` in `agent-worker`) share this contract:
/// connect to the hotel IPC, then run the event loop until graceful shutdown
/// or fatal error.
///
/// # Implementing a new runtime
///
/// 1. Create a struct that holds a [`philotic_client::PhiloticClient`].
/// 2. Implement this trait with a `run` body that loops on `ipc_client.recv_task()`.
/// 3. Return `Ok(())` on clean hotel disconnect, or propagate fatal errors.
#[allow(async_fn_in_trait)]
pub trait AgentDriver {
    /// Boot the runtime and run the event loop until disconnect or fatal error.
    async fn run(&mut self) -> anyhow::Result<()>;
}
