# Handoff — 2026-09-30 — Agent Frontdoor (for Claude Code on mac-jane)

> Written by a **cloud** Claude Code session that cannot reach the tailnet,
> Muninn, intel-graph, or even `static.crates.io`. Everything below needs the
> Mac (or vps-jane via the Mac). Proposal:
> [AGENT_FRONTDOOR_PROPOSAL.md](architecture/AGENT_FRONTDOOR_PROPOSAL.md).
> Branch: `claude/stoic-goldberg-f8w99b`.

> **Correction (2026-09-30, after reading `origin/develop`):** this branch was cut
> from a stale `main`. On develop, Muninn runs as a cluster: the **writable Cortex
> is on vps-jane**; mac-jane and mbp-jane run read-only observers that reject
> writes with HTTP 421 (`MUNINN_MEMORY_CORE_PROPOSAL.md`). So the `muninn-local`
> upstream should be registered on **vps-jane itself** (same hotel as the
> frontdoor, no mesh hop); only `intel-graph` stays on mac-jane. Also verify
> whether local Claude Code / Codex `muninn_remember` writes against the Mac
> observer still land. Rebase this branch onto `origin/develop` before any PR.

## Goal

Let cloud agents use the same Muninn continuity and intel-graph coordination
tools, by the same names, that local Claude and Codex use, through
`https://mcp.jaredlikes.com/agent/mcp` with a per-agent bearer. Cloud agents
here means Claude Code web, Codex cloud, or any MCP client that can send a
bearer header. Native Muninn and intel-graph stay on mac-jane loopback.

## What's already on the branch (unverified by compile — see step 1)

| Change | Files |
|---|---|
| New route target `McpUpstream { upstream_id, target_node }` and `mcp_caller_principal()` | `crates/ansible-mesh-core/src/mcp_route.rs` |
| Endpoint-config dispatch carries `target_node` (cross-hotel), for upstream targets only | `crates/membrane-mcp/src/transform.rs`, `server.rs` |
| Legacy route path rejects `McpUpstream` after auth | `crates/membrane-mcp/src/server.rs` |
| `mcp_upstream` target → role `mcp-client-runner`, `tool_name = mcp:<upstream>.<action>`, `agent_id = mcp:<token_id>` (anti-spoof re-stamp) | `crates/membrane/src/runtime.rs` |
| Frontdoor mode for the session gate (`PHILOTIC_FRONTDOOR_URL` + `PHILOTIC_AGENT_MCP_TOKEN`); verified against a mock server | `scripts/muninn_mcp.py` |
| Endpoint provisioner for the frontdoor hotel (mints per-agent tokens, writes them to 0600 files, pre-approves tools) | `scripts/provision-agent-frontdoor.py` |
| Upstream registration for mac-jane (`muninn-local` over stdio, `intel-graph` over HTTP, `grant_agents = mcp:<agent>`) | `scripts/register-frontdoor-upstreams.py` |

## Do this, in order

### 0. Session bootstrap as usual

Run `just session-start` and recall the triad. Then claim the work:
`graph_session_start` on proposal `agent-frontdoor`.

### 1. Compile and test the S1 slice (this has NEVER compiled)

```bash
git fetch origin claude/stoic-goldberg-f8w99b && git switch claude/stoic-goldberg-f8w99b
cargo test -p ansible-mesh-core -p membrane-mcp -p membrane -p membrane-mcp-client
just check
```

New tests to look for:
- `mcp_upstream_target_wire_shape`
- `route_target_variants`
- `mcp_upstream_target_carries_node`
- `mcp_upstream_target_emits_projected_call_to_pinned_node`
- `mcp_upstream_without_node_stays_local`

If anything fails, fix it on this branch. The design intent is in the proposal
(§S1). Also run `cargo fmt --all -- --check`. The files were formatted with
rustfmt standalone, not `cargo fmt`.

Then open a PR `claude/stoic-goldberg-f8w99b` → `develop`. Follow the repo's
worktree/PR conventions; `docs/task.md` needs a line for proposal
`agent-frontdoor`.

### 2. Confirm facts the cloud session assumed

Record any mismatch as a reality gap in Muninn and fix the scripts/proposal.

- [ ] The Mac hotel's node id is `mac-jane-aiua-01`. The provisioner's
      `UPSTREAM_NODE` defaults to it.
- [ ] Which philote agent id on mac-jane should own the upstreams? This becomes
      `OWNER_AGENT_ID` for `register-frontdoor-upstreams.py`.
- [ ] Is `mcp-client-runner` (`membrane-mcp-client`) running on mac-jane?
      Check `phil mcp upstreams`; the hotel spawns it on first registration.
- [ ] Does `muninn mcp` work with a **scrubbed env**? `membrane-mcp-client`
      clears the env except for a baseline plus `MCP_STDIO_ENV_PASSTHROUGH`.
      Does it default to `127.0.0.1:8750`, and does it need
      `~/.muninn/mcp.token` (so `HOME`) or `MUNINN_MCP_URL`? If so, set
      `MCP_STDIO_ENV_PASSTHROUGH=MUNINN_MCP_URL,…` for the guest.
- [ ] Does native Muninn MCP (`:8750`) write to the `default` vault when no
      vault argument is passed? That is what local Claude and Codex rely on.
- [ ] Is the intel-graph on mac-jane token-protected (`PHILOTIC_GRAPH_TOKEN`)?
      If so, register the upstream credential with the IPC op
      `provision_mcp_upstream_credential` (see `phil mcp …` / `mcp.set_credential`).

