# MCP Credential Lifecycle

Use this runbook when provisioning, rotating, validating, or revoking credentials for Philotic MCP clients.

This document covers the three current client paths:

- Codex and Claude trusted local clients using native Muninn MCP through `muninn mcp`
- Perplexity and other external capture clients using the Philotic HTTPS frontdoor
- governed LifeGraph clients using a separate LifeGraph MCP endpoint

Philotes provisioning their own endpoints follow the same rules through the
`mcp.endpoint_steward` skill ([skills/mcp-endpoint-steward/SKILL.md](/Users/jaredlikes/code/philotic-stack/skills/mcp-endpoint-steward/SKILL.md)):
`mcp.status` → design the surface and per-tool handler policies →
`mcp.provision` → `mcp.grant_token` → smoke → record. The raw token is
returned once in the `mcp.grant_token` result and must be relayed to the
operator's secret store, never stored by the agent.

## Default Posture

Native Muninn MCP is a private trusted-client surface.

Allowed:

- loopback MCP on the same machine
- SSH tunnel to a remote loopback listener
- short-lived scoped API keys for REST/API clients
- HTTPS Philotic MCP endpoints with explicit bearer grants

Not allowed:

- public native Muninn MCP listener
- broad reusable read/write bearer grants
- using a `context.capture` bearer as a LifeGraph or native Muninn credential
- committing raw tokens, hashes, vault refs paired with raw tokens, or copied terminal output that contains tokens

## Credential Classes

| Class | Surface | Scope | Default Expiry | Storage |
| --- | --- | --- | --- | --- |
| `local-native-muninn` | `muninn mcp` over loopback | native Muninn tools for trusted local client | session/local daemon lifetime | local Muninn token file or OS secret store |
| `remote-native-muninn-tunnel` | SSH tunnel to remote loopback | native Muninn tools for trusted operator session | session only | SSH agent/keychain; no MCP bearer in repo |
| `external-context-capture` | Philotic HTTPS MCP frontdoor | `context.write` only | 30-90 days | hotel vault hash + operator secret store |
| `lifegraph-readonly` | Philotic HTTPS LifeGraph MCP endpoint | `life.recall` | 30-90 days | hotel vault hash + operator secret store |
| `lifegraph-observe` | Philotic HTTPS LifeGraph MCP endpoint | `life.observe` proposed evidence only | 7-30 days | hotel vault hash + operator secret store |
| `remote-agent-frontdoor` | Philotic HTTPS agent frontdoor (`/agent/mcp`) | one bearer per cloud agent (`claude-cloud`, `codex-cloud`); `muninn_*` continuity (4) + intel-graph coordination (12) | 30 days | hotel vault BLAKE3 hash + cloud environment secret |
| `operator-admin` | native/API maintenance | full, manual maintenance only | shortest practical | operator secret store only |

## Provisioning Rules

Before creating a credential, record the planned grant:

- client name
- surface
- scope
- expiry
- owner
- storage location for the raw secret
- revocation command or path
- UAT command that proves the grant works and does not exceed scope

Only record labels, IDs, expiries, and vault refs in docs. Never record raw secret material.

### Perplexity Capture

Provision with [provision-mcp-bearer.py](/Users/jaredlikes/code/philotic-stack/scripts/provision-mcp-bearer.py).

Required outcome:

- `tools/list` exposes `context.capture`
- `context.capture` writes Muninn continuity memory
- no LifeGraph tools are exposed through this bearer
- no native Muninn tools are exposed through this bearer

### LifeGraph Readonly

Provision with [provision-lifegraph-mcp.py](/Users/jaredlikes/code/philotic-stack/scripts/provision-lifegraph-mcp.py).

Required outcome:

- `tools/list` exposes `life.recall`
- `life.observe` is absent unless `INCLUDE_LIFE_OBSERVE=1` was intentionally set
- `life.commit` and `life.resolve` are absent for external clients
- returned packets include provenance and authority labels

### Agent Frontdoor

