---
title: Owner Approval Hotel Verifier
doc_type: proposal
domain: operator-control-plane
status: accepted-current-slice
last_updated: 2026-11-14
tags:
- owner-approval
- hotel-verifier
- service-account
- test-only
related_docs:
- ARCHITECTURE_STATUS.md
- VPS_ACCOUNT_SEPARATION_PROPOSAL.md
- KEY_VAULT_PROPOSAL.md
- HOTEL_USER_IDENTITY_AND_OPERATOR_AUTH_PROPOSAL.md
- ../operations/VPS_ACCOUNT_CUTOVER_RUNBOOK.md
task_refs:
- docs/task.md
proposal_id: bill-owner-approval-hotel-verifier-20261114
active_seams:
- vault-secret-refs
- secret-handling-hardening
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Owner Approval Hotel Verifier

## Goal

One hotel-side approval check for security-sensitive hotel actions. The signer is the native Philotic app. This document records S0 only: the service user and the broker that owns approval state. S1 signature verification is not built here.

The graph recorded the domain as `operator-identity-approval`. That name is not in the repo vocabulary, so this file uses `operator-control-plane`.

## Core Recommendation

Approval state lives in `/var/lib/philotic-approval/approval.db`, owned by `philotic-approval` and mode `0700`. The hotel uid and agent accounts must not be able to write it. A socket-activated broker (`aiua approval-broker`) is the writer. It accepts a connection only when the kernel peer uid equals `PHILOTIC_APPROVAL_CALLER_UID`. The socket is mode `0600` and owned by the hotel user.

The broker stores a policy row, a pinned public key, a request, and a burn of `request_id` + `nonce` in one SQLite transaction. It does not check signatures, canonical bytes, or tiers. Unknown JSON fields, including a signature, are rejected. Root is not an allowed caller.

Guest spawn clears the environment, copies an allowlist, then applies that guest's own config map. `PHILOTIC_VAULT_MASTER_KEY` is removed from both. `PHILOTIC_MEMGRAPH_PASSWORD` is not inherited, but a guest config that already names it still receives it, because the hotel copies that value into specific guest configs today.

The hotel and ONNX units set `InaccessiblePaths=/var/lib/philotic-approval` and do not add that path to `ReadWritePaths`. `RuntimeDirectoryMode` stays `0700`. The live unit was observed at `0755`; the template was already `0700` and is not loosened. Reinstalling the unit is a live step.

`philotic_bin_owner_root` defaults off. Turning it on makes the legacy binary copy root-owned so the hotel user cannot replace `philotic-web`. Leaving it off keeps today's owner, so an ordinary hotel deploy does not chown binaries.

## Disposition

`accepted for current slice`

S0 is accepted with A1. S1 through S6 are not this slice. The graph's S1 verifier (ES256, canonical bytes, burn-on-use as the only `VerifiedApproval` constructor) is explicitly not implemented.

## Current Slice

Test-only broker, burn store, guest env scrub, and unit templates. The broker is not enabled on a host. In-hotel guests still share the hotel uid, so a guest running as `philotic` can open the broker socket. The filesystem still stops that guest from writing `approval.db` directly. Separating guest uids is later work.

The hotel writes a mode-0400 file for the `philotic-web` guest only while `PHILOTIC_WEB_SEPARATE_UNIT` is unset. Setting that variable (ansible flag `philotic_web_separate_unit`, default off) stops the handoff and stops spawning that guest. The web unit then reads its own `LoadCredential=` as user `philotic-web`. Do not enable `philotic_vault_key_via_load_credential` on the hotel unit if the goal is to keep the key out of the hotel mount namespace.

## Verification

`test-green` for peer-uid denial, env scrub, burn atomicity, and the hotel uid's write denial on a local fixture. Not smoke-green. No VPS cutover.

## Active Work Links

- [docs/task.md](../task.md)
- [VPS account separation](VPS_ACCOUNT_SEPARATION_PROPOSAL.md)
- [Cutover runbook](../operations/VPS_ACCOUNT_CUTOVER_RUNBOOK.md)
