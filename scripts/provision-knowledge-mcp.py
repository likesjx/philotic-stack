#!/usr/bin/env python3
"""Provision the governed Obsidian knowledge MCP server as a Philotic upstream.

The server remains a narrow stdio child. It exposes knowledge.* tools only;
the MCP client fabric owns process isolation, projection, and per-tool grants.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import socket
import struct
import sys
import time
from typing import Any

DEFAULT_SOCKET = os.environ.get("PHILOTIC_HOTEL_SOCKET", "/tmp/philotic-aiua.sock")
DEFAULT_OWNER = os.environ.get("PHILOTIC_KNOWLEDGE_OWNER", "agent-beacon-01")
DEFAULT_UPSTREAM = "obsidian-knowledge"
TOOL_NAMES = (
    "knowledge.search",
    "knowledge.read",
    "knowledge.sync.status",
    "knowledge.create.propose",
    "knowledge.patch.propose",
    "knowledge.link.propose",
    "knowledge.review.list",
)


def send_frame(sock: socket.socket, payload: dict[str, Any]) -> None:
    data = json.dumps(payload, separators=(",", ":")).encode()
    sock.sendall(struct.pack(">I", len(data)) + data)


def recv_frame(sock: socket.socket) -> dict[str, Any]:
    raw_length = b""
    while len(raw_length) < 4:
        chunk = sock.recv(4 - len(raw_length))
        if not chunk:
            raise RuntimeError("hotel socket closed before response length")
        raw_length += chunk
    length = struct.unpack(">I", raw_length)[0]
    data = b""
    while len(data) < length:
        chunk = sock.recv(length - len(data))
        if not chunk:
            raise RuntimeError("hotel socket closed during response")
        data += chunk
    result = json.loads(data)
    if not isinstance(result, dict):
        raise RuntimeError("hotel response was not a JSON object")
    return result


def ipc_call(sock: socket.socket, operation: str, payload: dict[str, Any]) -> dict[str, Any]:
    send_frame(sock, {"operation": operation, "payload": payload})
    for _ in range(32):
        response = recv_frame(sock)
        body = response.get("payload") if isinstance(response.get("payload"), dict) else response
        if (
            "mcp_upstream_id" in body
            or response_error(response)
            or response.get("operation") == "mcp_upstream_registered"
        ):
            return response
        # The hotel can broadcast Muninn/network/lease state between a request
        # and its FIFO reply. Raw IPC helpers must not mistake that push for
        # the registration result.
    raise RuntimeError("hotel sent 32 unrelated frames without a registration response")


def build_config(
    *,
    upstream_id: str,
    owner_agent_id: str,
    python_command: str,
    server_script: Path,
    updated_at: int,
) -> dict[str, Any]:
    return {
        "upstream_id": upstream_id,
        "owner_agent_id": owner_agent_id,
        "transport": {
            "kind": "stdio",
            "command": python_command,
            "args": [str(server_script.resolve()), "serve"],
        },
        "tool_allowlist": [
            {
                "remote_name": name,
                "allotment": 240 if name in {"knowledge.search", "knowledge.read"} else 60,
                "max_response_bytes": 262_144,
            }
            for name in TOOL_NAMES
        ],
        "grant_agents": [],
        "refresh_interval_secs": 300,
        "updated_at": updated_at,
    }


def response_error(response: dict[str, Any]) -> str | None:
    if response.get("ok") is False:
        return str(response.get("message") or response)
    if response.get("operation") == "error":
        return str(response.get("payload") or response)
    if "error" in response:
        return str(response["error"])
    return None


def parse_args() -> argparse.Namespace:
    default_script = Path(__file__).with_name("obsidian_knowledge.py")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", default=DEFAULT_SOCKET)
    parser.add_argument("--owner", default=DEFAULT_OWNER)
    parser.add_argument("--upstream-id", default=DEFAULT_UPSTREAM)
    parser.add_argument("--python", default=sys.executable)
    parser.add_argument("--server-script", type=Path, default=default_script)
    parser.add_argument(
        "--print-allow-command",
        action="store_true",
        help="print the exact operator allowlist command without changing state",
    )
    return parser.parse_args()


def allow_command_text(python_command: str, server_script: Path) -> str:
    return (
        f"phil mcp allow-command {json.dumps(python_command)} "
        f"--args-prefix {json.dumps(str(server_script.resolve()))} serve"
    )


def main() -> int:
    args = parse_args()
    script = args.server_script.resolve()
    if not script.is_file():
        print(f"ERROR: knowledge server not found: {script}", file=sys.stderr)
        return 2
    if args.print_allow_command:
        print(allow_command_text(args.python, script))
        return 0

    config = build_config(
        upstream_id=args.upstream_id,
        owner_agent_id=args.owner,
        python_command=args.python,
        server_script=script,
        updated_at=int(time.time()),
    )
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(10)
            sock.connect(args.socket)
            response = ipc_call(sock, "register_mcp_upstream", {"config": config})
    except (OSError, RuntimeError, json.JSONDecodeError) as exc:
        print(f"ERROR: cannot provision knowledge MCP upstream: {exc}", file=sys.stderr)
        print(
            "The hotel must be running. The stdio command must also be explicitly "
            "allowed first:\n  " + allow_command_text(args.python, script),
            file=sys.stderr,
        )
        return 1

    error = response_error(response)
    if error:
        print(f"ERROR: hotel rejected knowledge MCP upstream: {error}", file=sys.stderr)
        if "STDIO_NOT_ALLOWED" in error or "not on the operator allowlist" in error:
            print("Run:\n  " + allow_command_text(args.python, script), file=sys.stderr)
        return 1

    payload = response.get("payload") if isinstance(response.get("payload"), dict) else response
    actual_id = payload.get("mcp_upstream_id") or payload.get("upstream_id")
    if actual_id != args.upstream_id:
        print(f"ERROR: unexpected hotel response: {response}", file=sys.stderr)
        return 1
    materialized = bool(payload.get("mcp_upstream_materialized"))
    print(
        f"registered upstream={actual_id} materialized={str(materialized).lower()} "
        f"tools={len(TOOL_NAMES)}"
    )
    print("Verify projection with: phil mcp upstreams")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
