#!/usr/bin/env python3

import importlib.util
import json
from pathlib import Path
import struct
import unittest


SCRIPT = Path(__file__).with_name("provision-knowledge-mcp.py")
SPEC = importlib.util.spec_from_file_location("provision_knowledge_mcp", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class KnowledgeMcpProvisioningTests(unittest.TestCase):
    def test_config_pins_server_and_projects_only_governed_tools(self):
        server = Path("/opt/philotic/obsidian_knowledge.py")
        config = MODULE.build_config(
            upstream_id="knowledge",
            owner_agent_id="astrid",
            python_command="/usr/bin/python3",
            server_script=server,
            updated_at=42,
        )

        self.assertEqual(
            config["transport"],
            {
                "kind": "stdio",
                "command": "/usr/bin/python3",
                "args": [str(server), "serve"],
            },
        )
        self.assertEqual(
            [grant["remote_name"] for grant in config["tool_allowlist"]],
            list(MODULE.TOOL_NAMES),
        )
        self.assertNotIn("bash", str(config))
        self.assertNotIn("knowledge.apply", str(config))
        self.assertEqual(config["updated_at"], 42)

    def test_allow_command_pins_both_script_and_serve_mode(self):
        command = MODULE.allow_command_text(
            "/usr/bin/python3", Path("/opt/philotic/obsidian_knowledge.py")
        )
        self.assertEqual(
            command,
            'phil mcp allow-command "/usr/bin/python3" '
            '--args-prefix "/opt/philotic/obsidian_knowledge.py" serve',
        )

    def test_failed_hotel_response_is_not_success(self):
        self.assertIn(
            "STDIO_NOT_ALLOWED",
            MODULE.response_error(
                {
                    "ok": False,
                    "message": "STDIO_NOT_ALLOWED: operator action required",
                }
            ),
        )
        self.assertIsNone(
            MODULE.response_error(
                {
                    "operation": "mcp_upstream_registered",
                    "payload": {"mcp_upstream_id": "knowledge"},
                }
            )
        )

    def test_ipc_call_skips_out_of_band_status_before_registration_reply(self):
        class FakeSocket:
            def __init__(self):
                frames = [
                    {"available": True, "endpoint": "http://127.0.0.1:8475"},
                    {
                        "mcp_upstream_id": "knowledge",
                        "mcp_upstream_materialized": True,
                    },
                ]
                self.data = b"".join(
                    struct.pack(">I", len(encoded)) + encoded
                    for encoded in [
                        json.dumps(frame).encode() for frame in frames
                    ]
                )
                self.sent = b""

            def sendall(self, data):
                self.sent += data

            def recv(self, count):
                chunk, self.data = self.data[:count], self.data[count:]
                return chunk

        sock = FakeSocket()
        response = MODULE.ipc_call(sock, "register_mcp_upstream", {"config": {}})
        self.assertEqual(response["mcp_upstream_id"], "knowledge")
        self.assertIn(b"register_mcp_upstream", sock.sent)


if __name__ == "__main__":
    unittest.main()
