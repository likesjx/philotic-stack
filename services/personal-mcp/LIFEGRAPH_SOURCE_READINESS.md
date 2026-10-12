# LifeGraph source readiness and bounded rollout

This source slice builds on PR653. It implements a Linux canonical SQLite reservation owner, a bounded pinned catalog loader, a fixed-operation Unix socket client, and a synthetic-only composition. Production namespaces remain denied. Neither the client nor its same-UID socket checks constitute an authenticated owner server. No real data, server configuration, credential or grant was inspected or changed.

## Implemented ownership

`lifegraph-policy-owner.mjs` reserves the existing version-2 canonical privacy database with `BEGIN IMMEDIATE`. It never initializes a store or writes policy, grant, schema or revision rows. Installation must supply the reviewed device/inode identity and a privately owned existing file. Both reservation and read-only policy connections verify that same identity through pinned FDs. Only DELETE journal mode is supported; WAL sidecars cannot safely be resolved through the FD alias and are denied until a separately reviewed owner path is implemented. Replacement or permission changes invalidate the lease at admission and response handoff. The coordinator holds that reservation through response completion. Catalog files are approved read-only exports of canonical owner bindings, not another policy authority.

`lifegraph-owner-transport.mjs` sends only four fixed read operations plus snapshot begin/finish over a private same-UID Unix socket. Frames, rows, commands and deadlines are bounded; owner/session/sequence correlation is checked. No raw Cypher, credential, grant or role crosses this API. The supervised server still has to authenticate kernel peers against registered launch identity, enforce the fixed operations, and retain ownership until rollback/server quiescence on cancellation. A client disconnect alone cannot prove transaction completion.

`lifegraph-owned-storage.mjs` composes these implementations with the existing reader and response coordinator. It requires an issuer-owned current-actor resolver and only accepts `synthetic-read-only-v1`. Disabled mode does not inspect a catalog or database. It is an owner integration entrypoint, not a registration in Muninn's shared `server.mjs`; that file and issuer/broker deployment belong to the Muninn rollout owner.

## Actual writer and revoker inventory

| Entry point | Source disposition |
| --- | --- |
| `data-memorygraphrag::provider` connect/invoke: observe/batch, recall feedback, commit, conflict resolution, patch propose/apply, node edit, loop actions and tidy | Required-mode guard refuses the whole unfenced provider before connection or dispatch. Existing autocommit and timeout paths cannot safely release a lease on a dropped future. |
| Provider heartbeat reminders and hygiene timers in `main.rs` | Fresh provider connections are guarded; exported `hygiene::sweep` and `loop_action::apply` are independently guarded. |
| `graph-datasource::MemgraphProvider` arbitrary query tasks | Both connection and invocation guarded; raw queries are not treated as enrolled read operations. |
| `data-memorygraphrag/examples/{observe,recall,paracrine}_smoke.rs` | Direct graph connections guarded at main entry. Do not run against real records for acceptance. |
| Web edge LifeGraph edits/observe and Python ingest/batch probes | Route through the guarded provider. Scripts were inspected, not executed. |
| `scripts/idea-sweep.sh` SSH/mgconsole triage writes | Guard runs before SSH or host discovery. Required and unknown modes deny. |
| Raw `scripts/lifegraph-patches/*.cypher`, architecture migration Cypher, external mgconsole/third-party clients | Unenrolled. An environment guard in this checkout cannot fence external writers. Production admission is blocked until the owner inventories and excludes or supervises all such capabilities. |
| Canonical `PolicyStore` insert/revoke/cancel/enqueue operations | Existing SQLite transactions serialize with the actual reservation. Runtime database ownership/path is unverified; no live store initialization permitted. |
| Personal OAuth role/grant/revocation, client disable, principal/session change | Actual separate issuer checkout/commit is unavailable. Legacy website API-token code is not substituted. Muninn owner must provide the current source and enroll every authority-changing transaction in the same coordinator before admission. |
| Local launch registry/task receipts | Existing opt-in authenticated local RPC machinery is not external OAuth identity. No local receipt is promoted into an external client grant. |

