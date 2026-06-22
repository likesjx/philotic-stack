---
name: mcp-surface-hygiene
description: Use this skill when reviewing, provisioning, or documenting MCP membrane tools for external clients such as Perplexity, Claude, or Codex. It keeps Muninn capture, LifeGraph tools, and raw graph/runtime tools separated.
catalog:
  skill_name: mcp.surface_hygiene
  implied_tools:
    - mcp.status
    - mcp.provision
    - mcp.revoke
  validation_state: proposed
  skill_markers:
    - governed
    - membrane
    - boundary_hygiene
  field_sources:
    required_fields:
      - endpoint_id
      - intended_client
      - advertised_tools
    optional_fields:
      - exposure
      - preapproval_rules
      - owner_agent
    repo_skill_path: skills/mcp-surface-hygiene/SKILL.md
    workflow: "mcp.status -> surface audit -> mcp.provision/mcp.revoke if needed -> smoke external call"
---

# MCP Surface Hygiene

Use this skill before exposing, changing, or documenting an MCP membrane endpoint.

## Purpose

The MCP membrane is an external protocol boundary. Its job is to expose a small, deliberate tool surface to external clients without making the philote cognitive loop, raw graph operations, or LifeGraph truth writes the default hot path.

## Ownership Split

- `context.capture` is a Perplexity-to-Muninn continuity route.
- `life.recall` retrieves governed LifeGraph context packets.
- `life.observe` proposes LifeGraph evidence; it does not confirm durable truth by itself.
- `life.commit`, `life.resolve`, and raw graph writes require explicit governance or operator approval.
- `mcp.provision` configures the membrane; it is not the backend for the advertised tools.

## Surface Rules

Keep each MCP endpoint boring and explicit:

1. Name tools for the storage surface they touch.
2. State whether the tool writes to Muninn, proposes LifeGraph evidence, or only reads.
3. Do not advertise `context.capture` as a general knowledge-system write. It writes Muninn continuity memory.
4. Do not route `context.capture` into LifeGraph.
5. Do not expose raw graph/database mutation tools to external clients unless the endpoint is explicitly an admin endpoint with operator-approved preapproval rules.
6. Prefer separate endpoints for different authority levels, such as `perplexity-memory` and `lifegraph-readonly`.

## Recommended External Client Surface

For Perplexity:

- `context.capture`: write to Muninn continuity memory.
- Optional `life.recall`: read-only LifeGraph context, if the endpoint and bearer scope explicitly allow it.
- Optional `life.patch.propose`: propose governed improvements, not apply them.

For Claude or Codex outside the hotel runtime:

- Prefer `life.recall` and `life.observe` through a governed endpoint.
- Keep `life.commit` and `life.resolve` unavailable or approval-gated.
- Use native projected tools when already running inside Philotic.

## Review Checklist

Before calling an MCP surface clean, verify:

- `tools/list` descriptions name the destination surface.
- bearer scopes match the advertised authority.
- preapproval rules are narrower than the tool list.
- each write path has a validation state in the description.
- the endpoint exposure tier matches the actual network path.
- one external smoke call proves the route lands on the intended backend.

## Anti-Patterns

- A tool called `save_memory` that might write Muninn, LifeGraph, Obsidian, or raw graph depending on mood.
- Routing external MCP calls through a model turn when a deterministic runner owns the operation.
- Letting `life.observe` sound like confirmed truth.
- Reusing a Perplexity bearer token for higher-agency LifeGraph or admin tools.
