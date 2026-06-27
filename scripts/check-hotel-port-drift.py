#!/usr/bin/env python3
"""Compare Ansible peer ports with live hotel records in the context graph."""

from __future__ import annotations

import argparse
import json
import re
import shlex
import subprocess
import sys
from pathlib import Path


PEER_LIST_RE = re.compile(r"^philotic_backbone_peers:\s*$")
ITEM_RE = re.compile(r"^\s*-\s+name:\s*[\"']?([^\"']+)[\"']?\s*$")
FIELD_RE = re.compile(r"^\s+(host|beacon_port|blob_port|execution_port):\s*[\"']?([^\"'#]+)[\"']?\s*(?:#.*)?$")


def parse_peer_ports(path: Path) -> dict[str, dict[str, str | int]]:
    peers: dict[str, dict[str, str | int]] = {}
    in_peers = False
    current_name: str | None = None

    for raw_line in path.read_text().splitlines():
        if PEER_LIST_RE.match(raw_line):
            in_peers = True
            continue
        if not in_peers:
            continue
        if raw_line and not raw_line.startswith(" ") and not raw_line.startswith("-"):
            break

        item = ITEM_RE.match(raw_line)
        if item:
            current_name = item.group(1)
            peers[current_name] = {}
            continue

        field = FIELD_RE.match(raw_line)
        if current_name and field:
            key, value = field.groups()
            if key.endswith("_port"):
                peers[current_name][key] = int(value)
            else:
                peers[current_name][key] = value.strip()

    return peers


def graph_hotels(ssh_target: str, db_path: str) -> dict[str, dict[str, str | int]]:
    sql = (
        "select substr(node_key, 7) as hotel_name, "
        "json_extract(data_json, '$.mesh_host') as host, "
        "json_extract(data_json, '$.mesh_port') as mesh_port, "
        "json_extract(data_json, '$.blob_port') as blob_port, "
        "json_extract(data_json, '$.execution_port') as execution_port "
        "from graph_nodes where node_key like 'hotel:%';"
    )
    remote = " ".join(["sudo", "sqlite3", "-json", shlex.quote(db_path), shlex.quote(sql)])
    cmd = ["ssh", ssh_target, remote]
    result = subprocess.run(cmd, check=True, capture_output=True, text=True)
    rows = json.loads(result.stdout or "[]")
    return {
        row["hotel_name"]: {
            "host": row["host"],
            "beacon_port": int(row["mesh_port"]),
            "blob_port": int(row["blob_port"]),
            "execution_port": int(row["execution_port"]),
        }
        for row in rows
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--host-vars",
        type=Path,
        default=Path("ansible/host_vars/jane-vps.yml"),
        help="Ansible host_vars file to compare",
    )
    parser.add_argument("--ssh-target", default="vps-jane", help="SSH host for the live graph")
    parser.add_argument(
        "--db-path",
        default="/opt/philotic/data/aiua_context.db",
        help="Remote context graph SQLite path",
    )
    args = parser.parse_args()

    configured = parse_peer_ports(args.host_vars)
    live = graph_hotels(args.ssh_target, args.db_path)

    mismatches: list[str] = []
    for name, peer in configured.items():
        actual = live.get(name)
        if not actual:
            mismatches.append(f"{name}: present in {args.host_vars} but missing from live graph")
            continue
        for key in ("host", "beacon_port", "blob_port", "execution_port"):
            expected = peer.get(key)
            observed = actual.get(key)
            if expected != observed:
                mismatches.append(f"{name}.{key}: host_vars={expected!r} graph={observed!r}")

    if mismatches:
        print("Port drift detected between Ansible host_vars and the live context graph:")
        for mismatch in mismatches:
            print(f"  - {mismatch}")
        return 1

    print(f"Port drift check passed for {args.host_vars} against {args.ssh_target}.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
