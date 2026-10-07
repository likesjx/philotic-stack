"""Shared hotel IPC client for operator scripts.

Frames are a 4-byte big-endian length followed by a JSON body
(`{"operation": <snake_case op>, "payload": {...}}`). The hotel also pushes
unsolicited frames on the same stream (for example a blob-endpoint advert right
after connect), so `call` reads until it sees the reply to its own operation:
a Standard envelope (`ok` / `code`) or one of the caller's `expect_keys`.

Scripts register as role `operator` with an owner-prefixed guest id
(`<owner_agent_id>:<suffix>`). The hotel's MCP owner check accepts that shape,
and the role keeps scripts off `hotel.internal`, which is reserved for the
hotel itself (PERIMETER_ENFORCEMENT_PROPOSAL.md P2; DEF-209).
"""

from __future__ import annotations

import json
import socket
import struct
from typing import Iterable

OPERATOR_ROLE = "operator"
MAX_FRAME_BYTES = 64 * 1024 * 1024
MAX_FRAMES_PER_CALL = 50


class IpcError(RuntimeError):
    pass


class Ipc:
    def __init__(self, path: str, timeout: float = 15.0, sock: socket.socket | None = None):
        if sock is None:
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            sock.settimeout(timeout)
            sock.connect(path)
        self.sock = sock

    def _read_exact(self, n: int) -> bytes:
        buf = b""
        while len(buf) < n:
            chunk = self.sock.recv(n - len(buf))
            if not chunk:
                raise IpcError("socket closed")
            buf += chunk
        return buf

    def send(self, operation: str, payload: dict) -> None:
        data = json.dumps({"operation": operation, "payload": payload}).encode()
        self.sock.sendall(struct.pack(">I", len(data)) + data)

    def read_frame(self) -> object:
        (length,) = struct.unpack(">I", self._read_exact(4))
        if length > MAX_FRAME_BYTES:
            raise IpcError(f"frame too large: {length} bytes")
        return json.loads(self._read_exact(length))

    def call(self, operation: str, payload: dict, expect_keys: Iterable[str] = ()) -> dict:
        self.send(operation, payload)
        keys = tuple(expect_keys)
        for _ in range(MAX_FRAMES_PER_CALL):
            frame = self.read_frame()
            if isinstance(frame, dict) and (
                "ok" in frame or "code" in frame or any(k in frame for k in keys)
            ):
                return frame
        raise IpcError(f"no reply to {operation} within {MAX_FRAMES_PER_CALL} frames")

    def register_operator(self, owner_agent_id: str, suffix: str) -> dict:
        """Register as `<owner>:<suffix>` with role `operator`; raises on refusal."""
        reply = self.call(
            "register",
            {
                "guest_id": f"{owner_agent_id}:{suffix}",
                "role": OPERATOR_ROLE,
                "supported_tools": [],
            },
        )
        if reply.get("ok") is not True:
            raise IpcError(f"register refused: {reply}")
        return reply

    def close(self) -> None:
        self.sock.close()

    def __enter__(self) -> "Ipc":
        return self

    def __exit__(self, *exc) -> None:
        self.close()


def response_payload(resp: dict) -> dict:
    payload = resp.get("payload")
    if isinstance(payload, dict):
        return payload
    data = resp.get("data")
    return data if isinstance(data, dict) else resp


def secret_ref_of(resp: dict) -> str | None:
    return resp.get("secret_ref") or response_payload(resp).get("secret_ref")
