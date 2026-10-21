# Existing-hotel memory activation and recovery

This extends the same default-off role with `memory-activate-v1` and
`memory-recover-v1`. Neither profile stages files or replaces the hotel binary.
They are separate from `memory-stage-v1`, whose `activate` operation still refuses.
No production execution or broker qualification is implied by these tasks.

The only admitted hotel bytes are installed/running
`b7d0c9703b3670efc5b93cfc8cb2561b3f80fe7d0d94be65c400987751eb7a17`.
The existing provenance helper checks the effective executable and actual running
inode. An independent same-binary qualification receipt must bind this exact
executable hash, source revision, public build ZIP digest/run/repository, and
qualification fixture hash. A missing receipt refuses. Source-label equality is
not behavioral qualification. Fixture receipts are synthetic test evidence only.

## Explicit inputs

`percival_memory_manifest`, `percival_memory_qualification`, and
`percival_memory_approval_sha256` are closed public contracts validated by
`percival_memory_contract.py`. The approval SHA is SHA256 of sorted compact ASCII
JSON (`canonical` in that module), not arbitrary formatting. There is no valid
example or executable default approval. Inputs require:

- Exact staged gateway709 closure, qualified Node24 hash, and public configuration
  digest; fixed whole-default-vault recall mapping, canonical issuer/broker and
  no LifeGraph/authority fields or inline credential values.
- Thirty-minute approval, unique transaction ID, same-binary qualification digest,
  base gateway unit, existing95 application drop-in, discovery configuration,
  approved discovery Node22, and stable supervisor-control hashes. Four existing
  discovery modules must match gateway709 source bytes.
- Exact issuer2031/image and absent authority/coordination owner attestations.
  The role does not inspect issuer environment or query Mongo.
- Explicit pairing, disabled-admission, private/future read-only vault scope,
  operator quiescence, policy adoption and shared hotel/gateway interruption
  attestations. These are owner assertions requiring independent evidence, not
  capabilities established by this program.
- Native-key attestation: `default`, `observe`, expiry and verification epochs,
  not revoked, matching the exact approved policy reference. Verification must
  precede approval by less than30minutes and activation approval must end before
  key expiry. No credential retrieval, provisioning or renewal is implemented.
- Exact installer justfile/configuration/playbook/task/module/helper closure. Controller files are hashed by the
  role; loaded remote helper/module sources are checked against the same map.
  Retain the entire exact operator release for recovery; upgrading the operator
  cannot reinterpret an earlier journal.

Before Ansible evaluates variables, the justfile entry validates the public JSON
file locally using `percival_memory_contract.py --validate-vars FILE --mode
activate|recover --approval-sha SHA`. Only the three public variable names above
are accepted; duplicate JSON keys, extra connection/profile/fixture settings,
stale approvals, mismatched explicit hashes and changed local operator files
refuse. `--emit-vars` returns the canonical validated public snapshot; the wrapper
passes this captured JSON directly to Ansible and does not reread the input file. The stage entry similarly accepts only package directory, configuration
file and configuration hash, with bounded plain absolute paths. These commands
validate inputs; successful local validation is not live approval or inspection.

The entry uses the dedicated public `ansible/muninn-memory.cfg`, included in the
operator hash closure. It disables automatic variable plugins and keeps host-key
checking enabled; no global repository vault-password configuration is inherited.

The production target is fixed to root on srv875272. Existing gateway1003/1003
and hotel999/987 identities, isolation, socket ownership/mode and loaded socket
relationships are checked. Accounts, socket units, hotel drop-ins, installed
hotel bytes, base gateway unit, discovery configuration, and existing
`95-application-709c0d7e.conf` remain unchanged. Unknown gateway drop-ins refuse.

## Ordered owned changes

The role records an exclusive public transaction under
`/var/lib/percival-memory-transactions/` before service changes. It stops the
gateway, adopts the exact public policy, creates only owned
`99-muninn-memory.conf`, reloads systemd, ensures the existing socket is started,
restarts the existing hotel to load policy, verifies running provenance, and
starts the gateway. The99 drop-in overrides but preserves95, selects the staged
Node/server/config, and adds socket/hotel ordering. Loaded commands, isolation,
actual running gateway executable, zero queued jobs, and owned file state are
checked at the relevant checkpoints. No gateway/broker/native credential request
or memory call is made; these checks are not real recall acceptance.

Only the policy and owned override mutate through a locked helper, with immediate
expected-content/inode checks, atomic no-replace override publication, atomic
policy replacement, and fsync. Journal records and the owned override are fully
written and synced under temporary names before Linux `renameat2(RENAME_NOREPLACE)`
publishes their final names. Existing final files are never replaced. Interrupted
journal temporary files are ignored only when their names, root ownership, regular
file type, single link, private mode and size match the fixed publication contract. This assumes cooperating privileged operators honoring
the quiescence window/lock. It does not claim atomic file CAS against an
uncooperative root writer. Service sequencing remains in Ansible.

Each action has a durable pending checkpoint before execution and a completed
checkpoint after verification. An uncertain command leaves pending state. Rescue
only requests gateway stop and then fails; it does not restore policy, restart
the hotel, delete records or retry an operation. Gateway-stop failure is itself
uncertain and requires operator inspection. A preparation failure can leave an
inert ownership record/directory; it never authorizes implicit cleanup/retry.

## Explicit recovery, including interrupted recovery

Supply a fresh recovery manifest preserving the original artifact/config/policy,
issuer/key attestations and operator closure, with `recovery.activation_sha256`,
`previous_record_sha256`, `previous_approval_sha256` (null for first recovery),
and `commands_quiescent:true`. The last record and prior approval must match
exactly; selected systemd units must have zero jobs. This is new authorization,
not retry authority from a failed exit code. After an interrupted recovery,
another fresh envelope must name its exact last record and prior recovery hash.

Recovery stops the gateway, restores only the captured approved public policy,
removes only the matching owned99 override, reloads, starts the existing socket,
restarts the unchanged hotel, and starts the original discovery gateway. Missing
or corrupt new staged bytes do not prevent recovery. Missing socket is allowed
only before the existing socket start; readiness must then be established before
hotel/gateway startup. A foreign replacement of owned files refuses rather than
being removed. Discovery source/config/runtime drift refuses. No unrelated file
or service rollback occurs. Existing95 and base configuration are preserved.

The original key attestation is validated historically; expiry does not block
closing access or restoring discovery. No new native-key validity claim is made
by recovery. Mongo is never queried or written: any admission rollback remains
with the separate issuer operator and its forward-only versions. Journals and
exclusive ownership marker are retained even after recovery; cleanup/new rollout
requires separate explicit review.

## Validation boundary

`memory_activation_contract_test.py` is pure local validation. The separate
`memory_activation_acceptance.py --node <qualified-node>` runs real Ansible only
inside an explicitly marked root disposable Linux container. It uses a synthetic
supervisor/provenance model beneath a fixed `/run/...` fixture root, actual exact
staged public bytes, and disposable socket metadata. It never executes Node,
hotel, systemd, credentials, or application code. It tests success, refusals,
replay, foreign override/source drift, damaged candidate recovery and uncertain
hotel failure and explicitly reauthorized interrupted recovery. Passing fixtures are not installed-broker qualification or live
activation approval.
