#!/usr/bin/env python3
"""Isolation repro for life.observe.batch (the handoff's decisive test).

Drives a multi-item life.observe.batch straight at the life-graph-runner over
the hotel IPC socket, bypassing philote / the model / the 90s WaitingTool
watchdog. Times the batch end to end and reports every item's outcome.

Speaks the hotel UDS wire protocol directly (4-byte big-endian length prefix +
JSON; `IpcRequest` serializes as {"operation":..,"payload":..}), so it needs no
Rust build — useful on vps-jane, where binaries only arrive via an ~11-minute CI
run. It is the Python twin of
`crates/philotic-client/examples/life_graph_ipc_smoke_driver.rs`.

Usage — local leg (runner on the same host):

    sudo -u philotic PROBE_N=25 python3 scripts/lifegraph_batch_probe.py

Usage — cross-mesh leg (a Mac driving the vps runner; reply must come back to
the LOCAL node, otherwise the response has nowhere to land):

    PHILOTIC_HOTEL_SOCKET=~/.philotic/bjork/aiua-mac-jane.sock \
    PHILOTIC_TARGET_NODE=vps-jane-aiua-01 \
    PHILOTIC_REPLY_NODE=mac-jane-aiua-01 \
    PROBE_N=6 PROBE_BAD_EDGE_AT=2 python3 scripts/lifegraph_batch_probe.py

`PROBE_BAD_EDGE_AT=<i>` gives item i a non-living-cycle `rel_type`, proving the
per-item isolation guarantee (that item errors, the rest still write, overall
status is `partial`).

IMPORTANT — this writes real nodes to the operator's live LifeGraph. They are
id-prefixed `batch-probe-`; always delete them afterward and confirm the node
count returns to its pre-run value:

  MATCH (n) WHERE n.id STARTS WITH 'batch-probe-' DETACH DELETE n;
"""
import json
import os
import socket
import struct
import sys
import time
import uuid

SOCK = os.environ.get("PHILOTIC_HOTEL_SOCKET", "/run/philotic/vps-jane.sock")
TARGET_NODE = os.environ.get("PHILOTIC_TARGET_NODE", "vps-jane-aiua-01")
# Where the runner sends the datasource_response. For a cross-hotel run this is
# the LOCAL node, so the reply travels back over the mesh leg being tested.
REPLY_NODE = os.environ.get("PHILOTIC_REPLY_NODE", TARGET_NODE)
# Index of an item given a deliberately invalid (non-living-cycle) rel_type, to
# prove one bad item is isolated and never aborts the rest of the batch.
BAD_EDGE_AT = os.environ.get("PROBE_BAD_EDGE_AT")
GUEST = "life-graph-batch-probe"
ROLE = "life-graph.batch.probe.reply"
N = int(os.environ.get("PROBE_N", "12"))
# PROBE_STRING_ENCODE=1 sends each observation as a JSON *string* instead of an
# object — the shape models actually emit for nested tool arguments, and the one
# that used to make plain serde derive reject the WHOLE batch with
# `invalid type: string ..., expected struct LifeObserveInput`. Every
# observation was discarded despite being valid. Use this to prove a deployed
# runner carries the lenient deserializer: same items, same expected outcome as
# a normal run.
STRING_ENCODE = os.environ.get("PROBE_STRING_ENCODE") == "1"
TOOL = os.environ.get("PROBE_TOOL", "life.observe.batch")
WAIT = float(os.environ.get("PROBE_WAIT", "300"))


class Ipc:
    def __init__(self, path):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.connect(path)
        self.buf = b""

    def send(self, operation, payload):
        frame = json.dumps({"operation": operation, "payload": payload}).encode()
        self.s.sendall(struct.pack(">I", len(frame)) + frame)

    def read_frame(self, deadline):
        while True:
            if len(self.buf) >= 4:
                (ln,) = struct.unpack(">I", self.buf[:4])
                if len(self.buf) >= 4 + ln:
                    payload = self.buf[4 : 4 + ln]
                    self.buf = self.buf[4 + ln :]
                    return json.loads(payload)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("no IPC frame before deadline")
            self.s.settimeout(remaining)
            chunk = self.s.recv(65536)
            if not chunk:
                raise RuntimeError("IPC stream closed")
            self.buf += chunk


