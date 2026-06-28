# Muninn Direct Client Access

Use this reference when Codex, Claude, or another trusted local client needs direct native Muninn MCP access.

Public and semi-public clients should keep using the Philotic MCP frontdoor unless there is a specific reason to grant native Muninn access.

## Default Decision

Do not expose the native Muninn MCP listener on `vps-jane` to the public internet.

Allowed direct paths:

- local loopback: `http://127.0.0.1:8750/mcp`
- private overlay or SSH tunnel to a loopback listener
- short-lived, scoped API keys only when a client needs REST/API access rather than the MCP session path

Disallowed direct paths:

- public `:8750` listener on `vps-jane`
- reusable public bearer grants for broad Muninn read/write access
- using Perplexity's `context.capture` bearer as a general Muninn or LifeGraph credential

The public shape remains:

- Perplexity and other external capture clients -> Philotic MCP HTTPS frontdoor -> `context.capture` -> Muninn continuity memory
- governed LifeGraph clients -> Philotic MCP HTTPS frontdoor -> `life.recall` / approval-gated `life.observe`
- trusted local clients -> native Muninn MCP over loopback or private tunnel

## Codex Local MCP

The repository `.mcp.json` includes a private local MCP server:

```json
{
  "muninn-local": {
    "type": "stdio",
    "command": "muninn",
    "args": ["mcp"],
    "env": {
      "MUNINN_MCP_URL": "http://127.0.0.1:8750/mcp"
    }
  }
}
```

`muninn mcp` is a stdio-to-HTTP proxy. It keeps the client config local while the Muninn daemon keeps its MCP listener on loopback.

## Claude Desktop / Claude Code Shape

Use the same stdio proxy shape for trusted local Claude clients:

```json
{
  "mcpServers": {
    "muninn-local": {
      "command": "muninn",
      "args": ["mcp"],
      "env": {
        "MUNINN_MCP_URL": "http://127.0.0.1:8750/mcp"
      }
    }
  }
}
```

Do not put raw tokens in this file. If a remote URL ever requires a bearer token, store it in the local secret manager or client-specific secret store.

## Private vps-jane Tunnel

Current standard: trusted remote native Muninn access uses an SSH tunnel to the remote loopback listener.

This is the only approved remote-native path for now. Tailscale-only routing or private HTTPS ingress can be reconsidered later, but only after bearer/key scope, rotation, and revocation are documented for that path.

For trusted access to `vps-jane` native Muninn, forward the remote loopback listener to a local high port:

```bash
ssh -N -L 18750:127.0.0.1:8750 vps-jane
```

Then point the stdio proxy at the tunnel:

```bash
MUNINN_MCP_URL=http://127.0.0.1:18750/mcp muninn mcp
```

Or smoke the MCP handshake through the shared helper:

```bash
python3 scripts/muninn_mcp.py --base-url http://127.0.0.1:18750/mcp health
```

Close the tunnel when the session ends.

The repo smoke script verifies the full private path:

```bash
just muninn-private-smoke
```

It checks local Muninn MCP health, verifies `vps-jane` is not publicly bound on native Muninn MCP, opens the SSH tunnel, calls MCP health through the tunnel, and tears the tunnel down.

## API Keys

Use Muninn API keys for REST/API clients, not as the default MCP path.

Rules:

- prefer `observe` keys for retrieval-only clients
- use short expiries
- label keys with the client and purpose
- store raw key material only in the operator's secret store
- document key IDs, labels, mode, and expiry without committing tokens
- revoke keys as part of client offboarding

## Client Memory Habit

Trusted clients should follow [MUNINN_CLIENT_MEMORY_PROTOCOL.md](/Users/jaredlikes/code/philotic-stack/docs/reference/MUNINN_CLIENT_MEMORY_PROTOCOL.md):

1. bootstrap Muninn before meaningful work
2. recall the triad: self, user, topic
3. write back only compact durable deltas

Muninn is continuity memory. LifeGraph is structured life truth and evidence. Direct Muninn access should not become a bypass around LifeGraph governance.
