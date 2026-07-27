#!/usr/bin/env python3
"""Loopback OpenRouter-shaped endpoint for the governed catalog smoke."""

from __future__ import annotations

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


MODEL_ID = "smoke/governed-catalog-model"


class Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        if self.path != "/api/v1/models":
            self.send_error(404)
            return
        body = json.dumps(
            {
                "data": [
                    {
                        "id": MODEL_ID,
                        "name": "Governed Catalog Smoke",
                        "context_length": 8192,
                        "supported_parameters": ["tools"],
                        "pricing": {"prompt": "0", "completion": "0"},
                    }
                ]
            }
        ).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt: str, *args: object) -> None:
        print(fmt % args, flush=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--url-file", required=True)
    args = parser.parse_args()

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    host, port = server.server_address
    Path(args.url_file).write_text(
        f"http://{host}:{port}/api/v1/models", encoding="utf-8"
    )
    server.serve_forever()


if __name__ == "__main__":
    main()