def observation(i):
    """One observation item, matching LifeObserveInput exactly."""
    node_id = f"batch-probe-{RUN}-{i}"
    edges = []
    if BAD_EDGE_AT is not None and int(BAD_EDGE_AT) == i:
        # 'ADVANCES' is not one of OWNS/SHAPES/SETS/SPAWNS/RELATES_TO/SCOPED_TO,
        # so plan_observe must reject this item pre-write.
        edges = [{"rel_type": "ADVANCES", "target_id": "some-target", "upsert_target": False}]
    return {
        "edges": edges,
        "observation_id": f"obs-{RUN}-{i}",
        "evidence": {
            "packet_id": f"pkt-{RUN}-{i}",
            "claim_ref": {"id": node_id, "label": "Signal"},
            "claim_summary": (
                f"Batch isolation probe item {i}: diagnostic write verifying "
                f"life.observe.batch end-to-end latency and per-item outcome."
            ),
            "source_refs": [
                {
                    "source_id": GUEST,
                    "source_kind": "runtime_observation",
                    "reliability": {"score": 0.95, "basis": "direct_observation"},
                }
            ],
            "passage_refs": [],
            "confidence": 0.9,
            "validation_state": "proposed",
            "observed_at": "2026-07-26T00:00:00Z",
            "source_reliability": 0.95,
            "conflict_ids": [],
            "adjudication_status": "not_needed",
            "metadata": {"probe": True, "route": "hotel_ipc"},
        },
        "proposed_graph_refs": [],
    }


RUN = uuid.uuid4().hex[:8]

ipc = Ipc(SOCK)
ipc.send("register", {"guest_id": GUEST, "role": ROLE, "supported_tools": []})
ipc.send("subscribe_inbox", {"role": ROLE})

turn_id = f"probe-turn-{RUN}"
if TOOL == "life.observe.batch":
    items = [observation(i) for i in range(N)]
    if STRING_ENCODE:
        items = [json.dumps(item) for item in items]
    arguments = {"observations": items}
else:
    arguments = observation(0)

task = {
    "action": "execute_tool",
    "tool_name": TOOL,
    "arguments": arguments,
    "session_id": f"probe:life-graph:{TOOL}:{RUN}",
    "turn_id": turn_id,
    "chat_id": "probe-chat",
    "agent_id": GUEST,
    "reply_to": REPLY_NODE,
    "reply_role": ROLE,
}

print(f"[probe] tool={TOOL} items={N if TOOL.endswith('batch') else 1} run={RUN} "
      f"target={TARGET_NODE} reply={REPLY_NODE}"
      + (f" bad_edge_at={BAD_EDGE_AT}" if BAD_EDGE_AT is not None else ""))
started = time.monotonic()
ipc.send(
    "emit_task",
    {
        "target_node": TARGET_NODE,
        "target_role": "life-graph-runner",
        "target_guest_id": None,
        "task_json": json.dumps(task),
    },
)

deadline = started + WAIT
result = None
while True:
    try:
        frame = ipc.read_frame(deadline)
    except TimeoutError:
        print(f"[probe] NO RESPONSE after {time.monotonic() - started:.1f}s — HUNG")
        sys.exit(2)
    inbound = frame.get("InboundTask") or (
        frame.get("payload") if frame.get("response") == "inbound_task" else None
    )
    if isinstance(frame, dict) and "task_json" in frame:
        inbound = frame
    if not inbound or "task_json" not in inbound:
        continue
    payload = json.loads(inbound["task_json"])
    if payload.get("action") != "datasource_response":
        continue
    if payload.get("turn_id") != turn_id and payload.get("capability") != TOOL:
        continue
    result = payload
    break

elapsed = time.monotonic() - started
print(f"[probe] responded in {elapsed:.2f}s")

if result.get("error"):
    print(f"[probe] ERROR: {json.dumps(result['error'])[:2000]}")
    sys.exit(3)

data = result.get("result", {}).get("data", {})
print(f"[probe] status={data.get('status')} requested={data.get('requested')} "
      f"succeeded={data.get('succeeded')} failed={data.get('failed')}")
if TOOL.endswith("batch"):
    per_item = elapsed / max(1, data.get("requested") or N)
    print(f"[probe] per-item wall time ≈ {per_item:.2f}s "
          f"→ projected 25-item batch ≈ {per_item * 25:.1f}s (watchdog is 90s)")
    for row in (data.get("results") or [])[:30]:
        r = row.get("result", {})
        status = r.get("status")
        detail = "" if status == "proposed" else f"  {json.dumps(r)[:400]}"
        print(f"  item {row.get('index'):>2} status={status} embed={r.get('embed_status')}{detail}")
else:
    print(f"[probe] {json.dumps(data)[:1200]}")