### 3. Register the upstreams on mac-jane

This needs the S1 hotel binary, because the `McpUpstreamConfig` shape is
unchanged but it is the first real use. Rebuild and restart the mac-jane hotel
from the branch first.

```bash
phil mcp allow-command --command "$(command -v muninn)" --args-prefix mcp   # operator ceremony
DRY_RUN=1 OWNER_AGENT_ID=<agent> AGENTS=claude-cloud,codex-cloud python3 scripts/register-frontdoor-upstreams.py
OWNER_AGENT_ID=<agent> AGENTS=claude-cloud,codex-cloud python3 scripts/register-frontdoor-upstreams.py
phil mcp upstreams      # both connected; allowlists = 4 muninn_* + 12 graph/session tools
```

Local smoke, before any public exposure: from a mac-jane philote or a small IPC
driver, call `mcp:muninn-local.muninn_where_left_off` with
`agent_id = mcp:claude-cloud`. Expect a result. Then call it with
`agent_id = mcp:not-granted`. Expect "not granted".

### 4. Deploy S1 to vps-jane and provision the endpoint

vps-jane needs the new `membrane-mcp`, `membrane` and `aiua` binaries. The hotel
deserializes the new `McpRouteTarget` variant when the endpoint config is stored.

```bash
just vps-deploy-ci          # or the current vps deploy path, from the merged develop
# on vps-jane:
DRY_RUN=1 AGENTS=claude-cloud,codex-cloud python3 scripts/provision-agent-frontdoor.py
PHILOTIC_HOTEL_SOCKET=/run/philotic/vps-jane.sock AGENTS=claude-cloud,codex-cloud \
  python3 scripts/provision-agent-frontdoor.py
```

- Tokens land in `~/.philotic/agent-frontdoor-tokens/<agent>.token` (0600).
  Move them into the operator secret store and delete the files. Never paste a
  token into a chat or a commit.
- Loopback smoke on vps-jane:

  ```bash
  PHILOTIC_FRONTDOOR_URL=http://127.0.0.1:8912/mcp \
  PHILOTIC_AGENT_MCP_TOKEN_FILE=~/.philotic/agent-frontdoor-tokens/claude-cloud.token \
  python3 scripts/muninn_mcp.py bootstrap
  ```

  Expect exit 0 and `transport: frontdoor`.
- Then run `python3 scripts/muninn_mcp.py where-left-off` the same way. Check
  that the result is **not double-wrapped** (proposal §S4.4).

### 5. Hardening gate (proposal §S4) — before public exposure

1. **Loopback trust behind the TLS proxy.** Find out how
   `mcp.jaredlikes.com` is terminated today; it isn't in the repo. If the proxy
   connects over `127.0.0.1`, make `membrane-mcp` treat requests carrying
   `X-Forwarded-For`/`Forwarded` as non-loopback (`server.rs`,
   `is_loopback` computation), with a test. **Blocker** for widening the
   frontdoor.
2. **Mac asleep.** Measure what a cloud agent sees when mac-jane is offline.
   If it's a 30 s hang, file the fast-fail as a follow-up.
3. **Caller tag on writes.** Decide: a per-tool tag injection in
   `mcp-client-runner`, or rely on clients tagging `claude`/`codex`.

### 6. Expose and hand the tokens to the cloud environments

- TLS proxy: `mcp.jaredlikes.com/agent/mcp → 127.0.0.1:8912/mcp`, with a path
  rewrite, mirroring `/lifegraph/mcp`. Commit the proxy config to the repo this
  time (proposal gap: no proxy config in repo).
- **Claude Code cloud environment** (environment menu → Edit):
  - add `mcp.jaredlikes.com` to the allowed domains;
  - add `static.crates.io` too, so cloud sessions can `cargo build`;
  - add environment variables `PHILOTIC_FRONTDOOR_URL=https://mcp.jaredlikes.com/agent/mcp`
    and `PHILOTIC_AGENT_MCP_TOKEN=<claude-cloud token>`.
- **Codex cloud:**
  - same domain allowlist;
  - `bearer_token_env_var = "PHILOTIC_AGENT_MCP_TOKEN"` in config;
  - check whether Codex exposes secrets only to setup scripts.
- After it's live, add the `philotic` HTTP server entry to `.mcp.json`
  (proposal §S5) and a "remote agents" paragraph to `CLAUDE.md`/`AGENTS.md`
  bootstrap. Both are hot files, so use their own small PR.

### 7. UAT and record

- Extend `scripts/mcp-client-uat.sh` with an `agent-frontdoor` mode:
  - `tools/list` shows exactly 16 tools;
  - a `muninn_recall` round trip succeeds;
  - `life.*` and `context.capture` are absent;
  - a revoked token fails.
- Add the `remote-agent-frontdoor` credential class to
  `docs/reference/MCP_CREDENTIAL_LIFECYCLE.md` (proposal §S6).
- `graph_decide` on the `agent-frontdoor` proposal. Write a Muninn memory delta
  (decision: frontdoor via `McpUpstream`; reality gaps found in step 2).

## Things the cloud session could not verify

- No `cargo build` or `cargo test`: `static.crates.io` is blocked by the cloud
  environment's allowlist.
- No live frontdoor, Muninn, or intel-graph: `mcp.jaredlikes.com` and the
  tailnet are blocked, and the Muninn binary is absent.
- The TLS termination for `mcp.jaredlikes.com` / `brain.jaredlikes.com` is not
  in the repo, so the proxy path mapping and the loopback-trust risk are
  inferred.
