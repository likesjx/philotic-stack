# Correlated memory refresh

`memory.fix` previously waited indefinitely for `RefreshMemoryConfig`. Its
solicited `MuninnStatus` reply shared the same shape as unsolicited reachability
broadcasts, so the client buffered the reply as a push and kept waiting. A tool
executed inline in philote could then block later chat inputs and watchdog work.

The hotel now accepts `refresh_memory_config_correlated` with a UUID and emits
`memory_config_refresh: {request_id, available, endpoint}`. The unique outer field
keeps the untagged response parser from confusing a reply with a status broadcast.
The client accepts only the requested UUID and preserves unsolicited broadcasts.
Both philote and the heal dispatcher use the typed `refresh_memory_config` API.

Philote uses a ten-second whole-operation deadline, including a backpressured
write, reply read, and subsequent tool-result/checkpoint processing. An outer
tool guard closes the connection if that later processing stalls or is cancelled.
Dropping the future, timing out, receiving a mismatched ID
or receiving an unsupported reply closes that connection. Partial frame bytes
and stale-reply accounting are discarded; already buffered pushes remain readable.
The caller must reconnect before sending further requests. This deliberately
does not attempt to replay an abandoned operation or guess which legacy reply
belongs to it. The guest runtime's existing disconnect recovery remains responsible
for reconnect/restart and persisted-turn cleanup; this change does not grant a
new restart authority or promise that an error can be delivered on a closed IPC
connection.

Version compatibility:

- New hotel and new guest: correlated reply, bounded operation, no broadcast loss.
- New guest and old hotel: the unknown operation's refusal (or a silent peer's
  deadline) becomes an explicit protocol error and closes the connection.
- Old guest and new hotel: legacy refresh returns
  `MEMORY_REFRESH_PROTOCOL_REQUIRED`, never an ambiguous `MuninnStatus` reply.
  Older philote reports an unrecognized probe response; old heal dispatchers
  report probe failure. Neither receives the status frame that caused the hang.
- Old hotel and old guest: unchanged, including the existing defect.

Upgrade hotel, philote and heal-dispatcher together for working refresh. An
isolated philote upgrade against an old hotel fails safely but cannot refresh
memory. Source preparation is not authorization to deploy or restart anything.

Verification uses synthetic socket pairs and an endpoint-free hotel fixture:
wire discrimination, matching/wrong IDs, broadcasts before and after replies,
subsequent request framing, legacy refusal, old-hotel refusal, late replies after
timeout, cancelled partial-frame reads, backpressured-write deadlines/cancellation,
and whole-tool timeout/cancellation after a probe has already answered.