Provision with [provision-agent-frontdoor.py](../../scripts/provision-agent-frontdoor.py)
on vps-jane, after [register-frontdoor-upstreams.py](../../scripts/register-frontdoor-upstreams.py)
registered `muninn-cortex` (vps-jane) and `intel-graph` (mac-jane) with
`grant_agents` listing each `mcp:<agent>`.

Required outcome:

- `tools/list` exposes exactly the 16 continuity/coordination tools; no `life.*`, no `context.capture`
- each agent has its own token; revoking one agent is one grant removal on the endpoint or on an upstream
- raw tokens exist only in the cloud environment's secret settings, never in chat, commits, or files left on the box
- `graph_*` calls fail fast (not hang) while mac-jane is asleep; `muninn_*` keep working
- `scripts/mcp-client-uat.sh agent-frontdoor` passes

### Native Muninn

Use [MUNINN_DIRECT_CLIENT_ACCESS.md](/Users/jaredlikes/code/philotic-stack/docs/reference/MUNINN_DIRECT_CLIENT_ACCESS.md).

Required outcome:

- local or tunneled MCP health succeeds
- remote native Muninn listener remains loopback-only
- no public raw `8750` listener exists
- session tunnel is torn down after use

## Rotation

Rotate credentials when:

- a client changes owner or machine
- a token may have been exposed
- the expiry window is reached
- a client scope changes
- a provisioning script or route definition changes

Rotation steps:

1. create the new secret in the operator secret store
2. provision the route with the new token hash
3. run client UAT with the new token
4. revoke or disable the old token/route
5. run deny-path UAT with the old token
6. update operator-only credential inventory with ID, label, scope, expiry, and revocation evidence

## Revocation

Revocation is part of normal credential lifecycle, not just incident response.

Minimum revocation evidence:

- old credential fails `tools/list` or the scoped operation
- new credential, if any, still passes
- raw secret is removed from the client
- docs do not contain the raw secret

## UAT Matrix

Run [mcp-client-uat.sh](/Users/jaredlikes/code/philotic-stack/scripts/mcp-client-uat.sh) for the safe local checks and any live checks for which you provide tokens through environment variables or readable token files. The `all` mode skips token-backed checks when tokens are absent; the `live` mode is strict and fails unless both live bearer tokens are supplied.

| Client | Required UAT | Live Secret Needed |
| --- | --- | --- |
| Codex | `.mcp.json` contains `muninn-local`; local Muninn health passes | no |
| Claude | trusted local config/harness exposes Muninn tools or uses stdio proxy shape | no, unless testing app config |
| Perplexity | `tools/list` shows only `context.capture`; call writes Muninn memory | yes |
| LifeGraph readonly | `tools/list` shows `life.recall` and not write tools | yes |
| Agent frontdoor | `mcp-client-uat.sh agent-frontdoor`: 16 tools, recall + graph_status round trips, revoked token refused | yes |
| Remote native Muninn | loopback-only binding and SSH tunnel health pass | SSH access |

Useful commands:

```bash
phil mcp uat all
phil mcp uat live \
  --perplexity-token-file /path/to/perplexity.token \
  --lifegraph-token-file /path/to/lifegraph.token

# Lower-level script/just wrappers are still available.
just mcp-client-uat
just mcp-client-uat all
just mcp-client-uat remote-native
PERPLEXITY_MCP_TOKEN=... just mcp-client-uat perplexity-capture
LIFEGRAPH_MCP_TOKEN=... just mcp-client-uat lifegraph-recall
PERPLEXITY_MCP_TOKEN=... LIFEGRAPH_MCP_TOKEN=... just mcp-client-uat live

# Prefer token files for large bearers or shell history hygiene.
PERPLEXITY_MCP_TOKEN_FILE=/path/to/perplexity.token \
LIFEGRAPH_MCP_TOKEN_FILE=/path/to/lifegraph.token \
  just mcp-client-uat live
```

The live modes read bearer tokens only from environment variables or explicit token files and do not print token values.

## Completion Gate

Do not call a credential slice complete unless:

- safe local UAT passes
- any newly provisioned live credential has positive-path and deny-path evidence, including `context.capture` or `life.recall` calls where applicable
- no raw secrets appear in `git diff`
- `docs/task.md` and the relevant proposal record which gates are proven and which remain operator-gated
