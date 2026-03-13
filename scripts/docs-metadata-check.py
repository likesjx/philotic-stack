#!/usr/bin/env python3
from __future__ import annotations

from pathlib import Path
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


def main() -> int:
    failures: list[str] = []

    for rel_path, required_keys in REQUIRED_DOCS.items():
        path = ROOT / rel_path
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

    if failures:
        for failure in failures:
            print(failure, file=sys.stderr)
        return 1

    print("docs metadata checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
