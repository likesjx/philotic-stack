"""Unit tests for scripts/philotic_ipc.py (DEF-209 framing).

Run: python3 -m unittest discover -s scripts/tests -p 'test_*.py'
"""

import json
import pathlib
import socket
import struct
import sys
import threading
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from philotic_ipc import Ipc, IpcError, OPERATOR_ROLE, response_payload, secret_ref_of  # noqa: E402


def _send(sock, obj):
    data = json.dumps(obj).encode()
    sock.sendall(struct.pack(">I", len(data)) + data)


def _recv(sock):
    header = b""
    while len(header) < 4:
        header += sock.recv(4 - len(header))
    (length,) = struct.unpack(">I", header)
    body = b""
    while len(body) < length:
        body += sock.recv(length - len(body))
    return json.loads(body)


class FakeHotel:
    """Pushes an unsolicited advert on connect, then answers each request with
    the reply returned by `handler(request)` (preceded by `noise` pushes)."""

    def __init__(self, handler, noise_per_reply=1):
        self.client, self.server = socket.socketpair()
        self.handler = handler
        self.noise = noise_per_reply
        self.requests = []
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.thread.start()

    def _run(self):
        _send(self.server, {"blob_endpoint": "http://127.0.0.1:1/upload"})
        try:
            while True:
                req = _recv(self.server)
                self.requests.append(req)
                for _ in range(self.noise):
                    _send(self.server, {"network_state": "online"})
                _send(self.server, self.handler(req))
        except (OSError, struct.error):
            pass


class IpcFramingTest(unittest.TestCase):
    def test_call_skips_unsolicited_frames(self):
        hotel = FakeHotel(lambda req: {"ok": True, "operation": req["operation"]})
        ipc = Ipc("unused", sock=hotel.client)
        reply = ipc.call("add_vault_entry", {"plaintext": "x"})
        self.assertEqual(reply, {"ok": True, "operation": "add_vault_entry"})
        # A second call gets its own reply, not one op late.
        reply2 = ipc.call("get_config", {"key": "k"})
        self.assertEqual(reply2["operation"], "get_config")
        ipc.close()

    def test_expect_keys_matches_non_standard_replies(self):
        hotel = FakeHotel(lambda req: {"mcp_routes_agent_id": "agent-beacon-01", "mcp_route_count": 1})
        ipc = Ipc("unused", sock=hotel.client)
        reply = ipc.call("update_mcp_routes", {}, expect_keys=("mcp_routes_agent_id",))
        self.assertEqual(reply["mcp_routes_agent_id"], "agent-beacon-01")
        ipc.close()

    def test_register_operator_uses_owner_prefixed_id_and_operator_role(self):
        hotel = FakeHotel(lambda req: {"ok": True})
        ipc = Ipc("unused", sock=hotel.client)
        ipc.register_operator("agent-beacon", "mcp-provisioner")
        req = hotel.requests[0]
        self.assertEqual(req["operation"], "register")
        self.assertEqual(req["payload"]["guest_id"], "agent-beacon:mcp-provisioner")
        self.assertEqual(req["payload"]["role"], OPERATOR_ROLE)
        self.assertNotEqual(req["payload"]["role"], "hotel.internal")
        ipc.close()

    def test_register_refusal_raises(self):
        hotel = FakeHotel(lambda req: {"ok": False, "code": "FORBIDDEN"})
        ipc = Ipc("unused", sock=hotel.client)
        with self.assertRaises(IpcError):
            ipc.register_operator("agent-beacon", "x")
        ipc.close()

    def test_gives_up_after_bounded_noise(self):
        hotel = FakeHotel(lambda req: {"ok": True}, noise_per_reply=60)
        ipc = Ipc("unused", sock=hotel.client)
        with self.assertRaises(IpcError):
            ipc.call("get_config", {})
        ipc.close()


class HelperTest(unittest.TestCase):
    def test_secret_ref_lookup(self):
        self.assertEqual(secret_ref_of({"ok": True, "secret_ref": "a"}), "a")
        self.assertEqual(secret_ref_of({"ok": True, "data": {"secret_ref": "b"}}), "b")
        self.assertEqual(secret_ref_of({"ok": True, "payload": {"secret_ref": "c"}}), "c")
        self.assertIsNone(secret_ref_of({"ok": True}))

    def test_response_payload_falls_back_to_frame(self):
        self.assertEqual(response_payload({"x": 1}), {"x": 1})


if __name__ == "__main__":
    unittest.main()
