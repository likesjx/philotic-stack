#!/usr/bin/env python3
"""Hermetic tests for scripts/agent_auth_doctor.py.

A stub server plays the graph (REST status + MCP) and the frontdoor, so these
run anywhere:

    python3 -m unittest discover -s scripts/tests -p test_agent_auth_doctor.py -v
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import plistlib
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
DOCTOR = ROOT / "scripts" / "agent_auth_doctor.py"
SECRET = "s3cret-graph-token-value"


def load_doctor():
    spec = importlib.util.spec_from_file_location("agent_auth_doctor", DOCTOR)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)
    return mod


doctor = load_doctor()


class Stub:
    """Graph server that requires SECRET; frontdoor that answers a set error."""

    def __init__(self, token_required=True, report_auth=True, frontdoor_error=None):
        self.token_required = token_required
        self.report_auth = report_auth
        self.frontdoor_error = frontdoor_error
        stub = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def _send(self, status, body):
                raw = json.dumps(body).encode() if not isinstance(body, str) else body.encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def do_GET(self):
                if self.path == "/api/status":
                    server = {"started_at": "2026-11-21T10:00:00Z", "binary_sha256": "ab" * 32}
                    if stub.report_auth:
                        server["auth"] = {
                            "token_required": stub.token_required,
                            "token_scope": "every MCP call",
                            "read_only": False,
                        }
                    self._send(200, {"server": server})
                else:
                    self._send(404, {})

            def do_POST(self):
                self.rfile.read(int(self.headers.get("Content-Length", 0)))
                auth = self.headers.get("Authorization", "")
                ok = {"jsonrpc": "2.0", "id": 1, "result": {"tools": [{"name": "graph_status"}]}}
                if self.path == "/mcp":
                    if stub.token_required and auth != f"Bearer {SECRET}":
                        self._send(401, "missing graph token")
                    else:
                        self._send(200, ok)
                elif self.path == "/agent/mcp":
                    if stub.frontdoor_error is not None:
                        self._send(
                            200,
                            {"jsonrpc": "2.0", "id": 1, "error": {"code": stub.frontdoor_error, "message": "x"}},
                        )
                    else:
                        self._send(200, ok)
                else:
                    self._send(404, {})

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()


class DoctorTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name) / "repo"
        self.repo.mkdir()
        (self.repo / ".mcp.json").write_text(
            json.dumps({"mcpServers": {"intel-graph": {"type": "http", "url": "http://127.0.0.1:8901/mcp"}}})
        )
        self.plist = Path(self.tmp.name) / "com.philotic.intel-graph.plist"
        with self.plist.open("wb") as fh:
            plistlib.dump({"EnvironmentVariables": {"PHILOTIC_GRAPH_TOKEN": SECRET}}, fh)
        self.token_dir = Path(self.tmp.name) / "tokens"
        self.token_dir.mkdir()

    def tearDown(self):
        self.tmp.cleanup()

    def run_doctor(self, stub, env, extra=()):
        argv = [
            "--graph-url", stub.url,
            "--graph-mcp-url", f"{stub.url}/mcp",
            "--repo", str(self.repo),
            "--plist", str(self.plist),
            "--token-dir", str(self.token_dir),
            *extra,
        ]
        clean = {k: v for k, v in os.environ.items() if not k.startswith("PHILOTIC_")}
        clean.update(env)
        out = io.StringIO()
        with mock.patch.dict(os.environ, clean, clear=True), contextlib.redirect_stdout(out):
            code = doctor.main(argv)
        return code, out.getvalue()

    def test_token_enforced_but_not_sent_is_diagnosed(self):
        stub = Stub(token_required=True)
        try:
            code, out = self.run_doctor(stub, {})
        finally:
            stub.close()
        self.assertEqual(code, 1)
        self.assertIn("HTTP 401: the server enforces a token", out)
        self.assertIn("PHILOTIC_GRAPH_TOKEN is not set in this shell", out)
        self.assertIn("token REQUIRED", out)
        self.assertIn("sets a graph token (EnvironmentVariables)", out)
        self.assertIn("intel-graph entry sends no token header", out)
        self.assertNotIn(SECRET, out)

    def test_shell_token_that_matches_points_at_clients(self):
        stub = Stub(token_required=True)
        try:
            code, out = self.run_doctor(stub, {"PHILOTIC_GRAPH_TOKEN": SECRET})
        finally:
            stub.close()
        self.assertIn("passes with PHILOTIC_GRAPH_TOKEN from this shell", out)
        self.assertNotIn(SECRET, out)

    def test_wrong_shell_token_is_called_out(self):
        stub = Stub(token_required=True)
        try:
            _, out = self.run_doctor(stub, {"PHILOTIC_GRAPH_TOKEN": "other"})
        finally:
            stub.close()
        self.assertIn("this shell's value differs from the server's", out)

    def test_open_server_and_old_binary(self):
        stub = Stub(token_required=False, report_auth=False)
        try:
            _, out = self.run_doctor(stub, {})
        finally:
            stub.close()
        self.assertIn("tools/list works without a token", out)
        self.assertIn("not reported (server built before this check existed)", out)

    def test_frontdoor_error_codes_are_named(self):
        for code, phrase in ((-32003, "grant expired"), (-32001, "token refused"), (-32002, "allotment")):
            stub = Stub(token_required=False, frontdoor_error=code)
            try:
                _, out = self.run_doctor(
                    stub,
                    {"PHILOTIC_FRONTDOOR_URL": f"{stub.url}/agent/mcp", "PHILOTIC_AGENT_MCP_TOKEN": SECRET},
                    extra=("--skip-graph",),
                )
            finally:
                stub.close()
            self.assertIn(phrase, out)
            self.assertNotIn(SECRET, out)

    def test_old_token_files_are_flagged(self):
        old = self.token_dir / "claude-cloud.token"
        old.write_text(SECRET)
        forty_days = time.time() - 40 * 86400
        os.utime(old, (forty_days, forty_days))
        fresh = self.token_dir / "codex-cloud.token"
        fresh.write_text(SECRET)
        report = doctor.Report()
        doctor.check_token_files(report, self.token_dir, 30)
        text = report.render()
        self.assertIn("claude-cloud.token", text)
        self.assertIn("likely expired", text)
        self.assertIn("codex-cloud.token", text)
        self.assertNotIn(SECRET, text)

    def test_repo_mcp_json_header_counts_as_sending(self):
        (self.repo / ".mcp.json").write_text(
            json.dumps(
                {
                    "mcpServers": {
                        "intel-graph": {
                            "type": "http",
                            "url": "http://127.0.0.1:8901/mcp",
                            "headers": {"Authorization": "Bearer ${PHILOTIC_GRAPH_TOKEN:-}"},
                        }
                    }
                }
            )
        )
        report = doctor.Report()
        doctor.check_client_configs(report, self.repo, True)
        self.assertIn("sends a token header", report.render())


if __name__ == "__main__":
    unittest.main()
