#!/usr/bin/env python3
import argparse
import datetime
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys
import time
import urllib.parse
import urllib.request
from typing import List, Optional


DEFAULT_BASE_URL = "http://localhost:8750/mcp"
REQUIRED_TOOLS = (
    "muninn_where_left_off",
    "muninn_recall",
    "muninn_remember",
    "muninn_decide",
)
APPROVAL_REQUIRED_EXIT = 42
DEFAULT_MUNINN_DIR = pathlib.Path.home() / "code" / "muninndb"


class MuninnMcpClient:
    def __init__(self, base_url: str, token: Optional[str] = None, request_timeout: float = 60.0):
        self.base_url = base_url.rstrip("/")
        self.token = token
        self.request_timeout = request_timeout
        self.message_url = None
        self._sse_response = None

    def connect(self) -> None:
        req = urllib.request.Request(self.base_url, headers=self._headers({"accept": "text/event-stream"}))
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
            headers=self._headers({"content-type": "application/json"}),
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=self.request_timeout) as response:
            raw = response.read().decode("utf-8", errors="replace")
        if not raw.strip():
            return {}
        return json.loads(raw)

    def _headers(self, headers: dict) -> dict:
        if self.token:
            headers = dict(headers)
            headers["authorization"] = f"Bearer {self.token}"
        return headers

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
    parser.add_argument("--token", help="MCP bearer token; defaults to MUNINN_MCP_TOKEN or token file")
    parser.add_argument(
        "--timeout",
        type=float,
        default=60.0,
        help="HTTP request timeout in seconds for MCP message calls.",
    )
    parser.add_argument(
        "--token-file",
        default=os.environ.get("MUNINN_MCP_TOKEN_FILE", str(pathlib.Path.home() / ".muninn" / "mcp.token")),
        help="Path to MCP bearer token file when token auth is enabled",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("health", help="Check Muninn MCP connectivity and required tools")
    sub.add_parser(
        "bootstrap",
        help="Require Muninn readiness, attempting a local muninndb-server start first when the service is merely down",
    )
    sub.add_parser(
        "require",
        help="Fail loudly unless Muninn MCP is reachable and ready; intended for session bootstrap gating",
    )
    sub.add_parser("tools", help="List available Muninn tools")

    where = sub.add_parser("where-left-off", help="Retrieve recent active memory")
    where.add_argument("--limit", type=int, default=5)

    recall = sub.add_parser("recall", help="Recall relevant memory")
    recall.add_argument("--context", action="append", required=True, help="Context phrase (repeatable)")
    recall.add_argument("--limit", type=int, default=5)
    recall.add_argument("--mode", default="semantic")
    recall.add_argument(
        "--tags-all",
        action="append",
        default=[],
        help="Require every listed tag. Repeat for multiple tags.",
    )
    recall.add_argument(
        "--tags-any",
        action="append",
        default=[],
        help="Require at least one listed tag. Repeat for multiple tags.",
    )
    recall.add_argument(
        "--tag-filter-json",
        help="Advanced Muninn tag_filter object as JSON, for key-prefix or lexical-bound filters.",
    )
    recall.add_argument(
        "--context-packet",
        action="store_true",
        help="Project recall results into a cross-agent ContextPacket with Muninn continuity authority.",
    )

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


def resolve_token(args: argparse.Namespace) -> Optional[str]:
    token = args.token or os.environ.get("MUNINN_MCP_TOKEN")
    if token:
        return token.strip()
    token_file = pathlib.Path(args.token_file).expanduser()
    if token_file.exists():
        value = token_file.read_text(encoding="utf-8").strip()
        return value or None
    return None


def extract_tool_names(result: dict) -> List[str]:
    tools = (
        result.get("result", {})
        .get("tools", [])
    )
    names: List[str] = []
    for tool in tools:
        name = tool.get("name")
        if isinstance(name, str) and name:
            names.append(name)
    return names


def health_payload(base_url: str, token: Optional[str] = None) -> dict:
    payload = {
        "base_url": base_url,
        "reachable": False,
        "required_tools_present": False,
        "missing_tools": [],
        "available_tools": [],
        "approval_required": False,
        "status": "unreachable",
    }

    client = MuninnMcpClient(base_url, token)
    try:
        client.connect()
        payload["reachable"] = True
        tools_result = client.tools_list()
        names = extract_tool_names(tools_result)
        payload["available_tools"] = names
        missing = [tool for tool in REQUIRED_TOOLS if tool not in names]
        payload["missing_tools"] = missing
        payload["required_tools_present"] = not missing
        payload["status"] = "ready" if not missing else "missing_tools"
        payload["approval_required"] = bool(missing)
        return payload
    except Exception as exc:  # noqa: BLE001 - helper should surface hard failure plainly
        payload["error"] = str(exc)
        payload["approval_required"] = True
        return payload
    finally:
        client.close()


def emit_approval_required(payload: dict) -> int:
    message = (
        "MUNINN BLOCKER: Muninn MCP is unavailable or incomplete. "
        "Operator approval is required before continuing without memory."
    )
    print(message, file=sys.stderr)
    print(json.dumps(payload, indent=2, sort_keys=True), file=sys.stderr)
    return APPROVAL_REQUIRED_EXIT


def resolve_local_server_dir() -> Optional[pathlib.Path]:
    env_dir = os.environ.get("MUNINN_SERVER_DIR")
    candidates = []
    if env_dir:
        candidates.append(pathlib.Path(env_dir).expanduser())
    candidates.append(DEFAULT_MUNINN_DIR)

    for candidate in candidates:
        binary = candidate / "muninndb-server"
        if binary.exists():
            return candidate
    return None


def resolve_muninn_cli() -> Optional[pathlib.Path]:
    env_cli = os.environ.get("MUNINN_CLI")
    if env_cli:
        candidate = pathlib.Path(env_cli).expanduser()
        if candidate.exists():
            return candidate

    found = shutil.which("muninn")
    if found:
        return pathlib.Path(found)
    return None


def try_start_local_server() -> dict:
    cli = resolve_muninn_cli()
    if cli is not None:
        try:
            proc = subprocess.run(  # noqa: S603
                [str(cli), "start"],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=20,
                check=False,
            )
        except Exception as exc:  # noqa: BLE001 - bootstrap should report the real failure plainly
            return {
                "started": False,
                "start_attempted": True,
                "start_method": "muninn start",
                "start_reason": f"failed to start local muninn CLI: {exc}",
                "server_binary": str(cli),
            }

        if proc.returncode != 0:
            return {
                "started": False,
                "start_attempted": True,
                "start_method": "muninn start",
                "start_reason": proc.stderr.strip() or proc.stdout.strip() or f"exit {proc.returncode}",
                "server_binary": str(cli),
            }

        time.sleep(1.0)
        return {
            "started": True,
            "start_attempted": True,
            "start_method": "muninn start",
            "server_binary": str(cli),
            "start_output": proc.stdout.strip(),
        }

    server_dir = resolve_local_server_dir()
    if server_dir is None:
        return {
            "started": False,
            "start_attempted": False,
            "start_reason": "local muninn CLI or muninndb-server binary not found",
        }

    binary = server_dir / "muninndb-server"
    try:
        subprocess.Popen(  # noqa: S603
            [str(binary), "--daemon"],
            cwd=server_dir,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
    except Exception as exc:  # noqa: BLE001 - bootstrap should report the real failure plainly
        return {
            "started": False,
            "start_attempted": True,
            "start_reason": f"failed to start local muninndb-server: {exc}",
            "server_dir": str(server_dir),
            "server_binary": str(binary),
        }

    time.sleep(1.0)
    return {
        "started": True,
        "start_attempted": True,
        "server_dir": str(server_dir),
        "server_binary": str(binary),
    }


def _result_text_json(result: dict) -> Optional[dict]:
    content = result.get("result", {}).get("content", [])
    if not content:
        return None
    text = content[0].get("text") if isinstance(content[0], dict) else None
    if not isinstance(text, str) or not text.strip():
        return None
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError:
        return None
    return parsed if isinstance(parsed, dict) else None


def _memory_summary(memory: dict) -> Optional[str]:
    for key in ("summary", "concept", "content"):
        value = memory.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return None


def muninn_context_packet(recall_payload: dict, contexts: list[str]) -> dict:
    memories = recall_payload.get("memories", [])
    if not isinstance(memories, list):
        memories = []
    query_text = " | ".join(contexts)
    query_hash = hashlib.sha256(query_text.encode("utf-8")).hexdigest()[:16]
    generated_at = datetime.datetime.now(datetime.UTC).replace(microsecond=0).isoformat().replace("+00:00", "Z")
    refs = []

    for memory in memories:
        if not isinstance(memory, dict):
            continue
        memory_id = memory.get("id")
        if not isinstance(memory_id, str) or not memory_id.strip():
            continue
        refs.append(
            {
                "ref_id": memory_id,
                "kind": "muninn_engram",
                "authority": "muninn_continuity",
                "summary": _memory_summary(memory),
                "metadata": {
                    "concept": memory.get("concept"),
                    "score": memory.get("score"),
                    "trust": memory.get("trust"),
                    "state": memory.get("state"),
                    "source": "muninn_recall",
                },
            }
        )

    ref_ids = [ref["ref_id"] for ref in refs]
    return {
        "packet_id": f"context:muninn:{query_hash}",
        "generated_at": generated_at,
        "query_id": f"muninn:recall:{query_hash}",
        "summary": f"Muninn recall for {query_text}" if query_text else "Muninn recall",
        "refs": refs,
        "sections": [
            {
                "title": "Muninn recall",
                "authority": "muninn_continuity",
                "ref_ids": ref_ids,
            }
        ],
        "policy_notes": [
            "Muninn refs are continuity memory, not confirmed LifeGraph truth.",
            "Promote life-relevant claims through LifeGraph evidence/governance before treating them as structured life truth.",
        ],
        "metadata": {
            "source": "muninn_recall",
            "total": recall_payload.get("total"),
        },
    }


def attach_muninn_context_packet(result: dict, contexts: list[str]) -> dict:
    parsed = _result_text_json(result)
    if parsed is None:
        return result
    parsed["cross_agent_context_packet"] = muninn_context_packet(parsed, contexts)
    result = dict(result)
    result_payload = dict(result.get("result", {}))
    content = list(result_payload.get("content", []))
    if content and isinstance(content[0], dict):
        first = dict(content[0])
        first["text"] = json.dumps(parsed, separators=(",", ":"))
        content[0] = first
        result_payload["content"] = content
        result["result"] = result_payload
    return result


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    token = resolve_token(args)

    if args.command == "health":
        payload = health_payload(args.base_url, token)
        json.dump(payload, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 0 if payload["status"] == "ready" else 1

    if args.command == "bootstrap":
        payload = health_payload(args.base_url, token)
        if payload["status"] == "ready":
            payload["start_attempted"] = False
            payload["started"] = False
            json.dump(payload, sys.stdout, indent=2, sort_keys=True)
            sys.stdout.write("\n")
            return 0

        start_info = try_start_local_server()
        if start_info.get("start_attempted"):
            payload.update(start_info)
            payload["status_before_start"] = payload["status"]

        retry_payload = health_payload(args.base_url, token)
        retry_payload.update(start_info)
        if start_info.get("start_attempted"):
            retry_payload["status_before_start"] = payload["status"]
        if retry_payload["status"] != "ready":
            return emit_approval_required(retry_payload)
        json.dump(retry_payload, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 0

    if args.command == "require":
        payload = health_payload(args.base_url, token)
        if payload["status"] != "ready":
            return emit_approval_required(payload)
        json.dump(payload, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 0

    client = MuninnMcpClient(args.base_url, token, request_timeout=args.timeout)
    client.connect()

    if args.command == "tools":
        result = client.tools_list()
    elif args.command == "where-left-off":
        result = client.call_tool("muninn_where_left_off", {"limit": args.limit})
    elif args.command == "recall":
        payload = {"context": args.context, "limit": args.limit, "mode": args.mode}
        if args.tags_all:
            payload["tags_all"] = args.tags_all
        if args.tags_any:
            payload["tags_any"] = args.tags_any
        if args.tag_filter_json:
            payload["tag_filter"] = json.loads(args.tag_filter_json)
        result = client.call_tool(
            "muninn_recall",
            payload,
        )
        if args.context_packet:
            result = attach_muninn_context_packet(result, args.context)
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
