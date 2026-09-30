# vps-jane edge (mcp.jaredlikes.com)

Reference copies of the out-of-band edge config for the public MCP host. These
were previously only on the box; keep them in sync when either side changes.

| File | Live location | Apply |
|---|---|---|
| `traefik-mcp-membrane.yml` | `/home/deploy/traefik/dynamic/mcp-membrane.yml` | copy; Traefik's file provider reloads it |
| `iptables-mcp.rules` | `/etc/iptables/rules.v4` (INPUT) | edit rules, `sudo iptables-restore < /etc/iptables/rules.v4` |

Routes on `https://mcp.jaredlikes.com`:

| Path | Backend | Listener |
|---|---|---|
| `/mcp` | `host.docker.internal:9100` via `mcp-frontdoor-bridge.service` (socat) | membrane-mcp general listener (loopback) |
| `/lifegraph/mcp` | `:8911/mcp` | `lifegraph-readonly` endpoint |
| `/agent/mcp` | `:8912/mcp` | `agent-frontdoor` endpoint ([proposal](../../../docs/architecture/AGENT_FRONTDOOR_PROPOSAL.md)) |

Loopback trust: the `/mcp` route arrives at membrane-mcp from `127.0.0.1`
(socat). membrane-mcp treats any request carrying `X-Forwarded-For`,
`Forwarded` or `X-Real-IP` as remote, so Traefik's forwarding headers keep
public callers out of the loopback-only paths (ingress fence bypass and
`McpAuthScheme::None` tools). Do not enable `forwardedHeaders.insecure` or
strip these headers in Traefik.
