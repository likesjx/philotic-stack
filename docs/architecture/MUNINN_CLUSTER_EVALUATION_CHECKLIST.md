---
title: Muninn Cluster Evaluation Checklist
doc_type: seam
domain: memory-context
status: proposed
last_updated: 2026-06-30
tags:
  - muninn
  - cluster
  - memory
  - failover
  - validation
related_docs:
  - MUNINN_V07_CAPABILITY_ADOPTION_PROPOSAL.md
  - MUNINN_MEMORY_PROTOCOL_PROPOSAL.md
  - ARCHITECTURE_STATUS.md
task_refs:
  - docs/task.md#muninn-v07-capability-adoption
proposal_id: muninn-cluster-evaluation
implements:
  - muninn-v07-capability-adoption
implemented_by: []
active_seams:
  - muninn-hotel-cluster-authority
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# Muninn Cluster Evaluation Checklist

## Purpose

Evaluate Muninn cluster mode across `local-bjork`, `mbp-jane`, and `vps-jane` without accidentally promoting cluster mode into the production continuity architecture.

This checklist is intentionally conservative. Muninn v0.7 makes clustering plausible; it does not answer which hotel owns memory truth.

## Non-Negotiables

- Do not enable cluster mode for real continuity vaults during the first evaluation.
- Do not replicate raw API keys, MCP tokens, or operator secrets as test data.
- Do not expose Muninn ports publicly.
- Do not claim production readiness from source-only or single-node validation.
- Stop if leadership, replication, or recovery evidence is ambiguous.

## Preflight

- [ ] Confirm all three hosts run the same Muninn version.
  - [ ] `local-bjork`
  - [ ] `mbp-jane`
  - [ ] `vps-jane`
- [ ] Run non-mutating lab preflight:
  - [x] `just muninn-cluster-preflight`
  - [x] `RUN_REMOTE=1 just muninn-cluster-preflight all`
  - [x] disposable local alternate-port daemon probe
  - [x] disposable cluster enablement probe reached the admin-auth gate without touching real data
- [ ] Confirm current standalone services are healthy before cluster changes.
- [ ] Back up `auth_secret`, `mcp.token` if present, and any cluster config.
- [ ] Record listener bindings and firewall state.
- [ ] Create an isolated test data directory or disposable test vaults.
- [ ] Confirm the evaluation can be rolled back without touching real continuity data.

## Test Data

Use non-secret synthetic memories only.

Recommended seed shape:

- `cluster-smoke: local-bjork seed`
- `cluster-smoke: mbp-jane seed`
- `cluster-smoke: vps-jane seed`
- tags: `cluster-smoke`, `non-secret`, `uat`

Validation requires reading the same IDs from the expected nodes after replication.

## Network And Identity

- [ ] Use Tailscale or another private overlay for all cluster traffic.
- [ ] Record each node ID and peer address.
- [ ] Verify cluster transport authentication is configured.
- [ ] Confirm no cluster listener binds to public interfaces.
- [ ] Confirm `vps-jane` firewall continues to block unintended public access.

## Cluster Bring-Up

Do not run this section against real continuity data until the isolated-data or
test-vault plan is explicit. The local lab can start disposable Muninn daemons
on alternate ports with isolated `/tmp` data, but production cluster enablement
still needs an authenticated admin ceremony.

Preflight evidence recorded 2026-06-30: local, `mbp-jane`, and `vps-jane`
reported Muninn cluster CLI support and healthy standalone daemons. Listener
checks confirmed MCP was not public-bound on the remote hosts.

Isolation evidence recorded 2026-06-30: `muninn start` forwards hidden
`--rest-addr`, `--ui-addr`, `--mcp-addr`, `--mbp-addr`, and `--grpc-addr`
daemon flags. The Philotic preflight starts a disposable local daemon with
isolated `/tmp` data and verifies MCP health on the alternate MCP port before
cleanup.

Auth blocker recorded 2026-06-30: `muninn cluster enable` reaches
`/api/admin/cluster/enable`, but the current CLI path does not attach an admin
session cookie to that request. The disposable probe therefore treats HTTP 401
`admin session required` as the expected gate. The next lab slice needs either
an upstream Muninn CLI fix that authenticates cluster admin requests or an
explicit direct REST ceremony that obtains `muninn_session` from the UI login
endpoint without storing secrets.

- [ ] Enable cluster mode on the isolated data set.
- [ ] Add nodes one at a time.
- [ ] Record initial role for each node.
- [ ] Verify `muninn cluster info`.
- [ ] Verify `muninn cluster status --json`.
- [ ] Confirm the cluster converges to one leader.

## Replication Smoke

- [ ] Write one synthetic memory on the leader.
- [ ] Read it from each follower.
- [ ] Write one synthetic memory on a follower if the client path allows it.
- [ ] Confirm write routing behavior is explicit: accepted, forwarded, or rejected.
- [ ] Confirm no duplicate memories are created during replication.

## Failover Smoke

- [ ] Stop the leader.
- [ ] Observe follower election.
- [ ] Confirm exactly one new leader.
- [ ] Read pre-failover synthetic memories from the new leader.
- [ ] Write one post-failover synthetic memory.
- [ ] Restart the original leader.
- [ ] Confirm the returning primary defers to the elected leader.
- [ ] Confirm the returning node receives the missing post-failover memory.

## Quorum And Partition Smoke

- [ ] Simulate one-node loss.
- [ ] Confirm quorum behavior matches expectations.
- [ ] Simulate leader losing quorum.
- [ ] Confirm self-demotion or write rejection instead of split-brain.
- [ ] Restore quorum and confirm convergence.

## Secret Boundary Smoke

- [ ] Create a temporary observe API key only in the isolated test scope.
- [ ] Confirm the key does not appear on hosts where it should not exist.
- [ ] Confirm real `mcp.token` files are unchanged.
- [ ] Confirm real `auth_secret` files are unchanged.
- [ ] Revoke the temporary key and verify failure.

## Rollback

- [ ] Disable cluster mode.
- [ ] Stop test daemons.
- [ ] Archive logs and cluster status output.
- [ ] Remove isolated test data or disposable test vaults.
- [ ] Restore standalone Muninn status on all three hotels.
- [ ] Run local MCP `tools/list` on all three hotels.

## Decision Gate

Before production cluster adoption, record a decision that answers:

- Which Muninn node or cluster owns canonical continuity memory?
- Which vaults, if any, may replicate?
- Which clients may write to replicated memory?
- How are API keys and MCP tokens scoped, rotated, and revoked?
- How does this relate to LifeGraph truth and intel-graph source-of-truth records?

If those questions are not answered, cluster mode remains a lab capability.
