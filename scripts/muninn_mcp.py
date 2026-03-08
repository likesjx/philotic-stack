#!/usr/bin/env python3
import argparse
import json
import sys
import urllib.parse
import urllib.request


DEFAULT_BASE_URL = "http://localhost:8750/mcp"


class MuninnMcpClient:
    def __init__(self, base_url: str):
        self.base_url = base_url.rstrip("/")
        self.message_url = None
        self._sse_response = None

    def connect(self) -> None:
        req = urllib.request.Request(self.base_url, headers={"accept": "text/event-stream"})
        self._sse_response = urllib.request.urlopen(req, timeout=10)
        endpoint = None
        while True:
            raw = self._sse_response.readline()
            if not raw:
                break
            line = raw.decode("utf-8", errors="replace").strip()
            if line.startswith("data: "):
                endpoint = line[len("data: ") :].strip()
                break
        if not endpoint:
            self.close()
            raise RuntimeError("Muninn MCP handshake did not return a message endpoint")

        if endpoint.startswith("http://") or endpoint.startswith("https://"):
            self.message_url = endpoint
        else:
            parsed = urllib.parse.urlparse(self.base_url)
            origin = f"{parsed.scheme}://{parsed.netloc}"
            self.message_url = urllib.parse.urljoin(origin + "/", endpoint.lstrip("/"))
        self._post(
            {
                "jsonrpc": "2.0",
                "id": "initialize",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": "muninn_mcp.py", "version": "1.0"},
                },
            }
        )
        self._post(
            {
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {},
            }
        )

    def close(self) -> None:
        if self._sse_response is not None:
            self._sse_response.close()
            self._sse_response = None

    def _post(self, payload: dict) -> dict:
        if not self.message_url:
            raise RuntimeError("Muninn MCP client is not connected")
        body = json.dumps(payload).encode("utf-8")
        req = urllib.request.Request(
            self.message_url,
            data=body,
            headers={"content-type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=20) as response:
            raw = response.read().decode("utf-8", errors="replace")
        if not raw.strip():
            return {}
        return json.loads(raw)

    def tools_list(self) -> dict:
        return self._post({"jsonrpc": "2.0", "id": "tools-list", "method": "tools/list", "params": {}})

    def call_tool(self, name: str, arguments: dict) -> dict:
        return self._post(
            {
                "jsonrpc": "2.0",
                "id": f"tool-{name}",
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            }
        )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Muninn MCP helper")
    parser.add_argument("--base-url", default=DEFAULT_BASE_URL, help="Muninn MCP base URL")
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("tools", help="List available Muninn tools")

    where = sub.add_parser("where-left-off", help="Retrieve recent active memory")
    where.add_argument("--limit", type=int, default=5)

    recall = sub.add_parser("recall", help="Recall relevant memory")
    recall.add_argument("--context", action="append", required=True, help="Context phrase (repeatable)")
    recall.add_argument("--limit", type=int, default=5)
    recall.add_argument("--mode", default="semantic")

    remember = sub.add_parser("remember", help="Store an atomic memory")
    remember.add_argument("--content", required=True)
    remember.add_argument("--concept")
    remember.add_argument("--summary")
    remember.add_argument("--type", default="fact")
    remember.add_argument("--tag", action="append", default=[])

    decide = sub.add_parser("decide", help="Store a decision with rationale")
    decide.add_argument("--decision", required=True)
    decide.add_argument("--rationale", required=True)
    decide.add_argument("--alternative", action="append", default=[])
    decide.add_argument("--evidence-id", action="append", default=[])

    call = sub.add_parser("call", help="Call an arbitrary Muninn tool with JSON args")
    call.add_argument("tool_name")
    call.add_argument("--args-json", default="{}")

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()

    client = MuninnMcpClient(args.base_url)
    client.connect()

    if args.command == "tools":
        result = client.tools_list()
    elif args.command == "where-left-off":
        result = client.call_tool("muninn_where_left_off", {"limit": args.limit})
    elif args.command == "recall":
        result = client.call_tool(
            "muninn_recall",
            {"context": args.context, "limit": args.limit, "mode": args.mode},
        )
    elif args.command == "remember":
        payload = {
            "content": args.content,
            "type": args.type,
        }
        if args.concept:
            payload["concept"] = args.concept
        if args.summary:
            payload["summary"] = args.summary
        if args.tag:
            payload["tags"] = args.tag
        result = client.call_tool("muninn_remember", payload)
    elif args.command == "decide":
        payload = {
            "decision": args.decision,
            "rationale": args.rationale,
        }
        if args.alternative:
            payload["alternatives"] = args.alternative
        if args.evidence_id:
            payload["evidence_ids"] = args.evidence_id
        result = client.call_tool("muninn_decide", payload)
    elif args.command == "call":
        result = client.call_tool(args.tool_name, json.loads(args.args_json))
    else:
        parser.error(f"unsupported command {args.command}")
        return 2

    try:
        json.dump(result, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 0
    finally:
        client.close()


if __name__ == "__main__":
    raise SystemExit(main())
