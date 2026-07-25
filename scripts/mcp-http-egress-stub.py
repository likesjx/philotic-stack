#!/usr/bin/env python3
"""Loopback MCP HTTP stub for the governed outbound integration smoke."""

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import socket


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self) -> None:
        if self.path != "/mcp":
            self.send_error(404)
            return
        if self.headers.get("authorization") != "Bearer smoke-mcp-token":
            self.send_error(401)
            return
        length = int(self.headers.get("content-length", "0"))
        payload = json.loads(self.rfile.read(length) or b"{}")
        method = payload.get("method")
        request_id = payload.get("id")
        if method == "initialize":
            result = {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "egress-smoke", "version": "1"},
            }
        elif method == "notifications/initialized":
            result = None
        elif method == "tools/list":
            result = {
                "tools": [
                    {
                        "name": "echo",
                        "description": "Return governed MCP smoke evidence",
                        "inputSchema": {"type": "object", "properties": {}},
                    }
                ]
            }
        elif method == "tools/call":
            result = {
                "content": [
                    {
                        "type": "text",
                        "text": "mcp-through-governed-egress",
                    }
                ]
            }
        else:
            self._json(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "error": {"code": -32601, "message": f"unknown method {method}"},
                }
            )
            return
        response = {"jsonrpc": "2.0", "result": result}
        if request_id is not None:
            response["id"] = request_id
        self._json(response)

    def _json(self, payload: dict) -> None:
        body = json.dumps(payload, separators=(",", ":")).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("x-discard-me", "not-projected")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args: object) -> None:
        return


class LoopbackServer(ThreadingHTTPServer):
    def server_bind(self) -> None:
        # HTTPServer.server_bind performs a reverse-DNS lookup to populate
        # server_name. That can hang isolated CI/smoke environments and is
        # irrelevant for this loopback-only stub.
        self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.socket.bind(self.server_address)
        self.server_address = self.socket.getsockname()
        self.server_name = "127.0.0.1"
        self.server_port = self.server_address[1]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port-file", required=True)
    args = parser.parse_args()
    server = LoopbackServer(("127.0.0.1", 0), Handler)
    Path(args.port_file).write_text(str(server.server_port))
    server.serve_forever()


if __name__ == "__main__":
    main()
