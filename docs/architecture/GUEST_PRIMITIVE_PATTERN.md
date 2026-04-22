# Guest Primitive Pattern — Constitutional Specification

## Summary

Every major guest type in the Philotic Stack follows a two-layer structure:

- **Base crate** (`lib`) — defines the core trait, protocol-agnostic primitive types,
  and shared runtime plumbing. Publishable as a standalone crate. External repositories
  can import it and build their own implementations that plug into any compatible mesh.
- **Variant crates** (`bin`) — implement the trait for a specific protocol or provider.
  They depend on the base lib and on `philotic-client` for IPC.

This mirrors how `ansible-mesh-core` + `philotic-client` serve the hotel side: a stable
primitive layer that third-party code can build on.

---

## Guest Type Map

| Base crate (lib)    | Variants                                                  | Trait              |
|---------------------|-----------------------------------------------------------|--------------------|
| `membrane`          | `membrane-telegram`, `membrane-discord`, `membrane-mcp`  | `MembraneGuest`    |
| `tool-runner`       | `tool-runner-bash`, `tool-runner-workspace`, …            | `ToolRunner`       |
| `model-router`      | `model-controller-gemini`, `-onnx`, `-elevenlabs`, …      | `ModelProvider`    |

> `model-router` already implements this pattern with `ModelProvider` + `ProviderRegistry`
> in `src/lib.rs` and variant binaries in `src/bin/`. That is the reference implementation.

---

## Base Crate Contract

A base crate MUST:

1. **Depend only on `philotic-client` and `ansible-mesh-core`** as Philotic SDK deps.
   No variant-specific deps (Telegram client, axum, ONNX runtime, etc.) belong in the base.
2. **Export a single primary trait** (`MembraneGuest`, `ToolRunner`, `ModelProvider`)
   that variants implement with `#[async_trait]`.
3. **Export protocol-agnostic primitive types** — the envelopes and result types that
   flow through the trait boundary.
4. **Provide a `Runtime` struct** that handles the IPC lifecycle (register, lease, reconnect
   loop, inbound/outbound dispatch) and calls into the trait implementation.
5. **Be `no_std`-compatible at the type level** — primitive types (`InboundEnvelope`,
   `OutboundReply`, `ToolInvocation`, etc.) must not require a Tokio runtime in their
   definition, only in their usage.

---

## Membrane Pattern

### Trait

```rust
#[async_trait]
pub trait MembraneGuest: Send + Sync + 'static {
    /// IPC guest role name (e.g. `"telegram-membrane"`, `"mcp-membrane"`).
    fn role(&self) -> &'static str;

    /// Lease key for acquire/renew/release cycle.
    fn lease_key(&self) -> String;

    /// Protocol-specific setup: acquire lease, start listener (poll loop,
    /// HTTP server, WebSocket). Called once after IPC registration.
    async fn setup(&mut self, client: &mut PhiloticClient) -> Result<()>;

    /// Deliver an outbound reply from the hotel to the external caller.
    async fn deliver(&mut self, reply: OutboundReply) -> Result<()>;

    /// Called on the lease-renew interval. Variant sends the appropriate
    /// renew IPC request (RenewTelegramPollLease, RenewMcpMembraneLease, etc.).
    async fn renew(&mut self, client: &mut PhiloticClient) -> Result<LeaseRenewResult>;

    /// Called on clean shutdown. Variant releases its lease.
    async fn teardown(&mut self, client: &mut PhiloticClient);
}
```

### Primitive Types

```
InboundEnvelope     — protocol-agnostic inbound message (session, turn, sender, content)
SenderInfo          — who sent the message (id, display_name, is_operator)
OutboundReply       — what the hotel sends back (Text, StreamingToken, Error, ApprovalRequired)
MembraneContext     — handle passed to start(); exposes inbound_tx + shutdown signal
LeaseRenewResult    — Ok(epoch) | NeedsReacquire | Lost(owner)
```

### Runtime

`MembraneRuntime::run<G: MembraneGuest>(guest)`:

1. Connect + register as IPC guest.
2. Call `guest.setup(client)` — variant acquires its lease and starts its listener.
3. Spawn IPC receive loop: push messages → `guest.deliver()`.
4. Spawn lease-renew task: on interval call `guest.renew(client)`.
5. On IPC disconnect: reconnect with backoff, re-register, re-acquire lease, call `guest.setup()`.
6. On SIGTERM: call `guest.teardown(client)`, exit cleanly.

---

## Tool Runner Pattern

### Trait

```rust
#[async_trait]
pub trait ToolRunner: Send + Sync + 'static {
    fn id(&self) -> &'static str;
    fn supported_classes(&self) -> &[&'static str];
    async fn invoke(&self, invocation: ToolInvocation) -> Result<ToolResult>;
}
```

### Primitive Types

```
ToolInvocation      — tool_ref, args (Value), caller_context, correlation_id
ToolResult          — content (Value), artifacts, error flag, trace
ToolRunnerRegistry  — resolves ToolInvocation → ToolRunner (same pattern as ProviderRegistry)
```

---

## Model Controller Pattern (existing — `model-router`)

Already implemented. Naming: `ModelProvider` trait, `ProviderRegistry`, `ControllerTask`,
`ProviderOutput`, `ControllerResponseEnvelope`. The `model-router` crate is the base lib;
`src/bin/model-controller-*.rs` are the variant binaries.

**Pending rename**: `model-router` → `model-controller` (crate name only; no API changes).

---

## External Repo Integration

An external developer who wants to build a custom membrane for their mesh:

```toml
[dependencies]
membrane = { git = "https://github.com/philotic/philotic-stack", tag = "v0.x.y" }
philotic-client = { git = "...", tag = "..." }
async-trait = "0.1"
```

```rust
struct MyProtocolMembrane { /* ... */ }

#[async_trait]
impl MembraneGuest for MyProtocolMembrane {
    fn role(&self) -> &'static str { "my-protocol-membrane" }
    fn lease_key(&self) -> String { "my-protocol:primary".into() }
    async fn setup(&mut self, client: &mut PhiloticClient) -> Result<()> { /* ... */ }
    async fn deliver(&mut self, reply: OutboundReply) -> Result<()> { /* ... */ }
    async fn renew(&mut self, client: &mut PhiloticClient) -> Result<LeaseRenewResult> { /* ... */ }
    async fn teardown(&mut self, client: &mut PhiloticClient) { /* ... */ }
}

#[tokio::main]
async fn main() -> Result<()> {
    MembraneRuntime::new(args.ipc_socket, args.guest_id)
        .run(MyProtocolMembrane::new(args))
        .await
}
```

The guest is registered with the hotel automatically. The hotel materializes it like any
first-party membrane.

---

## Migration Status

| Guest type      | Base lib exists | Trait defined | Runtime shared | Variants migrated          |
|-----------------|-----------------|---------------|----------------|----------------------------|
| `model-router`  | ✅              | ✅            | ✅             | gemini, onnx, elevenlabs   |
| `membrane`      | 🔄 in progress  | 🔄            | 🔄             | mcp (new), telegram/discord (pending) |
| `tool-runner`   | ❌ pending      | ❌            | ❌             | —                          |

---

## Invariants

- Base crate version is the coordination point. All variants of the same type must depend
  on the same major version of their base.
- Primitive types are `Serialize + Deserialize` so they can cross process boundaries if needed.
- The `Runtime` struct handles reconnect transparently — variants never see IPC disconnects.
- A variant that fails `setup()` causes the runtime to retry with exponential backoff, not crash.
