# Protected text source acceptance

`protected_text_acceptance.rs` exercises one synthetic ordinary text attempt.
The parent test owns the child process and attaches it to the canonical
`LocalLaunchRegistry`. Kernel Unix peer credentials establish the authenticated
session; the child's JSON administrator claim does not. The parent owns a
temporary `PolicyStore`, `LocalTaskAuthority`, exact payload manifest and the
server-classified endpoint map. The child uses the real SDK over length-prefixed
Unix IPC and calls `model_request_payloads_with_rpc_recall`.

The canonical catalog is fixed synthetic fixture data, not a production lookup
or a live vault. It matches the entire recalled record, including provenance and
annotations, and its actor/human/session relationship. Content, provenance,
metadata and subject mismatches omit recall. Missing runtime authority continues
to omit recall through the ordinary assembly path. Stored records and complete
tool arguments/results remain unchanged.

The parent independently renders the expected complete outgoing fixture task,
including prompt, context, context projection and tools. The fake renderer fixes
only the clock instruction to a deterministic synthetic time before binding;
production clocks and serializers are unchanged. The canonical manifest accepts
only those exact complete bytes. Assembling context changes bytes, so the outgoing
attempt uses a fresh task ID/envelope from the same issuer, never the original
inbound grant. A fixture-only fake text provider resolves the outgoing authority
afresh at its actual endpoint before recording invocation. Changed payload bytes,
tampered envelope, a local cloud proxy classified External, and revoked policy
produce zero fake-provider calls.

Pre-assembly revocation/cancellation and a wrong generic RPC ACK return an error
without legacy fallback. For cancellation/revocation during assembly, the
blocking canonical lookup signals a bounded fixture gate; the parent changes the
canonical authority before releasing it. The assembler's post-worker RPC then
denies the whole result. This proves admission denial, not active abort/join,
provider cancellation or publication fences.

Verification commands (CI; local builds held for this increment):

```sh
cargo test --locked -p philote --test protected_text_acceptance -- --test-threads=1
```

The dedicated `Protected context text acceptance` workflow runs this exact target
on Linux for changes to Philote, SDK/shared authority, Cargo inputs and the
workflow itself. Normal workspace tests also run it on macOS. The ignored child
entrypoint is launched only by the parent fixture and is not a stack service.
All SQLite files, gate markers and sockets are generated in an owned temporary
directory and cleaned together with the owned child. No production mounts,
credentials, grants, vaults or provider network calls are used.

Production completion remains separate: canonical memory owner/read interface,
protected launch/inbound/park wiring, complete real outgoing serialization binding,
fresh asynchronous per-candidate dispatch gates across retries/fallback/credential
rebuilds, guarded registry installation, graph/native handling and quiescence.
This fixture does not install `guarded_registry`, add a production catalog or
claim the ordinary runtime is now a protected memory feature.

Related source boundary: [context management](../../../docs/architecture/PHILOTE_CONTEXT_MANAGEMENT_SLICE.md).
