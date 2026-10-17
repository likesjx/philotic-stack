# R3 preflight evidence correction

The one approved run39044226721 failed before stage() on the shared assertion
that service and slice LoadState must both be not-found. Neither the selected
unit name nor actual state was logged. No runtime test or real credential stage
was reached. Report construction followed the assertion, so no JSON artifact
was produced. Do not rewrite that failure as a successful rehearsal.

systemd255 source provides a strong explanation: slice_load calls
unit_load_fragment_and_dropin with fragment_required=false; that function turns
an otherwise stub unit into UNIT_LOADED even without a fragment. Therefore
fragmentless loaded slices are expected, and not-found is not a sound general
test for absence of an operator-configured slice. The old log still cannot
prove which name failed or the actual observed state.

Primary sources:
- https://raw.githubusercontent.com/systemd/systemd/v255/src/core/slice.c (slice_load)
- https://raw.githubusercontent.com/systemd/systemd/v255/src/core/unit.c (unit_load_fragment_and_dropin)

The reviewed ownership correction replaces only the incorrect slice LoadState
assumption. A slice may be implicitly loaded only with mandatory inactive/dead,
empty fragment/source/drop-ins/control-group and nontransient metadata. Services
still require not-found. Every manager-reported unit search path is checked for
an exact unit file, dangling symlink or exact drop-in directory. Queued jobs and
configured/active parent slices are rejected. Unknown metadata/parse output fails.
All isolation, capability, archive/unit hashes and limits remain unchanged.
It records fixed preflight fields
before assertions, selected service/slice metadata, and writes a receipt on
preflight failure. No cleanup/systemd mutation occurs if no unit/slice is owned;
owned cleanup remains unchanged. No secrets, broad environment, command lines,
unit contents or systemctl stderr are emitted.

Validation: seven mocked preflight/inert/owned-cleanup tests and six existing
cleanup fault-injection tests pass against the changed module. Default CLI inert.
Independent review found no blocking issue in the ownership change. Full mocked
preflight covers fragmentless parent/leaf slices, unrelated jobs and malformed
job output. Negative controls cover every metadata field, omitted fields, queued
jobs, dangling symlinks and drop-in directories. This is not Linux runtime proof.

## Exact rerun scope to be approved separately

Publish the reviewed ownership-correction commit to the existing rehearsal branch only
after checking its exact diff, then verify its remote head. Dispatch registered
build-linux.yml exactly once with ref codex/synthetic-broker-systemd-rehearsal-r3.
Do not use GitHub's rerun facility: run_attempt>1 is rejected. Eight-minute job,
same disposable ubuntu24.04 VM, same archive/unit/helper/adapter and boundaries;
no production connection, dependency installation, credentials or policy changes.
Monitor to terminal and retain the sanitized receipt. No merge or VPS action.

This is one proposed exact runtime rehearsal after the corrected freshness gate,
not a claim of success. Actual manager output shape and runtime isolation remain
unverified until that run. No second dispatch has been performed or is currently
authorized. Preserve the original failed run and diagnostic-only commit history.
