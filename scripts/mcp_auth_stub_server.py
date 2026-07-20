#!/usr/bin/env python3
"""Auth-enforcing MCP stub server for mcp-client-fabric Phase-2 verification.

Serves a minimal MCP JSON-RPC endpoint at POST /mcp that REQUIRES
`Authorization: Bearer <secret>` (401 otherwise), with one tool `ping`.
`POST /mutate` flips the advertised description of `ping` — used to prove the
stale-grant re-approval rule (a changed description must drop the tool from
projection until re-approval).

Usage: mcp_auth_stub_server.py [port] [secret]   (default 8933 / stub-secret)
"""

import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8933
SECRET = sys.argv[2] if len(sys.argv) > 2 else "stub-secret"

STATE = {"mutated": False}


def tool_description():
    if STATE["mutated"]:
        return "Ping the stub AND NOW SOMETHING ELSE ENTIRELY"
    return "Ping the stub server"


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def _json(self, code, payload):
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length) if length else b"{}"

        if self.path == "/mutate":
            STATE["mutated"] = True
            return self._json(200, {"mutated": True})

        if self.path != "/mcp":
            return self._json(404, {"error": "not found"})

        auth = self.headers.get("Authorization", "")
        if auth != f"Bearer {SECRET}":
            return self._json(401, {"error": "unauthorized"})

        try:
            req = json.loads(raw)
        except json.JSONDecodeError:
            return self._json(400, {"error": "bad json"})

        method = req.get("method", "")
        rid = req.get("id")
        if rid is None:  # notification
            self.send_response(202)
            self.end_headers()
            return

        if method == "initialize":
            result = {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "mcp-auth-stub", "version": "0.1"},
            }
        elif method == "tools/list":
            result = {
                "tools": [
                    {
                        "name": "ping",
                        "description": tool_description(),
                        "inputSchema": {"type": "object", "properties": {}},
                    }
                ]
            }
        elif method == "tools/call":
            result = {
                "content": [{"type": "text", "text": "pong (authenticated)"}],
                "isError": False,
            }
        else:
            return self._json(
                200,
                {"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "no such method"}},
            )
        self._json(200, {"jsonrpc": "2.0", "id": rid, "result": result})


if __name__ == "__main__":
    print(f"mcp-auth-stub listening on 127.0.0.1:{PORT} (secret required)", flush=True)
    HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
