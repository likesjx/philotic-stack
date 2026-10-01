# Handoff — 2026-10-01 — Agent Frontdoor (back to cloud sessions)

> Written by the mac-jane Claude Code session that executed
> [the 2026-09-30 handoff](HANDOFF-2026-09-30-agent-frontdoor-mac-jane.md).
> Proposal: [AGENT_FRONTDOOR_PROPOSAL.md](architecture/AGENT_FRONTDOOR_PROPOSAL.md).
> Audience: a cloud Claude Code / Codex session with the frontdoor configured.

## What is live

`https://mcp.jaredlikes.com/agent/mcp` — vps-jane membrane-mcp endpoint
`agent-frontdoor` (:8912, behind Traefik), one bearer per agent
(`claude-cloud`, `codex-cloud`; expire ~2026-10-30).

| Tools | Served by |
|---|---|
| `muninn_where_left_off`, `muninn_recall`, `muninn_remember`, `muninn_decide` | `muninn-cortex` upstream on **vps-jane** → the Muninn Cortex, `default` vault (writes replicate) |
| `graph_status`, `graph_digest`, `graph_next_task`, `graph_context_for`, `graph_impact`, `graph_search`, `graph_agent_dashboard`, `session_start`, `session_activity`, `session_close`, `graph_decide`, `graph_record_test_run` | `intel-graph` upstream on **mac-jane** (via `McpUpstream.target_node`) |

Public UAT (2026-09-30): exactly 16 tools; `muninn_recall` and `graph_status`
round trips; no token → 401; unknown token → refused.

## How a cloud session should start

1. Environment must have `PHILOTIC_FRONTDOOR_URL`,
   `PHILOTIC_AGENT_MCP_TOKEN`, and `mcp.jaredlikes.com` (plus
   `static.crates.io` for cargo) in the network allowlist.
2. `python3 scripts/muninn_mcp.py bootstrap` → expect `status: ready`,
   `transport: frontdoor`.
3. Use the `philotic` MCP server from `.mcp.json` (PR #605) or call through
   `scripts/muninn_mcp.py`. Skip `just session-start`, `idea-sweep`,
   `harness-drift` (local-only).
4. Self-check: `AGENT_MCP_TOKEN=$PHILOTIC_AGENT_MCP_TOKEN bash scripts/mcp-client-uat.sh agent-frontdoor`.

## What changed from the 2026-09-30 plan (reality gaps)

- **Muninn moved to vps-jane.** The Mac Muninn is a cluster observer and
  rejects writes (`-32002`); the frontdoor uses the Cortex directly.
  `muninn-local` in the earlier handoff is superseded by `muninn-cortex`.
- `register_guest` is not an IPC op (`register`), and the hotel pushes a frame
  right after connect — both frontdoor scripts were fixed (DEF-209 tracks two
  older scripts).
- Hardening landed: proxied requests are never trusted as loopback
  (`is_trusted_loopback`, #597); upstream `CallToolResult` is no longer
  double-wrapped (#601); endpoint config/grant/revoke pushes now actually
  reach the running endpoint guest (DEF-212, #601); a refused dispatch to a
  sleeping Mac fails fast (DEF-211 partial, #601).
- Edge config is now in the repo: `ansible/files/vps-jane-edge/`.

## Known limits / open items

- **DEF-211 (rest):** in the first ~15 s after mac-jane goes offline,
  `graph_*` calls still time out at 30 s and may execute late when the Mac
  returns. Treat a `graph_*` timeout as "unknown outcome"; do not blindly retry
  `session_start` / `graph_decide`. Needs a task expiry.
- **Caller tag on writes:** not injected server-side. Tag your Muninn writes
  with `claude` or `codex`.
- **Local Codex on the Mac** cannot write Muninn (its config targets the Mac
  observer). Operator follow-up: Cortex key + tailnet URL.
- **OAuth connectors** (claude.ai / ChatGPT custom connectors) are not
  supported — bearer header only.
- Rotation: rerunning `scripts/provision-agent-frontdoor.py` rotates every
  agent's token; tokens live only in the operator Keychain
  (`philotic-agent-frontdoor`) and the cloud environment settings.

## Good next slices for a cloud session

- DEF-211 remainder: an `expires_at` on frontdoor-originated tool tasks so a
  late delivery is dropped, plus a test.
- DEF-209: port the `register` op + read-until-reply fix to
  `scripts/provision-lifegraph-mcp.py` and `scripts/provision-mcp-bearer.py`
  (owner-prefixed guest ids).
- Server-side caller tag injection for `muninn_remember` / `muninn_decide`
  in `mcp-client-runner` (proposal S4.3).

Anything needing the tailnet, a hotel socket, or a deploy goes back to a Mac
session.
