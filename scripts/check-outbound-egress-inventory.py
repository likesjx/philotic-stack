#!/usr/bin/env python3
"""Fail when a production Rust direct network caller lacks an egress disposition."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
INVENTORY = ROOT / "docs/architecture/outbound-egress-inventory.json"
DIRECT_CLIENT = re.compile(
    r"(?<![A-Za-z0-9_])"
    r"(?:(?:reqwest::)?Client::(?:new|builder)|reqwest::get|connect_async)\s*\("
)
TRAFFIC_CLASSES = {
    "general-api",
    "model-provider",
    "communication",
    "local-resource",
    "mesh-peer",
    "artifact",
}
DISPOSITIONS = {
    "controlled-boundary",
    "named-exception",
    "temporary-exception",
    "future-violation",
}


def discovered_callers() -> set[str]:
    callers: set[str] = set()
    for path in (ROOT / "crates").rglob("*.rs"):
        relative = path.relative_to(ROOT)
        if "tests" in relative.parts or "examples" in relative.parts:
            continue
        if DIRECT_CLIENT.search(path.read_text(encoding="utf-8")):
            callers.add(relative.as_posix())
    return callers


def fail(message: str) -> None:
    print(f"outbound-egress inventory: {message}", file=sys.stderr)


def main() -> int:
    payload = json.loads(INVENTORY.read_text(encoding="utf-8"))
    entries = payload.get("direct_callers", [])
    declared = {entry.get("path") for entry in entries}
    discovered = discovered_callers()
    errors = False

    missing = sorted(discovered - declared)
    stale = sorted(declared - discovered)
    if missing:
        fail("unclassified direct callers:\n  " + "\n  ".join(missing))
        errors = True
    if stale:
        fail("stale direct-caller entries:\n  " + "\n  ".join(stale))
        errors = True

    if len(declared) != len(entries):
        fail("duplicate direct-caller path")
        errors = True

    for entry in entries:
        path = entry.get("path", "<missing>")
        classes = set(entry.get("traffic_classes", []))
        unknown_classes = sorted(classes - TRAFFIC_CLASSES)
        if not classes or unknown_classes:
            fail(f"{path}: invalid traffic_classes {sorted(classes)}")
            errors = True
        if entry.get("disposition") not in DISPOSITIONS:
            fail(f"{path}: invalid disposition {entry.get('disposition')!r}")
            errors = True
        if not entry.get("authority") or not entry.get("note"):
            fail(f"{path}: authority and note are required")
            errors = True

    migrated = payload.get("migrated_callers", [])
    for entry in migrated:
        path = entry.get("path", "<missing>")
        if path in discovered:
            fail(f"{path}: migrated caller still constructs a direct network client")
            errors = True
        if not (ROOT / path).is_file():
            fail(f"{path}: migrated caller path does not exist")
            errors = True

    if errors:
        return 1
    print(
        "outbound-egress inventory clean: "
        f"{len(discovered)} direct callers classified, {len(migrated)} migrations guarded"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
