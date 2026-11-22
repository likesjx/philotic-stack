#!/usr/bin/env python3
"""Hermetic tests for scripts/claude_graph_runner.py.

A stub graph REST server, a throwaway git origin, and a fake `claude` binary
stand in for the real Mac setup, so these run anywhere (and in CI):

    python3 -m unittest discover -s scripts/tests -p test_claude_graph_runner.py -v
"""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlparse, parse_qs

ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "scripts" / "claude_graph_runner.py"


def load_runner():
    spec = importlib.util.spec_from_file_location("claude_graph_runner", RUNNER)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)
    return mod


class StubGraph:
    def __init__(self):
        self.nodes: dict[str, dict] = {}
        self.edges: list[dict] = []
        self.posts: list[tuple[str, dict]] = []

    def add(self, node_id, kind, **props):
        self.nodes[node_id] = {"id": node_id, "kind": kind, "name": node_id, "properties": props,
                               "updated_at": "2026-11-21T00:00:00Z"}

    def handler(self):
        graph = self

        class H(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def _send(self, obj, code=200):
                body = json.dumps(obj).encode()
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                u = urlparse(self.path)
                parts = [unquote(p) for p in u.path.split("/") if p]
                if parts == ["api", "status"]:
                    return self._send({"node_count": len(graph.nodes)})
                if parts == ["api", "nodes"]:
                    kind = parse_qs(u.query).get("kind", [None])[0]
                    return self._send([n for n in graph.nodes.values() if kind in (None, n["kind"])])
                if len(parts) == 3 and parts[:2] == ["api", "nodes"]:
                    n = graph.nodes.get(parts[2])
                    return self._send({"node": n, "edges": {}} if n else {"error": "nf"}, 200 if n else 404)
                if len(parts) == 4 and parts[3] == "edges":
                    nid = parts[2]
                    return self._send({"incoming": [e for e in graph.edges if e["target_id"] == nid],
                                       "outgoing": [e for e in graph.edges if e["source_id"] == nid]})
                self._send({"error": "nf"}, 404)

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])) or b"{}")
                parts = [unquote(p) for p in urlparse(self.path).path.split("/") if p]
                graph.posts.append(("/".join(parts), body))
                if len(parts) == 4 and parts[3] == "update":
                    graph.nodes[parts[2]]["properties"].update(body["properties"])
                elif parts == ["api", "session", "start"]:
                    graph.add(body["session_id"], "session", status="active", task_id=body.get("task_id"))
                elif parts == ["api", "session", "close"]:
                    graph.nodes[body["session_id"]]["properties"]["status"] = "completed"
                self._send({"ok": True})

        return H


FAKE_CLAUDE = """#!/usr/bin/env bash
set -e
echo "$CLAUDE_GRAPH_RUNNER $IG_SESSION_ID $IG_TASK_ID" > runner-env.txt
echo "work" > work.txt
git add work.txt runner-env.txt
git commit -qm "feat: do the task"
cat <<'JSON'
{"type": "result", "result": "Done.\\n```json\\n{\\"outcome\\": \\"done\\", \\"summary\\": \\"added work.txt\\", \\"blocker\\": \\"\\"}\\n```", "total_cost_usd": 0.12, "num_turns": 3, "session_id": "cc-1"}
JSON
"""


