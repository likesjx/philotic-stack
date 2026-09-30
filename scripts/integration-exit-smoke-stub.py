#!/usr/bin/env python3
"""Loopback HTTP stub for the installed two-hotel egress smoke."""

import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Handler(BaseHTTPRequestHandler):
    token = ""

    def do_GET(self) -> None:
        if self.path != "/v1/echo?probe=bounded":
            self.send_error(404)
            return
        if self.headers.get("Authorization") != f"Bearer {self.token}":
            self.send_error(401)
            return
        body = b'{"ok":true,"source":"bounded-egress"}'
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("X-Discard-Me", "secret-ish")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args: object) -> None:
        print(format % args, flush=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--token", required=True)
    args = parser.parse_args()
    Handler.token = args.token
    ThreadingHTTPServer(("127.0.0.1", args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
