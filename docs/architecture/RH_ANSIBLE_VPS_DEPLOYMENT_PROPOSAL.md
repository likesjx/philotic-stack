# Red Hat Ansible And VPS Deployment Proposal

## Goal

Define the authority boundary between Philotic's hotel runtime and Red Hat Ansible as the external infrastructure orchestrator, with VPS deployment as the first concrete target.

## Core Recommendation

- Red Hat Ansible should own machine provisioning, package/runtime installation, service units, secrets/bootstrap placement, peer inventory, and rollout operations.
- Philotic `ansible` should own hotel runtime state, guest materialization, session/context authority, and inter-hotel runtime behavior once the process is running.
- Peer topology should be rendered explicitly for deployed hotels instead of inferred from local loopback assumptions used in development.

## Disposition

`accepted for current slice`

## Current Slice

This slice records the deployment boundary so infrastructure work does not leak into runtime architecture by accident.

What is accepted:
- one hotel per deployed machine is the default deployment model
- Red Hat Ansible is the outer control plane for VPS/bootstrap operations
- Philotic hotel config remains the inner runtime authority

What remains deferred:
- exact inventory schema for hotel peers and public/private addresses
- systemd/service packaging details
- secrets/bootstrap flow for VPS nodes
- watched live VPS deployment validation

## Proposed Ownership Split

### Red Hat Ansible owns

- host inventory
- Linux package/runtime prerequisites
- binary placement or build artifact deployment
- service lifecycle (`systemd`, restart, rollback)
- network/bootstrap setup such as Tailscale or WireGuard
- initial config and secret material placement
- peer address rendering for hotels

### Philotic owns

- context graph contents
- hotel identity and guest manifests
- guest materialization and supervision
- session state and routing
- mesh event dispatch/ack behavior
- blob/event/cursor persistence
- agent identity import and runtime config projection

## VPS Target

The first deployment target is a Linux VPS running one Philotic hotel with a materialized guest stack. Multi-hotel and mixed local/VPS deployments should build on the same contract.

## Active Work Links

- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
- [docs/implementation_plan.md](/Users/jaredlikes/code/philotic-stack/docs/implementation_plan.md)
