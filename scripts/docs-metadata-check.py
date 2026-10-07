#!/usr/bin/env python3
from __future__ import annotations

from pathlib import Path
import argparse
import re
import sys


ROOT = Path(__file__).resolve().parent.parent

PROPOSAL_KEYS = {
    "title",
    "doc_type",
    "domain",
    "status",
    "last_updated",
    "tags",
    "related_docs",
    "task_refs",
    "proposal_id",
    "active_seams",
    "source_of_truth_targets",
}

REQUIRED_DOCS = {
    "docs/architecture/README.md": {
        "title",
        "doc_type",
        "domain",
        "status",
        "last_updated",
        "tags",
        "related_docs",
    },
    "docs/architecture/DOMAIN_MAP.md": {
        "title",
        "doc_type",
        "domain",
        "status",
        "last_updated",
        "tags",
        "related_docs",
        "task_refs",
    },
    "docs/architecture/SEAM_REGISTRY.md": {
        "title",
        "doc_type",
        "domain",
        "status",
        "last_updated",
        "tags",
        "related_docs",
        "task_refs",
    },
    "docs/architecture/ARCHITECTURE_STATUS.md": {
        "title",
        "doc_type",
        "domain",
        "status",
        "last_updated",
        "tags",
        "related_docs",
        "task_refs",
        "tracks_domains",
    },
    "docs/architecture/ARCHITECTURE.md": {
        "title",
        "doc_type",
        "domain",
        "status",
        "last_updated",
        "tags",
        "related_docs",
        "task_refs",
        "tracks_domains",
    },
    "docs/architecture/OUTBOUND_INTEGRATIONS.md": {
        "title",
        "doc_type",
        "domain",
        "status",
        "last_updated",
        "tags",
        "related_docs",
        "task_refs",
        "tracks_domains",
    },
    "docs/architecture/OUTBOUND_EGRESS_INVENTORY.md": {
        "title",
        "doc_type",
        "domain",
        "status",
        "last_updated",
        "tags",
        "related_docs",
        "task_refs",
        "seam_id",
        "proposal_refs",
        "source_of_truth_targets",
        "verification_level",
    },
    "docs/architecture/AGENT_LOOP_RESEARCH.md": {
        "title",
        "doc_type",
        "domain",
        "status",
        "last_updated",
        "tags",
        "related_docs",
        "task_refs",
    },
    "docs/architecture/DOC_TAGGING_FRONTMATTER_PROPOSAL.md": {
        "title",
        "doc_type",
        "domain",
        "status",
        "last_updated",
        "tags",
        "related_docs",
        "task_refs",
        "proposal_id",
    },
    "docs/architecture/AGENT_LOOP_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/AGENT_INCARNATION_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/AGENT_PLUGIN_HOOKS_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/AGENT_WORKFLOW_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/APPROVAL_UX_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/DEV_ENGINE_OPTIMIZATION_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/GUEST_BINARY_RESOLUTION_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/HOMEBREW_DISTRIBUTION_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/HOTEL_PERIMETER_TRUST_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/INTER_HOTEL_ROUTING_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/KEY_VAULT_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/LOCAL_ADMIN_FALLBACK_MODEL_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/MEMORY_ENGINE_ABSTRACTION_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/MODEL_CONTROLLER_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/MUNINN_MEMORY_PROTOCOL_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/OPENCLAW_PARITY_MIGRATION_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/PERIMETER_EGRESS_CONTROL_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/PERSONALITY_AND_CONTEXT_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/PHILOTIC_AGENT_LOOP_SPEC.md": {
        "title",
        "doc_type",
        "domain",
        "status",
        "last_updated",
        "tags",
        "related_docs",
        "task_refs",
    },
    "docs/architecture/PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/PORT_BLUEPRINT.md": {
        "title",
        "doc_type",
        "domain",
        "status",
        "last_updated",
        "tags",
        "related_docs",
    },
    "docs/architecture/PROPOSAL_ORGANIZATION_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/ROLE_POSTURE_AND_ADMIN_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/ROUTER_NATIVE_OBSERVABILITY_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/SESSION_LOOP_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/TASK_RUNNER_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/TELEGRAM_INTEGRATION_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/TELEGRAM_POLL_LEASE_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/TOOL_MANAGEMENT_PLANE_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/VOICE_MACHINE_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/architecture/ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md": PROPOSAL_KEYS,
    "docs/ARCHITECT_THOUGHTS_CONTEXT_GRAPH.md": {
        "title",
        "doc_type",
        "domain",
        "status",
        "last_updated",
        "tags",
        "related_docs",
    },
    "docs/PHILOTIC-ARCHITECTURE.md": {
        "title",
        "doc_type",
        "domain",
        "status",
        "last_updated",
        "tags",
        "related_docs",
    },
    "docs/walkthrough.md": {
        "title",
        "doc_type",
        "domain",
        "status",
        "last_updated",
        "tags",
        "related_docs",
    },
}


# Proposal lifecycle states (docs/DOCUMENTATION_LIFECYCLE.md). `active` and
# `historical` are kept because live docs use them; spelling variants such as
# `in_progress` are normalized in the docs, not accepted here.
PROPOSAL_STATUSES = {
    "proposed",
    "accepted",
    "accepted-current-slice",
    "in-progress",
    "implemented",
    "verified",
    "architecture",
    "superseded",
    "deferred",
    "archived",
    "active",
    "historical",
}