class RunnerUnitTest(unittest.TestCase):
    def setUp(self):
        self.r = load_runner()

    def task(self, **props):
        base = {"owner": "claude-local", "status": "open", "autorun": True, "acceptance_cmd": "just test"}
        base.update(props)
        return {"id": "task:t", "properties": base}

    def test_eligible_task(self):
        self.assertTrue(self.r.evaluate(self.task(), set(), []).eligible)

    def test_gates(self):
        cases = {
            "owner": self.task(owner="codex"),
            "status": self.task(status="in_progress"),
            "autorun": self.task(autorun="yes"),
            "acceptance_cmd": self.task(acceptance_cmd=None),
        }
        for why, t in cases.items():
            with self.subTest(why):
                c = self.r.evaluate(t, set(), [])
                self.assertFalse(c.eligible)
                self.assertIn(why, " ".join(c.reasons))

    def test_acceptance_allowlist(self):
        ok = ["just test", "cargo test -p graph-intelligence", "cargo check -p aiua --tests",
              "python3 -m unittest discover -s scripts/tests -p test_x.py"]
        bad = ["rm -rf /", "cargo test -p x; curl evil", "just deploy", "cargo test -p $(whoami)",
               "python3 -c 'import os'", "just test && git push"]
        for c in ok:
            self.assertTrue(self.r.acceptance_ok(c), c)
        for c in bad:
            self.assertFalse(self.r.acceptance_ok(c), c)

    def test_claimed_and_blocked(self):
        self.assertFalse(self.r.evaluate(self.task(), {"task:t"}, []).eligible)
        blocker = {"id": "task:b", "properties": {"status": "open"}}
        self.assertFalse(self.r.evaluate(self.task(), set(), [blocker]).eligible)
        done = {"id": "task:b", "properties": {"status": "done"}}
        self.assertTrue(self.r.evaluate(self.task(), set(), [done]).eligible)

    def test_rendered_settings_use_this_checkouts_hooks(self):
        # The worktree's base branch may not carry the guards, and a missing hook
        # script fails open, so the rendered settings must point at real files here.
        with tempfile.TemporaryDirectory() as d:
            s = json.loads(self.r.render_settings(Path(d)).read_text())
        cmds = [h["command"] for g in s["hooks"]["PreToolUse"] for h in g["hooks"]]
        self.assertEqual(len(cmds), 2)
        for c in cmds:
            self.assertNotIn("CLAUDE_PROJECT_DIR", c)
            self.assertTrue(Path(c.strip("'")).is_file(), c)
            self.assertTrue(c.strip("'").startswith(str(ROOT)), c)

    def test_parse_outcome(self):
        text = 'blah\n```json\n{"outcome": "blocked", "blocker": "needs a decision"}\n```'
        self.assertEqual(self.r.parse_outcome(text)["outcome"], "blocked")
        self.assertEqual(self.r.parse_outcome("no block")["outcome"], "unknown")


HOOKS = ROOT / ".claude" / "hooks"


