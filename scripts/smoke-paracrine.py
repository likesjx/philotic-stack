#!/usr/bin/env python3
"""Paracrine delegation plane smoke — run against a LIVE hotel socket.

Usage: smoke-paracrine.py <hotel-socket> [specialist-role]
  e.g. scripts/smoke-paracrine.py ~/.philotic/bjork/aiua-mac-jane.sock
       sudo python3 scripts/smoke-paracrine.py /run/philotic/vps-jane.sock Chronos

Speaks 4-byte big-endian length-prefixed JSON IpcRequests over the hotel UDS.
Proves, against the *installed, supervised* runtime (DEF-085/086 + S2-S5):
  1. ParacrineEmit to a nonexistent role is REFUSED (SPECIALIST_UNAVAILABLE),
     never swallowed with success — a blocking philote trusts a success and
     parks its whole turn on the 660s whisper deadline.
  2. ParacrineEmit to the given real specialist role is ACCEPTED (delivered,
     or parked with materialization credibly triggered).
"""
import json
import socket
import struct
import sys
import time
import uuid

PASS = "\033[32mPASS\033[0m"
FAIL = "\033[31mFAIL\033[0m"


def frame(obj):
    data = json.dumps(obj).encode()
    return struct.pack(">I", len(data)) + data


def read_frame(sock):
    hdr = b""
    while len(hdr) < 4:
        chunk = sock.recv(4 - len(hdr))
        if not chunk:
            raise RuntimeError("socket closed")
        hdr += chunk
    (n,) = struct.unpack(">I", hdr)
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise RuntimeError("socket closed mid-frame")
        buf += chunk
    return json.loads(buf)


def wait_for_corr(sock, corr_id, timeout=10.0):
    """Read frames (skipping pushes) until a response with corr_id arrives."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            r = read_frame(sock)
        except socket.timeout:
            break
        if isinstance(r, dict) and r.get("corr_id") == corr_id:
            return r
    raise RuntimeError(f"no response with corr_id={corr_id} within {timeout}s")


def emit(sock, role):
    sock.sendall(
        frame(
            {
                "operation": "paracrine_emit",
                "payload": {
                    "role": role,
                    "exosome": {
                        "prompt": "smoke-paracrine liveness probe: reply briefly; "
                        "do not create cron jobs or graph writes.",
                        "context": None,
                        "paracrine_id": f"smoke-paracrine-{uuid.uuid4()}",
                        "response_routing": None,
                        "source_session_id": "smoke:paracrine",
                        "source_chat_id": None,
                    },
                    "reply_to_node": "smoke-local",
                    "reply_to_role": "agent",
                    "reply_to_guest_id": None,
                    "timeout_secs": None,
                },
            }
        )
    )
    return wait_for_corr(sock, "paracrine_emit")


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(2)
    sock_path = sys.argv[1]
    real_role = sys.argv[2] if len(sys.argv) > 2 else "Chronos"

    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(sock_path)
    s.settimeout(10)
    s.sendall(
        frame(
            {
                "operation": "register",
                "payload": {
                    "guest_id": f"smoke-paracrine-{uuid.uuid4().hex[:8]}",
                    "role": "smoke",
                    "supported_tools": [],
                },
            }
        )
    )
    wait_for_corr(s, "reg")

    failures = 0

    # 1. Bogus role must be refused, not swallowed.
    resp = emit(s, f"NoSuchRole-{uuid.uuid4().hex[:8]}")
    ok = resp.get("ok") is False and resp.get("code") == "SPECIALIST_UNAVAILABLE"
    print(f"{PASS if ok else FAIL} bogus role refused with SPECIALIST_UNAVAILABLE: {json.dumps(resp)[:140]}")
    failures += 0 if ok else 1

    # 2. Real specialist role must be accepted.
    resp = emit(s, real_role)
    ok = resp.get("ok") is True
    print(f"{PASS if ok else FAIL} whisper to '{real_role}' accepted: {json.dumps(resp)[:140]}")
    failures += 0 if ok else 1

    s.close()
    total = 2
    print(f"\n{total - failures}/{total} checks passed")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
