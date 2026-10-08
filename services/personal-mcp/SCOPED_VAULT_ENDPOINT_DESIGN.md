# Hotel-owned single-reference endpoint

Current source, evidence and outstanding gates are recorded in
[SCOPED_VAULT_INTEGRATION_CONTRACT.md](SCOPED_VAULT_INTEGRATION_CONTRACT.md).
This replaces the historical unwired design and excessive-authority Python broker.

The gateway has only a fixed credential operation on a dedicated mode0600 socket.
The hotel checks kernel UID, immutable root policy and exact singleton vault ACLs.
A gateway compromise must not provide general hotel IPC, database or root-key access.
The hotel remains the decrypting trust boundary; host root remains trusted.

The main caller and validated prebound descriptor adopter are now source-wired.
Live supervisor/socket/key/client/service changes are still unapproved and unexecuted.
The candidate supports Percival only; Claude requires a separate reviewed client/key
and endpoint policy after its cloud product is identified. Private recall stays held.