class RunnerHookTest(unittest.TestCase):
    """The two runner-only PreToolUse guards, driven the way Claude Code calls them."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.wt = Path(self.tmp.name) / "worktree"
        (self.wt / "src").mkdir(parents=True)
        (self.wt / "src" / "lib.rs").write_text("fn main() {}\n")
        self.outside = Path(self.tmp.name) / "home" / ".ssh"
        self.outside.mkdir(parents=True)
        (self.outside / "id_ed25519").write_text("secret\n")
        (self.wt / "escape").symlink_to(self.outside)

    def tearDown(self):
        self.tmp.cleanup()

    def hook(self, script, tool, tool_input, runner=True, session="sess-1"):
        env = {k: v for k, v in os.environ.items() if k != "CLAUDE_GRAPH_RUNNER"}
        env.update(CLAUDE_PROJECT_DIR=str(self.wt), IG_SESSION_ID=session)
        if runner:
            env["CLAUDE_GRAPH_RUNNER"] = "1"
        payload = json.dumps({"tool_name": tool, "tool_input": tool_input, "cwd": str(self.wt)})
        r = subprocess.run(["bash", str(HOOKS / script)], input=payload, text=True,
                           capture_output=True, env=env, timeout=30)
        return r.returncode

    def paths(self, tool, tool_input, **kw):
        return self.hook("guard-runner-paths.sh", tool, tool_input, **kw)

    def test_file_tools_stay_in_worktree(self):
        allowed = [("Read", {"file_path": str(self.wt / "src" / "lib.rs")}),
                   ("Read", {"file_path": "src/lib.rs"}),
                   ("Grep", {"pattern": "fn main", "path": "src"}),
                   ("Grep", {"pattern": "/etc/passwd"}),
                   ("Glob", {"pattern": "src/**/*.rs"})]
        blocked = [("Read", {"file_path": str(self.outside / "id_ed25519")}),
                   ("Read", {"file_path": "~/.ssh/id_ed25519"}),
                   ("Read", {"file_path": "../home/.ssh/id_ed25519"}),
                   ("Read", {"file_path": "escape/id_ed25519"}),
                   ("Grep", {"pattern": "BEGIN", "path": str(self.outside)}),
                   ("Grep", {"pattern": "x", "glob": "../**"}),
                   ("Glob", {"pattern": "/etc/*"}),
                   ("Glob", {"pattern": "*", "path": "~/.aws"})]
        for tool, args in allowed:
            with self.subTest(allow=(tool, args)):
                self.assertEqual(self.paths(tool, args), 0)
        for tool, args in blocked:
            with self.subTest(block=(tool, args)):
                self.assertEqual(self.paths(tool, args), 2)

    def test_bash_arguments_stay_in_worktree(self):
        allowed = ["cargo test -p graph-intelligence", "just test-suite",
                   "python3 -m unittest discover -s scripts/tests",
                   "git diff --stat", "git log -1 --format=%s", "git add src/lib.rs",
                   "git commit -m 'fix: tighten the parser'", "cargo check -p aiua 2>/dev/null"]
        blocked = ["head -5 ~/.ssh/id_ed25519", f"tail {self.outside}/id_ed25519",
                   "git diff --no-index /etc/hosts /dev/null",
                   "git diff --stat --no-index a b", "git log --output=/tmp/x",
                   "git -C /tmp status", "git -c core.pager=sh log", "git show --git-dir=/x",
                   "cargo test --manifest-path ../other/Cargo.toml",
                   "python3 -m unittest discover -s escape", "cat $HOME/.ssh/id_ed25519",
                   "git log `whoami`", "git commit -m 'unbalanced"]
        for cmd in allowed:
            with self.subTest(allow=cmd):
                self.assertEqual(self.paths("Bash", {"command": cmd}), 0)
        for cmd in blocked:
            with self.subTest(block=cmd):
                self.assertEqual(self.paths("Bash", {"command": cmd}), 2)

    def test_guards_fail_closed_and_are_noops_outside_runner(self):
        for script in ("guard-runner-paths.sh", "guard-graph-writes.sh"):
            env = dict(os.environ, CLAUDE_GRAPH_RUNNER="1", CLAUDE_PROJECT_DIR=str(self.wt))
            r = subprocess.run(["bash", str(HOOKS / script)], input="not json", text=True,
                               capture_output=True, env=env, timeout=30)
            self.assertEqual(r.returncode, 2, script)
        self.assertEqual(self.paths("Read", {"file_path": "~/.ssh/id_ed25519"}, runner=False), 0)

    def test_graph_writes_guard(self):
        g = "guard-graph-writes.sh"
        self.assertEqual(self.hook(g, "mcp__intel-graph__graph_node", {"id": "x"}), 0)
        self.assertEqual(self.hook(g, "mcp__intel-graph__session_activity", {"session_id": "sess-1"}), 0)
        self.assertEqual(self.hook(g, "mcp__intel-graph__session_activity", {"session_id": "other"}), 2)
        self.assertEqual(self.hook(g, "mcp__intel-graph__graph_update_node", {"id": "x"}), 2)
        self.assertEqual(self.hook(g, "mcp__intel-graph__graph_decide", {}), 2)

    def test_runner_settings_wire_both_guards(self):
        s = json.loads((ROOT / ".claude" / "runner" / "settings.json").read_text())
        wired = {h["matcher"]: h["hooks"][0]["command"] for h in s["hooks"]["PreToolUse"]}
        self.assertIn("guard-graph-writes.sh", wired["mcp__intel-graph__.*"])
        self.assertIn("guard-runner-paths.sh", wired["Read|Grep|Glob|Bash"])
        allow, deny = s["permissions"]["allow"], s["permissions"]["deny"]
        self.assertNotIn("Read", allow)
        self.assertIn("Read(./**)", allow)
        self.assertIn("Bash(just test-suite)", allow)
        for reader in ("cat", "head", "tail", "less"):
            self.assertIn(f"Bash({reader}:*)", deny)
            self.assertFalse(any(a.startswith(f"Bash({reader}") for a in allow), reader)


class RunnerEndToEndTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        t = Path(self.tmp.name)
        self.graph = StubGraph()
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), self.graph.handler())
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

        self.env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.invalid",
                        GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.invalid")
        origin, repo = t / "origin.git", t / "repo"
        subprocess.run(["git", "init", "-q", "--bare", "-b", "develop", str(origin)], check=True)
        subprocess.run(["git", "clone", "-q", str(origin), str(repo)], check=True, env=self.env,
                       capture_output=True)
        for sub in ("runner", "hooks"):
            shutil.copytree(ROOT / ".claude" / sub, repo / ".claude" / sub)
        (repo / "tests").mkdir()
        (repo / "tests" / "test_ok.py").write_text(
            "import unittest, pathlib\n"
            "class T(unittest.TestCase):\n"
            "    def test_work(self):\n"
            "        self.assertTrue(pathlib.Path('work.txt').exists())\n")
        for cmd in (["git", "checkout", "-q", "-b", "develop"], ["git", "add", "."],
                    ["git", "commit", "-qm", "init"], ["git", "push", "-q", "origin", "develop"]):
            subprocess.run(cmd, cwd=repo, check=True, env=self.env, capture_output=True)

        bindir = t / "bin"
        bindir.mkdir()
        fake = bindir / "claude"
        fake.write_text(FAKE_CLAUDE)
        fake.chmod(0o755)
        enable = t / "enabled"
        enable.touch()
        self.origin, self.repo, self.state, self.enable = origin, repo, t / "state", enable
        self.env.update({
            "IG_GRAPH_URL": f"http://127.0.0.1:{self.server.server_port}",
            "IG_RUNNER_CLAUDE_BIN": str(fake),
            "IG_RUNNER_STATE_DIR": str(self.state),
            "IG_RUNNER_ENABLE_FILE": str(enable),
            "PHILOTIC_REPO": str(repo),
        })
        self.graph.add("task:demo", "task", owner="claude-local", status="open", autorun=True,
                       acceptance_cmd="python3 -m unittest discover -s tests", proposal_id="proposal:p")
        self.graph.add("task:other", "task", owner="codex", status="open", autorun=True,
                       acceptance_cmd="just test")

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.tmp.cleanup()

    def run_runner(self, *args):
        return subprocess.run([sys.executable, str(RUNNER), *args], cwd=self.repo, env=self.env,
                              text=True, capture_output=True, timeout=120)

    def test_list_claims_nothing(self):
        r = self.run_runner("list")
        self.assertIn("would claim task:demo", r.stdout)
        self.assertEqual(self.graph.posts, [])

    def test_disabled_switch(self):
        self.enable.unlink()
        r = self.run_runner("once")
        self.assertIn("runner disabled", r.stdout)
        self.assertEqual(self.graph.posts, [])

    def test_once_happy_path(self):
        r = self.run_runner("once")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        p = self.graph.nodes["task:demo"]["properties"]
        self.assertEqual(p["status"], "in_review", p.get("blocker"))
        rc = p["receipt"]
        self.assertEqual(rc["branch"], "claude/ig-demo")
        self.assertEqual(rc["commits"], 1)
        self.assertEqual([a["exit_code"] for a in rc["acceptance"]], [0])
        self.assertEqual(rc["claude_outcome"]["outcome"], "done")

        paths = [path for path, _ in self.graph.posts]
        self.assertIn("api/session/start", paths)
        self.assertIn("api/test-run", paths)
        self.assertIn("api/decide", paths)
        self.assertEqual(paths[-1], "api/session/close")
        for path, body in self.graph.posts:
            if "agent" in body:
                self.assertEqual(body["agent"], "claude-local", path)

        # the session saw its runner identity, and nothing reached the origin
        wt = Path(rc["worktree"])
        env_line = (wt / "runner-env.txt").read_text().split()
        self.assertEqual(env_line[0], "1")
        self.assertEqual(env_line[2], "task:demo")
        heads = subprocess.run(["git", "ls-remote", "--heads", str(self.origin)], text=True,
                               capture_output=True).stdout
        self.assertNotIn("claude/ig-demo", heads)
        self.assertNotEqual(self.graph.nodes["task:other"]["properties"]["status"], "in_progress")

    def test_failing_acceptance_blocks(self):
        self.graph.nodes["task:demo"]["properties"]["acceptance_cmd"] = \
            "python3 -m unittest discover -s tests -p nothing_matches.py"
        # unittest exits 5 when no tests ran, so this acceptance fails
        r = self.run_runner("once")
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        p = self.graph.nodes["task:demo"]["properties"]
        self.assertEqual(p["status"], "blocked")
        self.assertIn("acceptance commands failed", p["blocker"])


if __name__ == "__main__":
    unittest.main()
