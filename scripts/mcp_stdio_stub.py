#!/usr/bin/env python3
"""Stdio MCP stub server for mcp-client-fabric Phase-3 verification.

Speaks newline-framed JSON-RPC over stdin/stdout (the stdio MCP convention),
exposing one tool `echo`. Also prints the scrubbed-or-not state of a marker
env var to stderr on startup so the env-scrub can be observed.

Used by scripts/mcp_stdio_stub.py registration in the smoke driver and by the
`phil mcp allow-command` ceremony proof.
"""

import json
import os
import sys


def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def main():
    # Prove env scrub: this should be ABSENT when spawned by the guest.
    leaked = os.environ.get("MCP_STDIO_SECRET_PROBE", "<absent>")
    sys.stderr.write(f"stdio-stub up; MCP_STDIO_SECRET_PROBE={leaked}\n")
    sys.stderr.flush()

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError:
            continue
        rid = req.get("id")
        method = req.get("method", "")
        if rid is None:  # notification
            continue
        if method == "initialize":
            result = {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "mcp-stdio-stub", "version": "0.1"},
            }
        elif method == "tools/list":
            result = {
                "tools": [
                    {
                        "name": "echo",
                        "description": "Echo back via stdio transport",
                        "inputSchema": {"type": "object", "properties": {}},
                    }
                ]
            }
        elif method == "tools/call":
            # Report the probe env var so the env-scrub is observable in the
            # tool result: "<absent>" proves the guest cleared it before spawn.
            probe = os.environ.get("MCP_STDIO_SECRET_PROBE", "<absent>")
            result = {
                "content": [
                    {"type": "text", "text": f"echo via stdio; probe={probe}"}
                ],
                "isError": False,
            }
        else:
            emit({"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "no such method"}})
            continue
        emit({"jsonrpc": "2.0", "id": rid, "result": result})


if __name__ == "__main__":
    main()