# Rough maturity order used only to spot a clearly contradictory
# status/disposition pair (two or more steps apart). Terminal states such as
# superseded/deferred/archived/historical are not ranked and never warn.
STATUS_RANK = {
    "proposed": 0,
    "accepted": 1,
    "accepted-current-slice": 1,
    "in-progress": 2,
    "active": 2,
    "implemented": 3,
    "verified": 4,
    "architecture": 5,
}

DISPOSITION_SPELLINGS = {
    "in_progress": "in-progress",
    "accepted for current slice": "accepted-current-slice",
    "accepted_current_slice": "accepted-current-slice",
}

# docs/DEFECTS.md Status column vocabulary (watch-live burn-down W0).
DEFECT_STATUSES = {
    "open",
    "partial",
    "fixed (deploy pending)",
    "fixed (live pending)",
    "fixed (verified)",
    "resolved",
    "wontfix",
}

DEFECT_STATUS_COLUMN = 3  # | ID | Title | Severity | Status | Pts | Found | Fixed by |


def _clean_value(value: str) -> str:
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "'\"":
        value = value[1:-1].strip()
    return value


def check_proposal_statuses(arch_dir: Path) -> tuple[list[str], list[str]]:
    """Return (errors, warnings) for docs/architecture/*_PROPOSAL.md frontmatter."""
    errors: list[str] = []
    warnings: list[str] = []
    for path in sorted(arch_dir.glob("*_PROPOSAL.md")):
        rel = path.name
        try:
            frontmatter = parse_frontmatter(path)
        except ValueError:
            # Docs without frontmatter are not status-checked here.
            continue
        status = _clean_value(frontmatter.get("status", ""))
        if status not in PROPOSAL_STATUSES:
            errors.append(
                f"{rel}: unknown status {status!r} "
                f"(allowed: {', '.join(sorted(PROPOSAL_STATUSES))})"
            )
        if "disposition" in frontmatter:
            disposition = _clean_value(frontmatter["disposition"])
            disposition = DISPOSITION_SPELLINGS.get(disposition, disposition)
            if status in STATUS_RANK and disposition in STATUS_RANK:
                if abs(STATUS_RANK[status] - STATUS_RANK[disposition]) >= 2:
                    warnings.append(
                        f"{rel}: status {status!r} contradicts disposition {disposition!r}"
                    )
    return errors, warnings


def split_table_row(line: str) -> list[str]:
    """Split a markdown table row on unescaped pipes."""
    cells = re.split(r"(?<!\\)\|", line.strip())
    return [cell.strip() for cell in cells[1:-1]]


def check_defect_statuses(path: Path) -> list[str]:
    errors: list[str] = []
    if not path.is_file():
        return errors
    for lineno, line in enumerate(path.read_text().splitlines(), start=1):
        if not line.startswith("| DEF-"):
            continue
        cells = split_table_row(line)
        if len(cells) <= DEFECT_STATUS_COLUMN:
            errors.append(f"{path.name}:{lineno}: malformed defect row")
            continue
        status = cells[DEFECT_STATUS_COLUMN]
        if status not in DEFECT_STATUSES:
            errors.append(
                f"{path.name}:{lineno}: {cells[0]} has status {status!r} outside the vocabulary"
            )
    return errors


def parse_frontmatter(path: Path) -> dict[str, str]:
    text = path.read_text()
    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        raise ValueError("missing opening frontmatter delimiter")

    end_idx = None
    for idx in range(1, len(lines)):
        if lines[idx].strip() == "---":
            end_idx = idx
            break
    if end_idx is None:
        raise ValueError("missing closing frontmatter delimiter")

    data: dict[str, str] = {}
    current_list_key: str | None = None
    for raw_line in lines[1:end_idx]:
        if not raw_line.strip():
            continue
        if raw_line.startswith("  - ") and current_list_key is not None:
            continue
        if raw_line.startswith("- ") and current_list_key is not None:
            continue

        if ":" not in raw_line:
            continue

        key, value = raw_line.split(":", 1)
        key = key.strip()
        value = value.strip()
        data[key] = value
        current_list_key = key if value == "" else None

    return data


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Check docs frontmatter and status vocabularies.")
    parser.add_argument(
        "--warn-only",
        action="store_true",
        help="report proposal-status and DEFECTS vocabulary violations as warnings "
        "(the required-keys checks stay fatal)",
    )
    parser.add_argument("--root", type=Path, default=ROOT, help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    root: Path = args.root

    failures: list[str] = []

    for rel_path, required_keys in REQUIRED_DOCS.items():
        path = root / rel_path
        if not path.is_file():
            failures.append(f"{rel_path}: missing file")
            continue

        try:
            frontmatter = parse_frontmatter(path)
        except ValueError as exc:
            failures.append(f"{rel_path}: {exc}")
            continue

        missing = sorted(required_keys - set(frontmatter))
        if missing:
            failures.append(f"{rel_path}: missing frontmatter keys: {', '.join(missing)}")

    vocab_errors, warnings = check_proposal_statuses(root / "docs" / "architecture")
    vocab_errors += check_defect_statuses(root / "docs" / "DEFECTS.md")

    for warning in warnings:
        print(f"warning: {warning}", file=sys.stderr)

    if args.warn_only:
        for error in vocab_errors:
            print(f"warning: {error}", file=sys.stderr)
    else:
        failures.extend(vocab_errors)

    if failures:
        for failure in failures:
            print(failure, file=sys.stderr)
        return 1

    print("docs metadata checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
