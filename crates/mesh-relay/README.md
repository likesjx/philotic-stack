# Mesh relay source consolidation

This is a source-only preservation candidate from `codex/mesh-relay-transport`.
There is no hotel consumer, runnable binary, service or deployment wiring. No
production exposure is authorized. Seventeen local tests pass, including synthetic
authentication, disposable loopback QUIC and reconnect tests; they do not prove
hotel failover or production readiness.

QUIC protects each network hop. The relay terminates TLS and can inspect payload
bytes. Authentication proves possession of a configured member identity key;
recipient validation of inner signed frames belongs to future hotel integration.

Before activation, resolve:

- Unbounded server outbound queues and absent slow-recipient overflow policy.
- Authentication deadlines and connection/concurrency admission limits.
- Writer cleanup on handshake errors/cancellation, including blocked writes.
- Supervisor shutdown while disconnected or repeatedly failing to connect.
- Outbound frame-size enforcement and byte budgets; client message counts do not
  bound memory for arbitrary byte vectors.
- Send failures and undeliverable frames: enqueue success is not acknowledgement,
  automatic direct-path fallback or durable/exactly-once delivery.

Add saturation, stalled authentication, cancellation, disconnected shutdown,
oversized-frame and delivery-failure tests before treating those limits as fixed.
Original branch history remains preserved; no active mesh path has changed.
