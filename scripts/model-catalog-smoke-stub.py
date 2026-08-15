#!/usr/bin/env python3
"""Loopback OpenRouter and Hugging Face endpoints for catalog egress smoke."""

from __future__ import annotations

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit


MODEL_ID = "smoke/governed-catalog-model"
HUGGINGFACE_MODEL_ID = "smoke/governed-huggingface-model"


class Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        request = urlsplit(self.path)
        if request.path == "/api/v1/models" and not request.query:
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
        elif request.path == "/api/models":
            query = parse_qs(request.query)
            expected = {
                "sort": ["downloads"],
                "direction": ["-1"],
                "limit": ["100"],
                "full": ["true"],
                "cardData": ["true"],
            }
            if query != expected:
                self.send_error(400, f"unexpected Hugging Face query: {query!r}")
                return
            body = json.dumps(
                [
                    {
                        "id": HUGGINGFACE_MODEL_ID,
                        "pipeline_tag": "sentence-similarity",
                        "library_name": "sentence-transformers",
                        "downloads": 12,
                        "likes": 3,
                        "private": False,
                        "sha": "smoke-revision",
                        "cardData": {"license": "apache-2.0"},
                    }
                ]
            ).encode()
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt: str, *args: object) -> None:
        print(fmt % args, flush=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--openrouter-url-file", required=True)
    parser.add_argument("--huggingface-url-file", required=True)
    args = parser.parse_args()

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    host, port = server.server_address
    Path(args.openrouter_url_file).write_text(
        f"http://{host}:{port}/api/v1/models", encoding="utf-8"
    )
    Path(args.huggingface_url_file).write_text(
        f"http://{host}:{port}/api/models", encoding="utf-8"
    )
    server.serve_forever()


if __name__ == "__main__":
    main()