`PHILOTIC_LIFE_EXTERNAL_READ_COORDINATION` unset or exactly `disabled` preserves existing internal routes. Any other value refuses the listed unfenced routes. This is a source transition guard, not a write lease and not evidence that remote processes have adopted it. Enabling required mode changes service behavior and needs its own reviewed deployment action. Reads from the external composition still deny production in all modes.

## Finite remaining implementation plan

1. Obtain the issuer owner's exact reviewed repository/commit and coordinate file ownership. Implement transaction participation for grant/role revocation, disable, principal/session changes and current-actor resolution. Prove revocation cannot pass response handoff and one client's change cannot authorize another.
2. Implement the supervised Unix owner server using the verified production driver's transaction API, registered kernel peer identity and cancellation/quiescence acknowledgement. Bind the graph revision/fence and snapshot to that owner. A same-process coordinator is insufficient for writers in other processes.
3. Produce a metadata-only installation manifest identifying the canonical store, approved catalog export authority, deployed writer binaries and all direct writer capabilities. Resolve missing source provenance/root bindings without reads by this assistant. An incomplete inventory keeps admission disabled.
4. Refactor retained graph writers into owner-controlled transactions with fence ownership lasting through server completion, or remove their production write capability. Default-disable mutations and semantic dedup activation. Explicit canonical roots/approved aliases resolve variants; semantic similarity alone never establishes deterministic equivalence.
5. Run synthetic integration with the actual owner server and issuer, including cancellation, restart, revocation, two clients, incomplete provenance, unmanaged writers and canonical-store replacement. Require independent review and exact-head CI on the resulting commit.
6. Present exact deployment and admission diffs to the operator. Roll out disabled first; admit each independently approved read-only client only after all owner checks pass. Existing memory-only clients never receive `life:recall` implicitly.

## Bounded metadata-only probe proposal (not executed)

Use an already authorized owner-operated channel. Do not guess connection details, SSH credentials or graph queries. Collect only: deployed binary/version and digest; allowlisted supervisor unit names and non-secret coordination flags; database driver/version/storage mode/isolation capability; schema labels/property names and index/constraint definitions restricted to reviewed LifeGraph schema; canonical SQLite `user_version`, table column definitions, file device/inode/UID/mode; issuer source version and transaction hook names; and a redacted inventory of direct writer capability classes with supervised/unmanaged status. Omit graph counts, IDs, property values, policies, people, summaries, environment dumps, tokens, credential hashes and admission records. The owner must review the exact commands and output filter before running the probe. Metadata must establish server version/edition capability before assuming RBAC or write exclusion is available.

## Exact action-time approvals still needed

* Deploy the reviewed binary/unit changes in default-disabled mode, identifying hosts, digests and rollback commands.
* Set required coordination mode on each known legacy writer/service; this deliberately blocks its unfenced routes.
* Bind the reviewed existing canonical store path/identity and reservation access; never initialize or migrate it under this approval.
* Install an owner-approved catalog export and provenance/root binding manifest; reading/exporting real personal identifiers or policies is a separate bounded action.
* Install the supervised broker socket/kernel-peer registration and dedicated graph read credential/role with exact privileges; no persistent secret or role is provisioned by this slice.
* Exclude/revoke unmanaged writer capabilities or deploy reviewed transactional writer changes, listing every affected client and rollback consequence.
* Create explicit per-client `life:recall` grants for dot and Claude with exact identity, scope, namespace, expiry and revocation path; each requires independent approval, separate from memory access.
* Enable production namespace admission only after complete coverage, independent review, exact-head CI and synthetic end-to-end evidence. Write/dedup activation requires a separate bounded proposal and approval.

No merge or production action is performed by this source work.
