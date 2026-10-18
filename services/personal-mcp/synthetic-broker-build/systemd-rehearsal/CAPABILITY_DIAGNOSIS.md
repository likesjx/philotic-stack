# Capability diagnostic candidate — not published or executed

Run39128526957 at commit1d34c95944b0ca3f478adf0fcd14b4082e860971 passed slice preflight and configured limits, then failed the validator's CapEff equality check. Cleanup passed. The failure did not record actual masks, so no runtime capability correction can yet be justified as confirmed.

## Leading hypothesis

The unit specifies User=root, an empty ambient set, and seccomp-producing protections. In systemd v255 exec-invoke.c, the keep_seccomp_privileges branch drops CAP_SETUID when absent from the ambient set; its restoration path restores the bounding set but explicitly handles SYS_ADMIN/SETPCAP. Combined with NoNewPrivileges, a missing permitted SETUID is a plausible explanation. This predicts effective/permitted0x200063 rather than expected0x2000e3, potentially with the bounding set unchanged. Runner distro patches and the actual masks remain unverified.

Primary source: https://raw.githubusercontent.com/systemd/systemd/v255/src/core/exec-invoke.c (keep_seccomp_privileges, enforce_user, saved_bset). Namespace --keep-caps concerns user namespaces; this bootstrap creates no user namespace, so adding that option is not an established fix: https://www.man7.org/linux/man-pages/man1/unshare.1.html.

## Minimal implemented correction

Correct the missing evidence and stale phase reporting first. Validator prints only a fixed phase, expected mask/names, five bounded64-bit capability masks/names, and boolean NoNewPrivs. It retains exact effective/permitted/bounding equality, empty inheritable/ambient requirements and NoNewPrivs. Unknown bits get numeric names and remain rejected. Malformed mask text is never echoed. No process environment, argv, credentials or memory records are emitted.

Runner labels staging/start/wait/receipt phases accurately and points runtime failure to the validator journal. Archive changes only validate.py and its manifest hash. Unit, bootstrap, helper, adapter, namespace restrictions, resource limits, workflow and cleanup remain unchanged. The change is diagnostic, not a claim that runtime qualification is fixed.

Do not add ambient capabilities, expand the bounding set, remove seccomp or accept subsets. If the predicted missing SETUID is established, a separately reviewed candidate could omit redundant User=root for a system-manager unit (default root), retaining Group=root and the validator's root guard, to avoid explicit UID setup. That behavior must be checked against the installed systemd revision and runtime evidence before acceptance; it is not included here.

## Verification and next-run bundle

Local fixtures:7 validator tests including missing/extra/unknown capabilities, malformed mask suppression, NoNewPrivs/ambient denial and SQLite ownership ordering;7 existing preflight tests;6 existing cleanup fault tests. These do not emulate systemd or establish Linux runtime success.

Proposed approval: publish this reviewed diagnostic candidate to the existing DO NOT MERGE PR672 branch and dispatch build-linux.yml exactly once after exact remote-head verification, on the same disposable ubuntu24.04 runner. Same120+15seconds,128MiB/no swap/25%CPU/32tasks service;640MiB/no swap/50%CPU/128tasks slice;8-minute job. Public dummy only. Preserve failure and cleanup receipt; no automatic retry, merge, VPS, package installation, security-policy relaxation or real credentials. No publication or third run has occurred.

Candidate archive SHA25645b79bf407a7353eadc670c1bd6996c3e18a11a829a3edba4b3233f88fa637f0.
Manifest SHA256516291589195f1451688b0f9ad9e07d0b8bdd08dbd27b650a637435ca6255587.
Validator SHA25690e7583b0f424065f196bf077e33f1e3596dfa2a0f071492e2d51fed32304b4c.

## Independent issuer/discovery work

Read-only source reconciliation and local fixture tests can proceed independently: identify which checked-in discovery route produces200/401 versus the observed403HTML, and verify issuer503 disabled/invalid-config branches with the LifeGraph owner. These need no credential grant or deployment. Existing source does not establish the live403 producer; live correlation needs narrowly scoped edge status/request-ID metadata, not a direct-backend bypass. Shared issuer edits remain with its owner. Activation, enabling issuer configuration, callback registration, admissions and credential provision require the separate current deployment/access bundle. No broad new implementation is necessary while the capability gate is unresolved.
