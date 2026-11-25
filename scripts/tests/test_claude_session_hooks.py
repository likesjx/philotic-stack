#!/usr/bin/env python3
"""Hermetic tests for scripts/claude_session_hooks.py.

A stub graph REST server, a throwaway git repo and a fake transcript stand in
for the Mac, so these run anywhere (and in CI):

    python3 -m unittest discover -s scripts/tests -p test_claude_session_hooks.py -v
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HOOKS = ROOT / "scripts" / "claude_session_hooks.py"


class StubGraph:
    def __init__(self, nodes):
        self.nodes = nodes
        self.requests: list[tuple[str, str]] = []
        graph = self

        class H(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def do_GET(self):
                graph.requests.append(("GET", self.path))
                body = json.dumps(graph.nodes).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_POST(self):
                graph.requests.append(("POST", self.path))
                self.send_response(500)
                self.end_headers()

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), H)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()


def task(node_id, owner, status="open", priority=3, updated="2026-11-25T00:00:00Z", name=None):
    return {"id": node_id, "kind": "task", "name": name or node_id, "updated_at": updated,
            "properties": {"owner": owner, "status": status, "priority": priority}}


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def git(cwd, *args):
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True)


class HookTestCase(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.receipts = self.tmp / "receipts"
        self.env = {k: v for k, v in os.environ.items() if k != "CLAUDE_GRAPH_RUNNER"}
        self.env.update({
            "CLAUDE_RECEIPTS_DIR": str(self.receipts),
            "IG_GRAPH_URL": f"http://127.0.0.1:{free_port()}",
            "PHILOTIC_GRAPH_TOKEN": "",
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_CONFIG_NOSYSTEM": "1",
        })

    def run_hook(self, sub, payload, **env):
        e = dict(self.env, **env)
        return subprocess.run([sys.executable, "-B", str(HOOKS), sub], input=json.dumps(payload),
                              capture_output=True, text=True, env=e, timeout=30)

    def make_repo(self) -> Path:
        origin = self.tmp / "origin"
        repo = self.tmp / "repo"
        origin.mkdir()
        git(origin, "init", "-q", "-b", "develop")
        git(origin, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "base")
        git(self.tmp, "clone", "-q", str(origin), str(repo))
        git(repo, "checkout", "-q", "-b", "claude/feature")
        (repo / "a.txt").write_text("one\ntwo\n")
        git(repo, "add", "a.txt")
        git(repo, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "work")
        (repo / "a.txt").write_text("one\ntwo\nthree\n")
        (repo / "dirty.txt").write_text("x\n")
        return repo

    def receipts_written(self):
        return sorted(self.receipts.glob("*.json")) if self.receipts.exists() else []


class StartHookTests(HookTestCase):
    def test_lists_only_open_tasks_owned_by_claude_local(self):
        graph = StubGraph([
            task("task:a", "claude-local", priority=2, name="Fix the thing"),
            task("task:b", "claude-local", status="done"),
            task("task:c", "priti"),
            task("task:d", "claude-local", status="in_progress", priority=5),
        ])
        self.addCleanup(graph.close)
        out = self.run_hook("start", {"session_id": "s1"}, IG_GRAPH_URL=graph.url)
        self.assertEqual(out.returncode, 0, out.stderr)
        lines = out.stdout.splitlines()
        self.assertIn("2 open task(s) owned by claude-local", lines[0])
        self.assertTrue(lines[1].startswith("- task:d [in_progress]"))  # highest priority first
        self.assertIn("task:a [open] Fix the thing", lines[2])
        self.assertNotIn("task:b", out.stdout)
        self.assertNotIn("task:c", out.stdout)
        self.assertEqual(graph.requests, [("GET", "/api/nodes?kind=task")])  # read-only

    def test_caps_the_list_at_five(self):
        graph = StubGraph([task(f"task:{i}", "claude-local") for i in range(8)])
        self.addCleanup(graph.close)
        out = self.run_hook("start", {}, IG_GRAPH_URL=graph.url)
        self.assertEqual(sum(1 for l in out.stdout.splitlines() if l.startswith("- task:")), 5)
        self.assertIn("and 3 more", out.stdout)

    def test_graph_down_says_so_and_exits_zero(self):
        out = self.run_hook("start", {})
        self.assertEqual(out.returncode, 0)
        self.assertIn("not reachable", out.stdout)

    def test_runner_sessions_are_skipped(self):
        graph = StubGraph([task("task:a", "claude-local")])
        self.addCleanup(graph.close)
        out = self.run_hook("start", {}, IG_GRAPH_URL=graph.url, CLAUDE_GRAPH_RUNNER="1")
        self.assertEqual((out.returncode, out.stdout), (0, ""))
        self.assertEqual(graph.requests, [])


class EndHookTests(HookTestCase):
    def test_writes_one_receipt_with_git_facts_and_tests(self):
        repo = self.make_repo()
        transcript = self.tmp / "t.jsonl"
        lines = [
            {"message": {"content": [{"type": "tool_use", "id": "u1", "name": "Bash",
                                      "input": {"command": "cargo test -p graph-intelligence"}}]}},
            {"message": {"content": [{"type": "tool_result", "tool_use_id": "u1", "is_error": True}]}},
            {"message": {"content": [{"type": "tool_use", "id": "u2", "name": "Bash",
                                      "input": {"command": "echo SECRET=abc"}}]}},
            {"message": {"content": [{"type": "tool_use", "id": "u3", "name": "Bash",
                                      "input": {"command": "just test"}}]}},
            {"message": {"content": [{"type": "tool_result", "tool_use_id": "u3"}]}},
            "not json",
        ]
        transcript.write_text("\n".join(l if isinstance(l, str) else json.dumps(l) for l in lines))
        out = self.run_hook("end", {"session_id": "abc-123", "cwd": str(repo), "reason": "exit",
                                    "transcript_path": str(transcript)})
        self.assertEqual(out.returncode, 0, out.stderr)
        files = self.receipts_written()
        self.assertEqual(len(files), 1)
        r = json.loads(files[0].read_text())
        self.assertEqual(r["schema"], "claude-local.session-receipt/v1")
        self.assertEqual((r["agent"], r["session_id"], r["reason"], r["published"]),
                         ("claude-local", "abc-123", "exit", False))
        g = r["git"]
        self.assertEqual(g["branch"], "claude/feature")
        self.assertEqual(g["base_ref"], "origin/develop")
        self.assertEqual(g["commits_ahead"], 1)
        self.assertIn("1 file changed", g["diffstat"])
        self.assertEqual(sorted(g["uncommitted"]), ["a.txt", "dirty.txt"])
        self.assertEqual(r["tests"], [
            {"command": "cargo test -p graph-intelligence", "result": "error"},
            {"command": "just test", "result": "ok"},
        ])
        self.assertNotIn("SECRET", files[0].read_text())

    def test_second_end_never_overwrites(self):
        repo = self.make_repo()
        payload = {"session_id": "same", "cwd": str(repo)}
        for _ in range(3):
            self.assertEqual(self.run_hook("end", payload).returncode, 0)
        files = self.receipts_written()
        self.assertEqual(len({f.name for f in files}), 3)
        for f in files:
            self.assertEqual(json.loads(f.read_text())["session_id"], "same")

    def test_outside_a_repo_still_writes_a_receipt(self):
        plain = self.tmp / "plain"
        plain.mkdir()
        out = self.run_hook("end", {"session_id": "x", "cwd": str(plain)})
        self.assertEqual(out.returncode, 0, out.stderr)
        r = json.loads(self.receipts_written()[0].read_text())
        self.assertEqual(r["git"], {"repo": False})
        self.assertEqual(r["tests"], [])

    def test_unwritable_journal_fails_open(self):
        blocker = self.tmp / "file"
        blocker.write_text("not a dir")
        out = self.run_hook("end", {"session_id": "x", "cwd": str(self.tmp)},
                            CLAUDE_RECEIPTS_DIR=str(blocker / "receipts"))
        self.assertEqual(out.returncode, 0)
        self.assertIn("claude_session_hooks end", out.stderr)

    def test_runner_sessions_write_nothing(self):
        out = self.run_hook("end", {"session_id": "x", "cwd": str(self.tmp)}, CLAUDE_GRAPH_RUNNER="1")
        self.assertEqual(out.returncode, 0)
        self.assertEqual(self.receipts_written(), [])


class SettingsTests(unittest.TestCase):
    def test_project_settings_wire_both_hooks(self):
        settings = json.loads((ROOT / ".claude" / "settings.json").read_text())
        hooks = settings["hooks"]
        commands = [h["command"] for group in hooks["SessionStart"] for h in group["hooks"]]
        self.assertTrue(any("claude_session_hooks.py" in c and c.endswith(" start") for c in commands))
        end = [h["command"] for group in hooks["SessionEnd"] for h in group["hooks"]]
        self.assertTrue(any("claude_session_hooks.py" in c and c.endswith(" end") for c in end))


if __name__ == "__main__":
    unittest.main()
