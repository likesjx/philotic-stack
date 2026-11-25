#!/usr/bin/env python3
"""Find out why agents get 401s from the intel graph or the agent frontdoor.

Run it where the agents run (the Mac for local Claude Code / Codex, vps-jane
for the frontdoor):

    just agent-auth-doctor            # or: python3 scripts/agent_auth_doctor.py

It never prints a token, only whether one is set, where it came from, and
what the server said when the token was (or wasn't) sent. Checks:

1. Graph server: is it up, which binary, and does it require a token
   (`/api/status` -> `auth`, reported by servers built from this change on)?
2. Graph MCP: does a plain `tools/list` get 401? Does it pass with
   PHILOTIC_GRAPH_TOKEN from this shell? That tells "server enforces a token
   the clients don't send" apart from "wrong token".
3. launchd (macOS): does the intel-graph plist set PHILOTIC_GRAPH_TOKEN?
4. Repo client configs: do .mcp.json, .claude/runner/mcp.json and
   .codex/config.toml send a token to the graph?
5. Frontdoor (when PHILOTIC_FRONTDOOR_URL is set): does `tools/list` work
   with PHILOTIC_AGENT_MCP_TOKEN(_FILE), and if not, which error (revoked or
   wrong token, expired grant, used-up allotment, HTTP 401)?
6. Frontdoor token files: how old are they? Provisioned tokens expire after
   EXPIRES_DAYS (default 30).

Exit code is 0 when nothing looks wrong, 1 when a finding needs attention.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import plistlib
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from typing import Optional

ROOT = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_GRAPH_URL = "http://127.0.0.1:8900"
DEFAULT_GRAPH_MCP_URL = "http://127.0.0.1:8901/mcp"
LAUNCHD_PLIST = pathlib.Path.home() / "Library" / "LaunchAgents" / "com.philotic.intel-graph.plist"
TOKEN_DIR = pathlib.Path(
    os.environ.get("TOKEN_OUT_DIR", str(pathlib.Path.home() / ".philotic" / "agent-frontdoor-tokens"))
).expanduser()
EXPIRES_DAYS = int(os.environ.get("EXPIRES_DAYS", "30"))

# membrane-mcp JSON-RPC error codes (crates/membrane-mcp/src/server.rs).
FRONTDOOR_ERRORS = {
    -32001: "token refused (wrong, revoked or rotated token, or tool not granted)",
    -32002: "allotment used up for this token",
    -32003: "grant expired: re-provision this agent's token",
}

OK, WARN, FAIL, INFO = "ok", "warn", "FAIL", "info"


@dataclass
class Report:
    lines: list[tuple[str, str, str]] = field(default_factory=list)

    def add(self, level: str, check: str, detail: str) -> None:
        self.lines.append((level, check, detail))

    @property
    def failed(self) -> bool:
        return any(level in (FAIL, WARN) for level, _, _ in self.lines)

    def render(self) -> str:
        width = max((len(c) for _, c, _ in self.lines), default=0)
        return "\n".join(f"[{lvl:>4}] {chk:<{width}}  {det}" for lvl, chk, det in self.lines)


@dataclass
class HttpResult:
    status: Optional[int]
    body: str
    error: Optional[str] = None

    def json(self) -> Optional[dict]:
        try:
            return json.loads(self.body)
        except (ValueError, TypeError):
            # Streamable-HTTP servers may answer as an SSE event.
            for line in self.body.splitlines():
                if line.startswith("data:"):
                    try:
                        return json.loads(line[5:].strip())
                    except ValueError:
                        return None
            return None


def http(url: str, payload: Optional[dict] = None, token: Optional[str] = None, timeout: float = 5.0) -> HttpResult:
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(url, data=data, method="POST" if data else "GET")
    req.add_header("Accept", "application/json, text/event-stream")
    if data:
        req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return HttpResult(resp.status, resp.read().decode(errors="replace"))
    except urllib.error.HTTPError as err:
        return HttpResult(err.code, err.read().decode(errors="replace"))
    except (urllib.error.URLError, OSError, TimeoutError) as err:
        return HttpResult(None, "", str(getattr(err, "reason", err)))


TOOLS_LIST = {"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}


def tool_count(result: HttpResult) -> Optional[int]:
    body = result.json() or {}
    tools = (body.get("result") or {}).get("tools")
    return len(tools) if isinstance(tools, list) else None


def check_graph_status(report: Report, graph_url: str) -> Optional[bool]:
    """Returns whether the server says it requires a token (None if unknown)."""
    res = http(f"{graph_url.rstrip('/')}/api/status")
    if res.status is None:
        report.add(FAIL, "graph server", f"not reachable at {graph_url} ({res.error}); is it running?")
        return None
    if res.status != 200:
        report.add(FAIL, "graph server", f"/api/status answered HTTP {res.status}")
        return None
    body = res.json() or {}
    server = body.get("server") or body.get("server_info") or {}
    started = server.get("started_at", "?")
    sha = (server.get("binary_sha256") or "")[:12] or "?"
    report.add(OK, "graph server", f"up; started {started}; binary sha256 {sha}")
    auth = server.get("auth")
    if not isinstance(auth, dict):
        report.add(INFO, "graph auth mode", "not reported (server built before this check existed); see the MCP probe")
        return None
    required = bool(auth.get("token_required"))
    mode = "read-only" if auth.get("read_only") else "full"
    report.add(
        INFO,
        "graph auth mode",
        f"token {'REQUIRED on ' + str(auth.get('token_scope')) if required else 'not required'} ({mode} server)",
    )
    return required


def check_graph_mcp(report: Report, mcp_url: str, env_token: Optional[str]) -> None:
    bare = http(mcp_url, TOOLS_LIST)
    if bare.status is None:
        report.add(FAIL, "graph MCP", f"not reachable at {mcp_url} ({bare.error})")
        return
    if bare.status == 200:
        report.add(OK, "graph MCP (no token)", f"tools/list works without a token ({tool_count(bare)} tools)")
        return
    if bare.status != 401:
        report.add(FAIL, "graph MCP (no token)", f"HTTP {bare.status}: {bare.body[:160]}")
        return
    report.add(
        WARN,
        "graph MCP (no token)",
        "HTTP 401: the server enforces a token, so every client that does not send one gets 401",
    )
    if not env_token:
        report.add(
            FAIL,
            "graph MCP (shell token)",
            "PHILOTIC_GRAPH_TOKEN is not set in this shell, so clients started from it can't send one. "
            "Either export it (same value the server has) or remove it from the server's environment",
        )
        return
    with_token = http(mcp_url, TOOLS_LIST, token=env_token)
    if with_token.status == 200:
        report.add(
            OK,
            "graph MCP (shell token)",
            f"passes with PHILOTIC_GRAPH_TOKEN from this shell ({tool_count(with_token)} tools): "
            "the fix is for clients to send it",
        )
    else:
        report.add(
            FAIL,
            "graph MCP (shell token)",
            f"HTTP {with_token.status} even with PHILOTIC_GRAPH_TOKEN: this shell's value differs from the server's",
        )


def check_launchd(report: Report, plist_path: pathlib.Path) -> None:
    if sys.platform != "darwin" and not plist_path.exists():
        return
    if not plist_path.exists():
        report.add(INFO, "launchd plist", f"{plist_path} not found (server not installed as a launchd service)")
        return
    try:
        with plist_path.open("rb") as fh:
            plist = plistlib.load(fh)
    except (OSError, plistlib.InvalidFileException) as err:
        report.add(WARN, "launchd plist", f"could not read {plist_path.name}: {err}")
        return
    env = plist.get("EnvironmentVariables") or {}
    has_token = bool(env.get("PHILOTIC_GRAPH_TOKEN"))
    args = plist.get("ProgramArguments") or []
    has_flag = "--token" in args
    if has_token or has_flag:
        report.add(
            INFO,
            "launchd plist",
            "sets a graph token ("
            + ("EnvironmentVariables" if has_token else "--token argument")
            + "): the server enforces it on loopback too",
        )
    else:
        report.add(OK, "launchd plist", "sets no graph token")
    mtime = time.strftime("%Y-%m-%d %H:%M", time.localtime(plist_path.stat().st_mtime))
    report.add(INFO, "launchd plist", f"last changed {mtime}")


def _graph_entry_sends_token(entry: dict) -> bool:
    headers = entry.get("headers") or {}
    return any(k.lower() in ("authorization", "x-philotic-graph-token") for k in headers)


def check_client_configs(report: Report, root: pathlib.Path, token_required: Optional[bool]) -> None:
    for rel in (".mcp.json", ".claude/runner/mcp.json"):
        path = root / rel
        if not path.exists():
            continue
        try:
            servers = json.loads(path.read_text()).get("mcpServers", {})
        except ValueError as err:
            report.add(WARN, rel, f"not valid JSON: {err}")
            continue
        entry = servers.get("intel-graph")
        if not entry:
            continue
        if _graph_entry_sends_token(entry):
            report.add(OK, rel, "intel-graph entry sends a token header")
        else:
            level = FAIL if token_required else INFO
            report.add(level, rel, "intel-graph entry sends no token header")
    codex = root / ".codex" / "config.toml"
    if codex.exists():
        text = codex.read_text()
        block = text.split("[mcp_servers.intel-graph]", 1)
        if len(block) == 2:
            section = block[1].split("\n[", 1)[0]
            sends = "bearer_token" in section or "headers" in section
            level = OK if sends else (FAIL if token_required else INFO)
            report.add(level, ".codex/config.toml", f"intel-graph entry {'sends' if sends else 'sends no'} token")


def _read_token_file(path_str: str) -> Optional[str]:
    path = pathlib.Path(path_str).expanduser()
    try:
        return path.read_text().strip() or None
    except OSError:
        return None


def check_frontdoor(report: Report, url: Optional[str], token: Optional[str], token_source: str) -> None:
    if not url:
        report.add(INFO, "frontdoor", "PHILOTIC_FRONTDOOR_URL not set; skipped")
        return
    if not token:
        report.add(WARN, "frontdoor", f"{url}: no PHILOTIC_AGENT_MCP_TOKEN(_FILE) in this shell; probing without one")
    res = http(url, TOOLS_LIST, token=token)
    if res.status is None:
        report.add(FAIL, "frontdoor", f"not reachable at {url} ({res.error})")
        return
    if res.status == 401:
        report.add(FAIL, "frontdoor", f"HTTP 401 from {url} with {token_source}: token missing or refused at the edge")
        return
    body = res.json() or {}
    err = body.get("error")
    if isinstance(err, dict):
        code = err.get("code")
        meaning = FRONTDOOR_ERRORS.get(code, err.get("message", "error"))
        report.add(FAIL, "frontdoor", f"JSON-RPC {code} with {token_source}: {meaning}")
        return
    if res.status != 200:
        report.add(FAIL, "frontdoor", f"HTTP {res.status}: {res.body[:160]}")
        return
    report.add(OK, "frontdoor", f"tools/list works with {token_source} ({tool_count(res)} tools)")


def check_token_files(report: Report, token_dir: pathlib.Path, expires_days: int, now: Optional[float] = None) -> None:
    if not token_dir.is_dir():
        return
    now = time.time() if now is None else now
    for path in sorted(token_dir.glob("*.token")):
        age_days = (now - path.stat().st_mtime) / 86400
        level = WARN if age_days >= expires_days else OK
        note = "past the default expiry; likely expired" if level == WARN else "within the default expiry"
        report.add(level, f"token file {path.name}", f"minted {age_days:.0f} days ago, {note} ({expires_days}d)")


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--graph-url", default=os.environ.get("PHILOTIC_GRAPH_URL", DEFAULT_GRAPH_URL))
    p.add_argument("--graph-mcp-url", default=os.environ.get("PHILOTIC_GRAPH_MCP_URL", DEFAULT_GRAPH_MCP_URL))
    p.add_argument("--frontdoor-url", default=os.environ.get("PHILOTIC_FRONTDOOR_URL"))
    p.add_argument("--repo", default=str(ROOT), help="repo checkout whose client configs to inspect")
    p.add_argument("--plist", default=str(LAUNCHD_PLIST))
    p.add_argument("--token-dir", default=str(TOKEN_DIR))
    p.add_argument("--skip-graph", action="store_true", help="skip the graph checks (e.g. on vps-jane)")
    return p


def main(argv: Optional[list[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    report = Report()

    graph_token = os.environ.get("PHILOTIC_GRAPH_TOKEN") or None
    report.add(INFO, "shell PHILOTIC_GRAPH_TOKEN", "set" if graph_token else "not set")

    token_required: Optional[bool] = None
    if not args.skip_graph:
        token_required = check_graph_status(report, args.graph_url)
        check_graph_mcp(report, args.graph_mcp_url, graph_token)
        check_launchd(report, pathlib.Path(args.plist).expanduser())
    check_client_configs(report, pathlib.Path(args.repo), token_required)

    fd_token = os.environ.get("PHILOTIC_AGENT_MCP_TOKEN") or None
    source = "PHILOTIC_AGENT_MCP_TOKEN"
    if not fd_token and os.environ.get("PHILOTIC_AGENT_MCP_TOKEN_FILE"):
        fd_token = _read_token_file(os.environ["PHILOTIC_AGENT_MCP_TOKEN_FILE"])
        source = "PHILOTIC_AGENT_MCP_TOKEN_FILE"
    check_frontdoor(report, args.frontdoor_url, fd_token, source if fd_token else "no token")
    check_token_files(report, pathlib.Path(args.token_dir).expanduser(), EXPIRES_DAYS)

    print(report.render())
    return 1 if report.failed else 0


if __name__ == "__main__":
    sys.exit(main())
