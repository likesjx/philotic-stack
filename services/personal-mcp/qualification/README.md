# Shared hotel synthetic qualification — 2026-10-21

Disposition: implemented on the qualification branch; isolated acceptance passed; production selection and rollout remain unqualified.

## Exact evidence

- Runtime source: 230d5aaa1ccdf9fe2b18fd14c7d66c91cff95219.
- Workflow/fixture revision: f7f1d8484df19c9e39bbc50c0435ae89b6bb55bf.
- [Successful run 39671418572](https://github.com/likesjx/philotic-stack/actions/runs/39671418572), job 119469989984.
- Tested Linux x86-64 release aiua SHA256: 7f789203dc87790740399885f093b940d22f66bff66dd6541c5fdf6f3345b9dd.
- [Artifact 12379818356](https://github.com/likesjx/philotic-stack/actions/runs/39671418572/artifacts/12379818356), archive digest 0e747ae7c5e0653c782becb1f916de114353fc866d76bd357678ed765309abf3, expires 2026-11-04. Includes executable, source-input hashes, compiler/package identities, checksums, isolation inspection, fixture identities and result receipts.
- Historical hotel reused from run 39665912963 artifact 12377009217: source 56b0cdc2ed1123aad01b22eb88fa7b43baf02bf6, executable SHA256 0beb8f1c02bb12c34acdedbf5a062950465e0dc681cbf3fdeb0b6a49f6254c82. This rebuild does not match recorded installed SHA256 b7d0c9703b3670efc5b93cfc8cb2561b3f80fe7d0d94be65c400987751eb7a17. Different build inputs/patches can produce different bytes; this mismatch does not disprove the documented source.

## Changes and acceptance

Startup credential migrations now share an owned SQLite transaction on one connection, with rollback on migration/COMMIT error and unwinding. Existing provider references are checked for reference/kind, permitted provider role, empty guest restrictions, decryption and nonempty value before duplicate plaintext is removed. Guest-restricted references fail while preserving plaintext; no ACL is widened. The transaction covers credential migrations, not preceding config import/rotation during aiua load.

Passing evidence:

- Three serial filtered migration units (700 other tests not executed), including valid model-only/wildcard role references with unchanged ciphertext/reference/registry.
- Eight state scenarios: historical-source SDK hops against both hotels; legacy columns/data; repeat/idempotent migration and valid existing references; seven invalid-reference variants; failed config-write atomicity and unique retry; SIGKILL during a startup SQLite write and recovery; COMMIT-busy rollback with exact error-boundary evidence; consistent synthetic snapshot plus executable-selection restore and historical SDK roundtrip.
- Eighteen scoped-vault harness tests plus Linux numeric-peer/FD/framing/resource-bound/lifecycle scripts and actual release-hotel bootstrap/SIGTERM/fresh-process recovery.

The controlled historical guest proves local SDK membrane→agent→model→agent→membrane message/reply hops, content/session/turn/task correlation, config and secret allow/deny, persistence and reconnect. It does not run actual provider event loops or complete real agent turns. SIGKILL coverage is a startup-write interruption, not proof of interruption at every instruction.

Runtime execution used a disposable Linux container, network none, zero host mounts, no privileged mode and clean synthetic environments/state/key files. No production data, master key, installed executable, external integrations, mac-jane checkout writes/builds, merge, deploy or sidecar was involved.

## Failed evidence retained

- [39668362194](https://github.com/likesjx/philotic-stack/actions/runs/39668362194): original retained candidate d49b977c…; five invalid-reference startup failures and one orphan secret/registry failure exposed the two migration defects.
- 39669556130: workflow syntax failure, no jobs executed.
- [39669654466](https://github.com/likesjx/philotic-stack/actions/runs/39669654466): first fix source 8c880f88…; invalid refs/atomicity/rollback/broker tests passed, but the valid-reference positive case failed because hotel.internal is not an ACL bypass. Validation was corrected to use an allowed provider role. Assertions were retained and a positive unit was added.
- Completed activation/recovery run 39653053760 was reused, not rerun.

Independent review covered source, fixture rigor, isolation and workflow effects; findings were addressed. Confidence is isolated smoke-green only. Memory/runtime registry writes and harness telemetry were excluded by task scope.

## Remaining gates and next action

Installed provenance remains unresolved. Historical-source proxies are not installed-binary equivalence. The full parallel workspace suite, actual installed guest population/provider loops, real state/key migration and systemd rollout/rollback have not run. Legacy memory-refresh calls are explicitly refused by candidate source; consumers requiring that protocol need compatibility selection.

Next obtain only an owner-projected non-sensitive inventory to select further fixtures: guest role/build/hash/protocol families; effective supervisor dependencies/readiness/stop settings; SQLite DDL/indexes and record field/type/presence/count shapes; credential-source mechanism/presence and ACL categories (including guest-restricted references), plus validity/decryptability booleans where needed. No credentials, credential hashes, memory/session payloads, raw configuration/environment or copied installed binary are required for that inventory. This note does not authorize querying production or activation.
