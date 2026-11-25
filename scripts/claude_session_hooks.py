#!/usr/bin/env python3
"""Claude Code session hooks for claude-local: show its graph tasks, keep a receipt.

Plan: graph engineering adoption, first slice (local session receipts).

    start   SessionStart hook. Prints up to five open IntelliGraph tasks owned
            by claude-local, so a session starts knowing what is assigned to it.
            Read-only: one GET against the loopback graph REST API.
    end     SessionEnd hook. Writes one receipt for the session to a local,
            append-only journal: branch, head and base sha, diffstat,
            uncommitted files and the test commands the session ran.

Rules this slice keeps:
  * Nothing here writes to the graph. Publishing receipts to the ledger is a
    later step that needs its own writer and the operator's approval.
  * A receipt file is created exclusively and never rewritten. A second
    SessionEnd for the same session gets its own file.
  * Both hooks fail open: any error prints one line to stderr and exits 0, so
    a broken hook never blocks or wedges a session.
  * Skipped in graph-runner sessions (CLAUDE_GRAPH_RUNNER=1); the runner
    writes its own receipt.

Stdlib only. Env overrides: IG_GRAPH_URL, PHILOTIC_GRAPH_TOKEN,
CLAUDE_LOCAL_AGENT, CLAUDE_RECEIPTS_DIR, CLAUDE_HOOKS_BASE_REF.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

SCHEMA = "claude-local.session-receipt/v1"
MAX_TASKS = 5
MAX_FILES = 50
MAX_TESTS = 20
DONE_STATUSES = {"done", "completed", "closed", "merged", "archived", "superseded", "cancelled"}
# Only commands that look like test runs are copied from the transcript, so a
# receipt never carries arbitrary shell lines.
TEST_COMMAND = re.compile(
    r"^\s*(just (test|test-suite|check)\b|cargo (test|nextest|check|clippy)\b|"
    r"python3? (-B )?-m (unittest|pytest)\b|pytest\b)"
)


def agent() -> str:
    return os.environ.get("CLAUDE_LOCAL_AGENT", "claude-local")


def graph_url() -> str:
    return os.environ.get("IG_GRAPH_URL", "http://127.0.0.1:8900").rstrip("/")


def receipts_dir() -> Path:
    default = Path.home() / ".philotic" / "claude-local" / "receipts"
    return Path(os.environ.get("CLAUDE_RECEIPTS_DIR", default))


def read_hook_input() -> dict:
    try:
        raw = sys.stdin.read()
        data = json.loads(raw) if raw.strip() else {}
        return data if isinstance(data, dict) else {}
    except (OSError, ValueError):
        return {}


# ── start: show claude-local's open tasks ─────────────────────────────────


def fetch_tasks(timeout: float = 3.0) -> list[dict]:
    headers = {"Accept": "application/json"}
    token = os.environ.get("PHILOTIC_GRAPH_TOKEN", "")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(graph_url() + "/api/nodes?kind=task", headers=headers)
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        nodes = json.loads(resp.read() or b"[]")
    return nodes if isinstance(nodes, list) else []


def open_tasks(nodes: list[dict], who: str) -> list[dict]:
    mine = []
    for n in nodes:
        props = n.get("properties") or {}
        if props.get("owner") != who:
            continue
        if str(props.get("status", "open")).lower() in DONE_STATUSES:
            continue
        mine.append(n)

    def prio(n: dict) -> int:
        value = (n.get("properties") or {}).get("priority", 3)
        return value if isinstance(value, int) else 3

    newest_first = sorted(mine, key=lambda n: str(n.get("updated_at", "")), reverse=True)
    return sorted(newest_first, key=prio, reverse=True)


def render_tasks(tasks: list[dict], who: str) -> str:
    if not tasks:
        return f"IntelliGraph: no open tasks owned by {who}."
    lines = [f"IntelliGraph: {len(tasks)} open task(s) owned by {who}:"]
    for n in tasks[:MAX_TASKS]:
        props = n.get("properties") or {}
        status = props.get("status", "open")
        name = (n.get("name") or "").replace("\n", " ")[:100]
        lines.append(f"- {n.get('id')} [{status}] {name}")
    if len(tasks) > MAX_TASKS:
        lines.append(f"- … and {len(tasks) - MAX_TASKS} more (graph_query owner={who})")
    lines.append("Claim one with session_start before working it; record the outcome with graph_decide.")
    return "\n".join(lines)


def cmd_start(_hook: dict) -> int:
    who = agent()
    try:
        tasks = open_tasks(fetch_tasks(), who)
    except (urllib.error.URLError, OSError, ValueError) as e:
        reason = getattr(e, "reason", e)
        print(f"IntelliGraph not reachable at {graph_url()} ({reason}); skipping the task list.")
        return 0
    print(render_tasks(tasks, who))
    return 0


# ── end: write one session receipt ─────────────────────────────────────────


def git(cwd: Path, *args: str) -> str | None:
    try:
        out = subprocess.run(
            ["git", *args], cwd=cwd, capture_output=True, text=True, timeout=10, check=False
        )
    except (OSError, subprocess.SubprocessError):
        return None
    # rstrip only: porcelain status lines start with a meaningful space.
    return out.stdout.rstrip() if out.returncode == 0 else None


def git_facts(cwd: Path) -> dict:
    top = git(cwd, "rev-parse", "--show-toplevel")
    if not top:
        return {"repo": False}
    root = Path(top)
    facts: dict = {
        "repo": True,
        "worktree": top,
        "branch": git(root, "rev-parse", "--abbrev-ref", "HEAD"),
        "head": git(root, "rev-parse", "HEAD"),
    }
    base_ref = os.environ.get("CLAUDE_HOOKS_BASE_REF", "origin/develop")
    base = git(root, "merge-base", base_ref, "HEAD") if facts["head"] else None
    facts["base_ref"] = base_ref if base else None
    facts["base"] = base
    if base:
        ahead = git(root, "rev-list", "--count", f"{base}..HEAD")
        facts["commits_ahead"] = int(ahead) if ahead and ahead.isdigit() else None
        facts["diffstat"] = (git(root, "diff", "--shortstat", f"{base}..HEAD") or "").strip()
    status = git(root, "status", "--porcelain=v1", "--untracked-files=normal")
    files = [line[3:] for line in (status or "").splitlines() if line.strip()]
    facts["uncommitted_count"] = len(files)
    facts["uncommitted"] = files[:MAX_FILES]
    return facts


def tests_from_transcript(path: str | None) -> list[dict]:
    if not path:
        return []
    calls: dict[str, dict] = {}
    order: list[str] = []
    try:
        with open(path, encoding="utf-8") as fh:
            for line in fh:
                try:
                    entry = json.loads(line)
                except ValueError:
                    continue
                content = ((entry.get("message") or {}).get("content")) if isinstance(entry, dict) else None
                if not isinstance(content, list):
                    continue
                for item in content:
                    if not isinstance(item, dict):
                        continue
                    if item.get("type") == "tool_use" and item.get("name") == "Bash":
                        command = str((item.get("input") or {}).get("command", ""))
                        if TEST_COMMAND.match(command) and item.get("id"):
                            calls[item["id"]] = {"command": command.strip()[:200], "result": "unknown"}
                            order.append(item["id"])
                    elif item.get("type") == "tool_result" and item.get("tool_use_id") in calls:
                        calls[item["tool_use_id"]]["result"] = "error" if item.get("is_error") else "ok"
    except OSError:
        return []
    return [calls[i] for i in order][-MAX_TESTS:]


def write_receipt(receipt: dict, directory: Path, stamp: str, session: str) -> Path:
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    safe = re.sub(r"[^A-Za-z0-9_-]", "", session)[:12] or "nosession"
    body = (json.dumps(receipt, indent=2, sort_keys=True) + "\n").encode()
    for n in range(100):
        suffix = "" if n == 0 else f"-{n}"
        path = directory / f"{stamp}-{safe}{suffix}.json"
        try:
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        except FileExistsError:
            continue
        with os.fdopen(fd, "wb") as fh:
            fh.write(body)
            fh.flush()
            os.fsync(fh.fileno())
        return path
    raise OSError("no free receipt file name")


def cmd_end(hook: dict) -> int:
    now = datetime.now(timezone.utc)
    cwd = Path(hook.get("cwd") or os.getcwd())
    session = str(hook.get("session_id") or "")
    receipt = {
        "schema": SCHEMA,
        "agent": agent(),
        "session_id": session or None,
        "reason": hook.get("reason"),
        "ended_at": now.isoformat(timespec="seconds"),
        "cwd": str(cwd),
        "git": git_facts(cwd),
        "tests": tests_from_transcript(hook.get("transcript_path")),
        "published": False,
    }
    path = write_receipt(receipt, receipts_dir(), now.strftime("%Y%m%dT%H%M%SZ"), session)
    print(f"session receipt: {path}", file=sys.stderr)
    return 0


def main(argv: list[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if os.environ.get("CLAUDE_GRAPH_RUNNER") == "1":
        return 0
    commands = {"start": cmd_start, "end": cmd_end}
    if not argv or argv[0] not in commands:
        print("usage: claude_session_hooks.py start|end  (hook JSON on stdin)", file=sys.stderr)
        return 0
    try:
        return commands[argv[0]](read_hook_input())
    except Exception as e:  # fail open: a hook must never block a session
        print(f"claude_session_hooks {argv[0]}: {type(e).__name__}: {e}", file=sys.stderr)
        return 0


if __name__ == "__main__":
    sys.exit(main())
