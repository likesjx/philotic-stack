---
title: Native publisher lifecycle candidate
doc_type: proposal
domain: operator-control-plane
status: draft
last_updated: 2026-11-03
---

# Native publisher lifecycle candidate

This document describes isolated, unqualified work. It is not a production activation procedure.

The hotel is the authority and supervisor. A publisher must not mutate protected state until an authorized operation is bound to the hotel fence. An already permitted effect may complete before a later revocation; a completed write is not rolled back by denying its response.

The experimental direct-child owner acquires the fence before spawning, retains it until the owned child exits and is reaped, and leaves the journal dirty on timeout, panic or uncertain outcome. Caller cancellation observed while waiting for the fence prevents spawning; cancellation after the post-fence check is not revocation. A non-yielding session can retain the fence indefinitely. Runtime shutdown or owner-task panic can still leave a live child; a dirty journal blocks supported authority work after restart but does not itself terminate that child. Descendant termination and a start-permission barrier are not established.

The existing `ProtectedLocalGuest::stop()` retires through the authority fence before it kills its child. It therefore cannot be used unchanged to stop a publisher while that same fence is held. Its separate `Drop` path bypasses the authority fence, directly retires the generation and attempts `start_kill()` without confirming a reap. A test-only alternative kills and reaps the same-UID owned child while a caller supplies a fence, then retires its generation. The test-only argument is not a non-forgeable permit, and cancellation during stop/reap is not qualified.

The current protected materializer launches and registers children with the hotel's effective UID. It sets `PR_SET_NO_NEW_PRIVS` and parent-death signaling on Linux, but it has no demonstrated mechanism to launch a UID-0 publisher from an unprivileged hotel. Registration's expected UID is an authentication expectation, not a credential change or signal privilege. Parent-death signaling and `kill_on_drop` do not by themselves prove root publisher termination or reaping in all shutdown paths.

Two alternatives need an explicit design and qualification decision:

1. Retain the fence until a genuinely owned publisher exits. This needs no new stop privilege but may block indefinitely; it does not solve how an unprivileged hotel obtains an owned UID-0 publisher. Shutdown recovery must still prevent the publisher from making late effects and must reconcile the dirty journal with durable receipts.
2. Give the hotel a narrowly scoped way to ask the service manager to stop one pinned publisher incarnation and confirm its entire relevant process group has exited. This may bound interruption, but requires a concrete authorization policy, identity/cgroup semantics and approval for the new privileged operation. A broad kill capability is not assumed or proposed as an implicit consequence.

Before any runtime enablement, qualification must cover root/service UID separation; trusted launch and executable identity; operation, deadline and generation binding; caller cancellation; a non-yielding session; exact-child and descendant termination; hotel shutdown and restart; durable receipt and installed-state reconciliation; and Linux/Memgraph composition. An independently protected rollback witness remains a separate unresolved failure-domain decision. No local fixture proves these production properties.

The existing authority journal has an additional restart-recovery limitation: if writing `clean` becomes visible but the following length or sync operation fails, in-memory poison can be lost on restart and `Journal::open` may accept the visible clean marker. The test-only fault injection demonstrates visibility after a simulated failure before sync; it does not model power-loss persistence. Reopen must not be treated as proof that an uncertain lifecycle was reconciled. A durable design needs explicit startup reconciliation of authenticated operation evidence and quiescence; another local marker alone cannot guarantee correctness when storage writes fail. No automatic journal clearing is proposed.
