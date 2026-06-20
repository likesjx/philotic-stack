#!/usr/bin/env python3
"""Bridge graphify tree-sitter call edges into the intel-graph.

Matches graphify function nodes to intel-graph function nodes by
(file_path, function name) and upserts `calls` edges via the REST API,
so graph_impact blast-radius analysis reflects real call paths.

Idempotent: edges are upserted by (source, target, relation).
Re-run after `just graphify-update` or a graph rescan:

    just graphify-update && python3 scripts/graphify_bridge.py
"""

import json
import re
import sys
import urllib.request
from collections import defaultdict
from pathlib import Path

INTEL_API = "http://127.0.0.1:8900/api"
GRAPHIFY_JSON = Path(__file__).resolve().parent.parent / "graphify-out" / "graph.json"


def get_json(url: str):
    with urllib.request.urlopen(url, timeout=30) as resp:
        return json.load(resp)


def post_json(url: str, body: dict):
    req = urllib.request.Request(
        url,
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.load(resp)


def graphify_fn_name(label: str) -> str:
    # graphify labels look like ".handle_observe()", "main()", "agent_id"
    return label.lstrip(".").removesuffix("()")


def normalize_path(source_file: str) -> str:
    # graphify paths are repo-relative ("crates/philote/src/runtime.rs");
    # intel-graph file_path drops the crates/ prefix ("philote/src/runtime.rs")
    return source_file.removeprefix("crates/")


def main() -> int:
    if not GRAPHIFY_JSON.exists():
        print(f"missing {GRAPHIFY_JSON} — run `just graphify-update` first", file=sys.stderr)
        return 1

    # Index intel-graph functions by (file_path, name); skip ambiguous keys
    functions = get_json(f"{INTEL_API}/nodes?kind=function")
    intel_index: dict[tuple[str, str], list[str]] = defaultdict(list)
    for fn in functions:
        key = (fn.get("file_path") or "", fn["name"])
        intel_index[key].append(fn["id"])
    ambiguous = {k for k, v in intel_index.items() if len(v) > 1}

    graph = json.loads(GRAPHIFY_JSON.read_text())
    nodes_by_id = {n["id"]: n for n in graph["nodes"]}

    def resolve(graphify_id: str) -> str | None:
        node = nodes_by_id.get(graphify_id)
        if node is None:
            return None
        label = node.get("norm_label") or node.get("label") or ""
        if not re.fullmatch(r"\.?\w+(\(\))?", label):
            return None  # not a plain function/method label
        key = (normalize_path(node.get("source_file") or ""), graphify_fn_name(label))
        if key in ambiguous:
            return None
        ids = intel_index.get(key)
        return ids[0] if ids else None

    stats = {"call_edges": 0, "matched": 0, "ambiguous_skipped": 0, "upserted": 0, "failed": 0}
    seen: set[tuple[str, str]] = set()

    for edge in graph["links"]:
        if edge.get("relation") != "calls":
            continue
        stats["call_edges"] += 1
        source_id = resolve(edge["source"])
        target_id = resolve(edge["target"])
        if source_id is None or target_id is None:
            continue
        if source_id == target_id or (source_id, target_id) in seen:
            continue
        seen.add((source_id, target_id))
        stats["matched"] += 1
        body = {
            "source_id": source_id,
            "target_id": target_id,
            "relation": "calls",
            "properties": {
                "origin": "graphify",
                "confidence": edge.get("confidence", "EXTRACTED"),
                "call_site": f"{edge.get('source_file', '')}:{edge.get('source_location', '')}",
            },
        }
        try:
            post_json(f"{INTEL_API}/edges", body)
            stats["upserted"] += 1
        except Exception as exc:  # noqa: BLE001
            stats["failed"] += 1
            if stats["failed"] <= 3:
                print(f"failed: {source_id} -> {target_id}: {exc}", file=sys.stderr)

    stats["ambiguous_skipped"] = len(ambiguous)
    print(json.dumps(stats, indent=2))
    return 0 if stats["failed"] == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
