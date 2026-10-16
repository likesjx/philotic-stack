# Public dummy exact-systemd rehearsal

This branch prepares one manually approved rehearsal of the immutable public-dummy R3 bundle on a disposable GitHub-hosted ubuntu24.04 runner. It is not a deployment branch and must not be merged as a replacement for the normal Linux build workflow.

GitHub requires a manually dispatched workflow's filename to exist on the default branch. This branch therefore uses the already registered `.github/workflows/build-linux.yml` filename, replacing its branch-local contents with only the bounded rehearsal job. The default branch remains unchanged. The job requires this exact branch, workflow_dispatch and run_attempt1. No push/PR/schedule trigger is added.

After separate publication and execution approval, manually dispatch `build-linux.yml` with ref `codex/synthetic-broker-systemd-rehearsal-r3`, after verifying its head equals the reviewed commit. Do not dispatch the default ref. The original broker-build PR remains unmerged; this work can be reviewed in a separate draft PR against `codex/synthetic-broker-build-only`.

Publication discloses the rehearsal Python source and service/validator source embedded in the archive, the unchanged already-built helper/adapter, dummy-only passwd/group fixture, fixed public dummy key, synthetic paths/UID999/GID987, resource bounds, and nonsecret result metadata. It does not include private host addresses, device names, SSH material, real credentials, operator identity IDs, production health snapshots or prior execution receipts. There is no source rebuild or dependency download.

The workflow grants contents-read only and uses pinned checkout/upload actions. Checkout does not persist credentials. GitHub downloads the public repository and action implementations; only the nonsecret rehearsal JSON is uploaded, retained7days. No repository secret is referenced. The service sees no runner home, checkout or token; it has read-only host/usr for installed tools, private PID/mount/network namespaces, and AF_UNIX sockets only.

The runner uses sudo on the disposable VM to create a fresh synthetic root, temporary service and temporary resource slice, start once, inspect bounded results, and stop/remove the exact owned units. It checks systemd255/PID1, x86_64, dump policy2 and Yama1 without changing them. Missing prerequisites fail without installing packages. The unit's capabilities are exactly CHOWN, DAC_OVERRIDE, KILL, SETGID, SETUID and SYS_ADMIN; FOWNER and SYS_PTRACE remain absent. Mode is set before ownership transfer.

Bounds: service120s+15s stop,128MiB RAM/no swap/25%CPU/32tasks; parent slice640MiB/no swap/50%CPU/128tasks; entire job8minutes. Root owns bootstrap operations, then helper/probes run UID999/GID987. Only public dummy key/data enter the isolated database. No live endpoints or production credentials are used.

Cleanup attempts service and CI-owned slice stops independently of journal collection, verifies actual hierarchical cgroup descendant population and owned mounts, and removes only stopped owned unit files. Unknown evidence preserves files and fails cleanup. Runtime scaffolding is retained. The temporary VM's destruction removes retained dummy state. No recursive cleanup or retry is performed.

The runner must never be used on an existing production machine: its fresh-path and hosted-runner guards are accidental-execution checks, and its owned-slice teardown is specific to the disposable environment. Local/source review does not establish Linux runtime success. The unchanged helper's5second receive timeout includes probing under CPU throttling; all positive/negative controls and cleanup must pass in the actual rehearsal.

Archive SHA256: `15d427d6fff8c4b6ac615c98e4f2672eb455e5a0663d005a384a63096811f3a0`.
Helper SHA256: `6cbbda167477d5e4365cf36b14e6d2b45c41baa0c6032004901318c509022989`.
