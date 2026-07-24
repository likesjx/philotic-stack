#!/usr/bin/env python3
"""Governed Obsidian index and stdio MCP server."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sqlite3
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from urllib.parse import quote

DEFAULT_SCOPE = "Efforts/Ongoing"
MAX_READ_CHARS = 40_000
MAX_EXCERPT_CHARS = 1_500
ALLOWED_LINK_RELATIONS = {"ABOUT", "DEVELOPS", "RECORDS", "PRODUCES", "SUPPORTS"}
WIKILINK = re.compile(r"\[\[([^\]|#]+)(?:[|#][^\]]*)?\]\]")
MARKDOWN_LINK = re.compile(r"\[[^\]]+\]\(([^)]+\.md(?:#[^)]+)?)\)")
HEADING = re.compile(r"^(#{1,6})\s+(.+?)\s*$", re.MULTILINE)
INLINE_TAG = re.compile(r"(?<![\w/])#([A-Za-z0-9][A-Za-z0-9_/-]*)")
SYNC_STATE_LOCK = threading.Lock()
SYNC_STATE: dict[str, Any] = {
    "state": "not_started",
    "started_at": None,
    "completed_at": None,
    "error": None,
}


class KnowledgeError(ValueError):
    pass


def now_rfc3339() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def sha256_text(content: str) -> str:
    return f"sha256:{hashlib.sha256(content.encode()).hexdigest()}"


def deterministic_id(prefix: str, *parts: str) -> str:
    digest = hashlib.sha256("\0".join(parts).encode()).hexdigest()[:32]
    return f"{prefix}:{digest}"


def required(payload: dict[str, Any], key: str) -> str:
    value = str(payload.get(key, "")).strip()
    if not value:
        raise KnowledgeError(f"{key} is required")
    return value


def compact_json(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def loads(value: str | None, default):
    try:
        return json.loads(value or "")
    except json.JSONDecodeError:
        return default


def default_vault_root() -> Path:
    if configured := os.environ.get("PHILOTIC_OBSIDIAN_VAULT"):
        return Path(configured).expanduser().resolve()
    try:
        result = subprocess.run(
            ["obsidian-cli", "print-default"],
            check=True,
            capture_output=True,
            text=True,
            timeout=5,
        )
    except (FileNotFoundError, subprocess.SubprocessError) as exc:
        raise KnowledgeError(
            "set PHILOTIC_OBSIDIAN_VAULT or configure obsidian-cli"
        ) from exc
    for line in result.stdout.splitlines():
        if line.strip().lower().startswith("default vault path:"):
            return Path(line.split(":", 1)[1].strip()).expanduser().resolve()
    raise KnowledgeError("obsidian-cli did not report a default vault path")


def default_db_path() -> Path:
    if configured := os.environ.get("PHILOTIC_KNOWLEDGE_DB"):
        return Path(configured).expanduser().resolve()
    return Path("~/.local/share/philotic/knowledge-index.sqlite3").expanduser()


def settings(payload: dict[str, Any]) -> tuple[Path, Path, str, str]:
    vault = Path(payload.get("vault_root") or default_vault_root()).expanduser().resolve()
    if not vault.is_dir():
        raise KnowledgeError(f"vault_root does not exist: {vault}")
    database = Path(payload.get("db_path") or default_db_path()).expanduser().resolve()
    scope = str(
        payload.get("scope")
        or os.environ.get("PHILOTIC_OBSIDIAN_SCOPE")
        or DEFAULT_SCOPE
    ).strip().strip("/")
    scope_root = (vault / scope).resolve()
    if not scope_root.is_relative_to(vault):
        raise KnowledgeError("scope must remain inside vault_root")
    if not scope_root.is_dir():
        raise KnowledgeError(f"scope does not exist: {scope}")
    return vault, database, scope, str(payload.get("vault_id") or vault.name)


def connect(database: Path) -> sqlite3.Connection:
    database.parent.mkdir(parents=True, exist_ok=True)
    connection = sqlite3.connect(database)
    connection.row_factory = sqlite3.Row
    connection.execute("PRAGMA journal_mode=WAL")
    connection.executescript(
        """
        CREATE TABLE IF NOT EXISTS documents (
          document_id TEXT PRIMARY KEY, vault_id TEXT NOT NULL,
          relative_path TEXT NOT NULL, file_identity TEXT NOT NULL,
          content_hash TEXT NOT NULL, title TEXT NOT NULL,
          headings_json TEXT NOT NULL, tags_json TEXT NOT NULL,
          outbound_links_json TEXT NOT NULL, created_at TEXT NOT NULL,
          modified_at TEXT NOT NULL, indexed_at TEXT NOT NULL,
          provenance_json TEXT NOT NULL, tombstoned_at TEXT
        );
        CREATE UNIQUE INDEX IF NOT EXISTS documents_vault_path
          ON documents(vault_id, relative_path);
        CREATE INDEX IF NOT EXISTS documents_identity
          ON documents(vault_id, file_identity);
        CREATE INDEX IF NOT EXISTS documents_hash
          ON documents(vault_id, content_hash);
        CREATE TABLE IF NOT EXISTS document_lineage (
          lineage_id TEXT PRIMARY KEY, document_id TEXT NOT NULL,
          from_path TEXT NOT NULL, to_path TEXT NOT NULL,
          changed_at TEXT NOT NULL, reason TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS review_queue (
          proposal_id TEXT PRIMARY KEY, operation TEXT NOT NULL,
          document_id TEXT, target_ref TEXT, relation TEXT,
          payload_json TEXT NOT NULL, rationale TEXT NOT NULL,
          evidence TEXT NOT NULL, status TEXT NOT NULL,
          operator_approved INTEGER NOT NULL DEFAULT 0,
          created_at TEXT NOT NULL, reviewed_at TEXT
        );
        CREATE TABLE IF NOT EXISTS sync_runs (
          run_id TEXT PRIMARY KEY, vault_id TEXT NOT NULL, scope TEXT NOT NULL,
          started_at TEXT NOT NULL, completed_at TEXT NOT NULL,
          created_count INTEGER NOT NULL, updated_count INTEGER NOT NULL,
          renamed_count INTEGER NOT NULL, tombstoned_count INTEGER NOT NULL,
          unchanged_count INTEGER NOT NULL, duration_ms INTEGER NOT NULL
        );
        """
    )
    return connection


def parse_frontmatter(content: str) -> tuple[dict[str, Any], str]:
    if not content.startswith("---\n") or (end := content.find("\n---", 4)) == -1:
        return {}, content
    values: dict[str, Any] = {}
    for line in content[4:end].splitlines():
        if ":" not in line or line.startswith((" ", "\t")):
            continue
        key, raw = line.split(":", 1)
        raw = raw.strip()
        values[key.strip()] = (
            [v.strip().strip("\"'") for v in raw[1:-1].split(",") if v.strip()]
            if raw.startswith("[") and raw.endswith("]")
            else raw.strip("\"'")
        )
    return values, content[end + 4 :].lstrip("\n")


def parse_note(content: str, fallback_title: str) -> dict[str, Any]:
    frontmatter, body = parse_frontmatter(content)
    headings = [match.group(2).strip() for match in HEADING.finditer(body)]
    raw_tags = frontmatter.get("tags", [])
    if isinstance(raw_tags, str):
        raw_tags = [value.strip() for value in raw_tags.split(",") if value.strip()]
    return {
        "title": str(frontmatter.get("title") or (headings[0] if headings else fallback_title)),
        "headings": headings,
        "tags": sorted(
            {str(tag).lstrip("#") for tag in raw_tags} | set(INLINE_TAG.findall(body))
        ),
        "outbound_links": sorted(
            set(WIKILINK.findall(body)) | set(MARKDOWN_LINK.findall(body))
        ),
    }


def file_info(vault: Path, path: Path, vault_id: str) -> dict[str, Any]:
    resolved = path.resolve()
    if not resolved.is_relative_to(vault):
        raise KnowledgeError(f"note escaped vault: {path}")
    content = resolved.read_text(encoding="utf-8")
    stat = resolved.stat()
    relative_path = resolved.relative_to(vault).as_posix()
    created = getattr(stat, "st_birthtime", stat.st_ctime)
    return {
        "relative_path": relative_path,
        "file_identity": f"{stat.st_dev}:{stat.st_ino}",
        "content_hash": sha256_text(content),
        **parse_note(content, resolved.stem),
        "created_at": datetime.fromtimestamp(created, timezone.utc)
        .isoformat()
        .replace("+00:00", "Z"),
        "modified_at": datetime.fromtimestamp(stat.st_mtime, timezone.utc)
        .isoformat()
        .replace("+00:00", "Z"),
        "provenance": {
            "source": "obsidian_markdown",
            "vault_id": vault_id,
            "relative_path": relative_path,
        },
    }


def sync_documents(payload: dict[str, Any]) -> dict[str, Any]:
    started = time.monotonic()
    started_at = now_rfc3339()
    vault, database, scope, vault_id = settings(payload)
    scanned = {
        info["relative_path"]: info
        for info in (
            file_info(vault, path, vault_id)
            for path in sorted((vault / scope).rglob("*.md"))
            if path.is_file()
        )
    }
    connection = connect(database)
    active_rows = connection.execute(
        """
        SELECT * FROM documents WHERE vault_id = ? AND tombstoned_at IS NULL
          AND (relative_path = ? OR relative_path LIKE ?)
        """,
        (vault_id, scope, f"{scope}/%"),
    ).fetchall()
    active = {row["relative_path"]: row for row in active_rows}
    missing = {path: row for path, row in active.items() if path not in scanned}
    missing_by_identity = {row["file_identity"]: row for row in missing.values()}
    missing_by_hash: dict[str, list[sqlite3.Row]] = {}
    for row in missing.values():
        missing_by_hash.setdefault(row["content_hash"], []).append(row)
    counts = dict(created=0, updated=0, renamed=0, tombstoned=0, unchanged=0)
    consumed: set[str] = set()
    indexed_at = now_rfc3339()

    for relative_path, info in scanned.items():
        row = active.get(relative_path)
        rename_reason = None
        if row is None:
            identity_match = missing_by_identity.get(info["file_identity"])
            hash_matches = [
                candidate
                for candidate in missing_by_hash.get(info["content_hash"], [])
                if candidate["relative_path"] not in consumed
            ]
            if identity_match and identity_match["relative_path"] not in consumed:
                row, rename_reason = identity_match, "filesystem_identity"
            elif len(hash_matches) == 1:
                row, rename_reason = hash_matches[0], "unique_content_hash"
        if row is None:
            row = connection.execute(
                "SELECT * FROM documents WHERE vault_id = ? AND relative_path = ?",
                (vault_id, relative_path),
            ).fetchone()
        if row is None:
            document_id = deterministic_id(
                "document:obsidian", vault_id, info["file_identity"]
            )
            connection.execute(
                """
                INSERT INTO documents VALUES
                (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)
                """,
                (
                    document_id,
                    vault_id,
                    relative_path,
                    info["file_identity"],
                    info["content_hash"],
                    info["title"],
                    compact_json(info["headings"]),
                    compact_json(info["tags"]),
                    compact_json(info["outbound_links"]),
                    info["created_at"],
                    info["modified_at"],
                    indexed_at,
                    compact_json(info["provenance"]),
                ),
            )
            counts["created"] += 1
            continue

        previous_path = row["relative_path"]
        if previous_path != relative_path:
            consumed.add(previous_path)
            connection.execute(
                "INSERT OR IGNORE INTO document_lineage VALUES (?, ?, ?, ?, ?, ?)",
                (
                    deterministic_id(
                        "lineage", row["document_id"], previous_path, relative_path
                    ),
                    row["document_id"],
                    previous_path,
                    relative_path,
                    indexed_at,
                    rename_reason or "path_reactivation",
                ),
            )
            counts["renamed"] += 1

        changed = (
            row["content_hash"] != info["content_hash"]
            or row["title"] != info["title"]
            or row["headings_json"] != compact_json(info["headings"])
            or row["tags_json"] != compact_json(info["tags"])
            or row["outbound_links_json"] != compact_json(info["outbound_links"])
            or row["tombstoned_at"] is not None
        )
        connection.execute(
            """
            UPDATE documents SET relative_path = ?, file_identity = ?,
              content_hash = ?, title = ?, headings_json = ?, tags_json = ?,
              outbound_links_json = ?, created_at = ?, modified_at = ?,
              indexed_at = ?, provenance_json = ?, tombstoned_at = NULL
            WHERE document_id = ?
            """,
            (
                relative_path,
                info["file_identity"],
                info["content_hash"],
                info["title"],
                compact_json(info["headings"]),
                compact_json(info["tags"]),
                compact_json(info["outbound_links"]),
                info["created_at"],
                info["modified_at"],
                indexed_at,
                compact_json(info["provenance"]),
                row["document_id"],
            ),
        )
        if changed:
            counts["updated"] += 1
        elif previous_path == relative_path:
            counts["unchanged"] += 1

    for relative_path, row in missing.items():
        if relative_path not in consumed:
            connection.execute(
                "UPDATE documents SET tombstoned_at = ?, indexed_at = ? WHERE document_id = ?",
                (indexed_at, indexed_at, row["document_id"]),
            )
            counts["tombstoned"] += 1

    duration_ms = int((time.monotonic() - started) * 1000)
    run_id = deterministic_id("knowledge-sync", vault_id, scope, started_at)
    connection.execute(
        "INSERT INTO sync_runs VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        (
            run_id,
            vault_id,
            scope,
            started_at,
            now_rfc3339(),
            counts["created"],
            counts["updated"],
            counts["renamed"],
            counts["tombstoned"],
            counts["unchanged"],
            duration_ms,
        ),
    )
    connection.commit()
    active_count = connection.execute(
        """
        SELECT COUNT(*) FROM documents WHERE vault_id = ?
          AND tombstoned_at IS NULL
          AND (relative_path = ? OR relative_path LIKE ?)
        """,
        (vault_id, scope, f"{scope}/%"),
    ).fetchone()[0]
    connection.close()
    return {
        "status": "synced",
        "run_id": run_id,
        "vault_id": vault_id,
        "scope": scope,
        "active_documents": active_count,
        **{f"{key}_count": value for key, value in counts.items()},
        "duration_ms": duration_ms,
    }


def safe_note_path(vault: Path, relative_path: str) -> Path:
    path = (vault / relative_path).resolve()
    if not path.is_relative_to(vault):
        raise KnowledgeError("note path escaped vault")
    return path


def excerpt(content: str, terms: list[str], limit: int) -> str:
    lowered = content.lower()
    positions = [lowered.find(term) for term in terms if lowered.find(term) >= 0]
    start = max(0, (min(positions) if positions else 0) - limit // 4)
    value = content[start : start + limit].strip()
    if start:
        value = f"…{value}"
    if start + limit < len(content):
        value = f"{value}…"
    return value


def row_to_document(row: sqlite3.Row, excerpt_text: str, score: float) -> dict[str, Any]:
    vault_id, path = row["vault_id"], row["relative_path"]
    return {
        "document_id": row["document_id"],
        "vault_id": vault_id,
        "relative_path": path,
        "content_hash": row["content_hash"],
        "title": row["title"],
        "excerpt": excerpt_text,
        "score": round(max(0.0, min(1.0, score)), 4),
        "modified_at": row["modified_at"],
        "headings": loads(row["headings_json"], []),
        "tags": loads(row["tags_json"], []),
        "outbound_links": loads(row["outbound_links_json"], []),
        "provenance": loads(row["provenance_json"], {}),
        "metadata": {
            "authority": "authored_knowledge",
            "source_uri": (
                f"obsidian://open?vault={quote(vault_id)}&file={quote(path)}"
            ),
        },
    }


def knowledge_context_packet(query: str, documents: list[dict[str, Any]]) -> dict[str, Any]:
    refs = [
        {
            "ref_id": document["document_id"],
            "kind": "obsidian_document",
            "authority": "authored_knowledge",
            "summary": document["excerpt"],
            "uri": document["metadata"]["source_uri"],
            "metadata": {
                key: value
                for key, value in document.items()
                if key not in {"document_id", "excerpt", "metadata"}
            },
        }
        for document in documents
    ]
    query_hash = hashlib.sha256(query.encode()).hexdigest()[:16]
    return {
        "packet_id": deterministic_id("context:knowledge", query, now_rfc3339()),
        "generated_at": now_rfc3339(),
        "query_id": f"knowledge:search:{query_hash}",
        "audience_role": None,
        "summary": f"Obsidian authored knowledge for {query!r}",
        "refs": refs,
        "sections": [
            {
                "title": "Obsidian authored knowledge",
                "authority": "authored_knowledge",
                "ref_ids": [ref["ref_id"] for ref in refs],
            }
        ],
        "policy_notes": [
            "Obsidian Markdown remains canonical for note bodies and authored structure.",
            "Backlinks and extracted entity links are candidates, not confirmed LifeGraph truth.",
            "Create, patch, and LifeGraph-link operations require a reviewable proposal.",
        ],
        "metadata": {"source": "knowledge_search"},
    }


def search_documents(payload: dict[str, Any]) -> dict[str, Any]:
    query = required(payload, "query")
    _, database, scope, vault_id = settings(payload)
    limit = min(max(int(payload.get("results", 5)), 1), 20)
    excerpt_chars = min(
        max(int(payload.get("excerpt_chars", 800)), 100), MAX_EXCERPT_CHARS
    )
    terms = [term for term in re.findall(r"[a-z0-9_-]+", query.lower()) if len(term) > 1]
    if not terms:
        raise KnowledgeError("query requires at least one searchable term")
    connection = connect(database)
    rows = connection.execute(
        """
        SELECT * FROM documents WHERE vault_id = ? AND tombstoned_at IS NULL
          AND (relative_path = ? OR relative_path LIKE ?)
        """,
        (vault_id, scope, f"{scope}/%"),
    ).fetchall()
    ranked = []
    for row in rows:
        title = row["title"].lower()
        metadata = " ".join(
            [
                row["relative_path"],
                row["headings_json"],
                row["tags_json"],
                row["outbound_links_json"],
            ]
        ).lower()
        raw_score = sum(
            8 * title.count(term) + 4 * metadata.count(term)
            for term in terms
        )
        matched = sum(1 for term in terms if term in metadata)
        if raw_score <= 0:
            continue
        score = min(1.0, (matched / len(terms)) * 0.7 + min(raw_score, 30) / 100)
        summary = " · ".join(
            value
            for value in [
                row["title"],
                ", ".join(loads(row["headings_json"], [])),
                " ".join(f"#{tag}" for tag in loads(row["tags_json"], [])),
            ]
            if value
        )
        ranked.append(
            (
                score,
                row["modified_at"],
                row_to_document(row, summary[:excerpt_chars], score),
            )
        )
    ranked.sort(key=lambda item: (item[0], item[1]), reverse=True)
    documents = [item[2] for item in ranked[:limit]]
    connection.close()
    result = {
        "status": "ok",
        "query": query,
        "vault_id": vault_id,
        "scope": scope,
        "authority": "authored_knowledge",
        "documents": documents,
        "count": len(documents),
        "policy_notes": [
            "Markdown remains canonical for note bodies.",
            "Search uses the derived metadata index; knowledge.read retrieves the live body.",
            "Extracted links are candidates, not confirmed LifeGraph truth.",
        ],
    }
    result["context_packet"] = (
        knowledge_context_packet(query, documents) if documents else None
    )
    return result


def read_document(payload: dict[str, Any]) -> dict[str, Any]:
    document_id = required(payload, "document_id")
    vault, database, _, _ = settings(payload)
    max_chars = min(
        max(int(payload.get("max_chars", MAX_READ_CHARS)), 100), MAX_READ_CHARS
    )
    connection = connect(database)
    row = connection.execute(
        "SELECT * FROM documents WHERE document_id = ? AND tombstoned_at IS NULL",
        (document_id,),
    ).fetchone()
    connection.close()
    if row is None:
        raise KnowledgeError(f"active document not found: {document_id}")
    path = safe_note_path(vault, row["relative_path"])
    if not path.is_file():
        raise KnowledgeError("source note is missing; run knowledge.sync")
    content = path.read_text(encoding="utf-8")
    return {
        "status": "ok",
        "authority": "authored_knowledge",
        "document": row_to_document(row, content[:max_chars], 1.0),
        "content": content[:max_chars],
        "truncated": len(content) > max_chars,
    }


def sync_status(payload: dict[str, Any]) -> dict[str, Any]:
    _, database, scope, vault_id = settings(payload)
    connection = connect(database)
    active = connection.execute(
        """
        SELECT COUNT(*) FROM documents WHERE vault_id = ?
          AND tombstoned_at IS NULL
          AND (relative_path = ? OR relative_path LIKE ?)
        """,
        (vault_id, scope, f"{scope}/%"),
    ).fetchone()[0]
    tombstoned = connection.execute(
        """
        SELECT COUNT(*) FROM documents WHERE vault_id = ?
          AND tombstoned_at IS NOT NULL
          AND (relative_path = ? OR relative_path LIKE ?)
        """,
        (vault_id, scope, f"{scope}/%"),
    ).fetchone()[0]
    pending = connection.execute(
        "SELECT COUNT(*) FROM review_queue WHERE status = 'pending'"
    ).fetchone()[0]
    last = connection.execute(
        """
        SELECT * FROM sync_runs WHERE vault_id = ? AND scope = ?
        ORDER BY completed_at DESC LIMIT 1
        """,
        (vault_id, scope),
    ).fetchone()
    connection.close()
    with SYNC_STATE_LOCK:
        refresh = dict(SYNC_STATE)
    return {
        "status": "ok",
        "vault_id": vault_id,
        "scope": scope,
        "active_documents": active,
        "tombstoned_documents": tombstoned,
        "pending_review_proposals": pending,
        "last_sync": dict(last) if last else None,
        "background_refresh": refresh,
    }


def propose_operation(payload: dict[str, Any], operation: str) -> dict[str, Any]:
    _, database, _, _ = settings(payload)
    document_id = str(payload.get("document_id", "")).strip() or None
    target_ref = str(payload.get("target_ref", "")).strip() or None
    relation = str(payload.get("relation", "")).strip().upper() or None
    rationale = required(payload, "rationale")
    evidence = str(payload.get("evidence", "")).strip()
    proposal = payload.get("proposal", {})
    if operation in {"patch", "link"} and not document_id:
        raise KnowledgeError(f"document_id is required for {operation} proposals")
    if operation == "link":
        if not target_ref:
            raise KnowledgeError("target_ref is required for link proposals")
        if relation not in ALLOWED_LINK_RELATIONS:
            raise KnowledgeError(
                f"relation must be one of {sorted(ALLOWED_LINK_RELATIONS)}"
            )
    if operation in {"create", "patch"} and not proposal:
        raise KnowledgeError(f"proposal is required for {operation} proposals")
    identity = compact_json(
        {
            "operation": operation,
            "document_id": document_id,
            "target_ref": target_ref,
            "relation": relation,
            "proposal": proposal,
            "rationale": rationale,
            "evidence": evidence,
        }
    )
    proposal_id = deterministic_id("knowledge-proposal", identity)
    connection = connect(database)
    if document_id and not connection.execute(
        "SELECT 1 FROM documents WHERE document_id = ? AND tombstoned_at IS NULL",
        (document_id,),
    ).fetchone():
        connection.close()
        raise KnowledgeError(f"active document not found: {document_id}")
    connection.execute(
        """
        INSERT OR IGNORE INTO review_queue (
          proposal_id, operation, document_id, target_ref, relation,
          payload_json, rationale, evidence, status, operator_approved,
          created_at, reviewed_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'pending', 0, ?, NULL)
        """,
        (
            proposal_id,
            operation,
            document_id,
            target_ref,
            relation,
            compact_json(proposal),
            rationale,
            evidence,
            now_rfc3339(),
        ),
    )
    connection.commit()
    row = connection.execute(
        "SELECT * FROM review_queue WHERE proposal_id = ?", (proposal_id,)
    ).fetchone()
    connection.close()
    return {
        "status": "proposed",
        "proposal_id": proposal_id,
        "operation": operation,
        "review_state": row["status"],
        "operator_approved": bool(row["operator_approved"]),
        "applied": False,
    }


def list_review_queue(payload: dict[str, Any]) -> dict[str, Any]:
    _, database, _, _ = settings(payload)
    status = str(payload.get("status", "pending"))
    limit = min(max(int(payload.get("limit", 50)), 1), 200)
    connection = connect(database)
    rows = connection.execute(
        """
        SELECT * FROM review_queue WHERE status = ?
        ORDER BY created_at ASC LIMIT ?
        """,
        (status, limit),
    ).fetchall()
    connection.close()
    proposals = []
    for row in rows:
        item = dict(row)
        item["payload"] = loads(item.pop("payload_json"), {})
        item["operator_approved"] = bool(item["operator_approved"])
        proposals.append(item)
    return {"status": "ok", "review_state": status, "proposals": proposals}


TOOLS = {
    "knowledge.search": (
        "Search indexed Obsidian authored metadata without blocking on cloud note hydration.",
        {"query": "string", "results": "integer"},
        search_documents,
    ),
    "knowledge.read": (
        "Read a bounded current Obsidian note body by stable document ID.",
        {"document_id": "string", "max_chars": "integer"},
        read_document,
    ),
    "knowledge.sync.status": (
        "Inspect governed Obsidian projection and review-queue status.",
        {},
        sync_status,
    ),
    "knowledge.create.propose": (
        "Queue a proposed note creation; never writes the vault directly.",
        {"proposal": "object", "rationale": "string", "evidence": "string"},
        lambda payload: propose_operation(payload, "create"),
    ),
    "knowledge.patch.propose": (
        "Queue a proposed note patch; never applies it directly.",
        {
            "document_id": "string",
            "proposal": "object",
            "rationale": "string",
        },
        lambda payload: propose_operation(payload, "patch"),
    ),
    "knowledge.link.propose": (
        "Queue a proposed LifeGraph relationship for operator review.",
        {
            "document_id": "string",
            "target_ref": "string",
            "relation": "string",
            "rationale": "string",
        },
        lambda payload: propose_operation(payload, "link"),
    ),
    "knowledge.review.list": (
        "List pending or reviewed knowledge proposals.",
        {"status": "string", "limit": "integer"},
        list_review_queue,
    ),
}


def tool_schema(properties: dict[str, str]) -> dict[str, Any]:
    schema = {
        "type": "object",
        "properties": {
            name: {"type": type_name} for name, type_name in properties.items()
        },
    }
    required_fields = [
        name
        for name in ("query", "document_id", "proposal", "rationale", "target_ref", "relation")
        if name in properties
    ]
    if required_fields:
        schema["required"] = required_fields
    return schema


def handle_mcp(request: dict[str, Any]) -> dict[str, Any] | None:
    request_id, method = request.get("id"), request.get("method")
    if method == "notifications/initialized":
        return None
    if method == "initialize":
        result = {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "philotic-obsidian-knowledge", "version": "0.1.0"},
        }
    elif method == "tools/list":
        result = {
            "tools": [
                {
                    "name": name,
                    "description": definition[0],
                    "inputSchema": tool_schema(definition[1]),
                }
                for name, definition in TOOLS.items()
            ]
        }
    elif method == "tools/call":
        params = request.get("params") or {}
        name = params.get("name")
        if name not in TOOLS:
            return {
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {"code": -32601, "message": f"unknown tool: {name}"},
            }
        value = TOOLS[name][2](params.get("arguments") or {})
        result = {
            "content": [{"type": "text", "text": compact_json(value)}],
            "structuredContent": value,
            "isError": False,
        }
    else:
        return {
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": -32601, "message": f"unknown method: {method}"},
        }
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


def background_sync_once(payload: dict[str, Any] | None = None) -> None:
    with SYNC_STATE_LOCK:
        SYNC_STATE.update(state="running", started_at=now_rfc3339(), error=None)
    try:
        sync_documents(payload or {})
    except Exception as exc:
        with SYNC_STATE_LOCK:
            SYNC_STATE.update(
                state="error",
                completed_at=now_rfc3339(),
                error=f"{type(exc).__name__}: {exc}",
            )
    else:
        with SYNC_STATE_LOCK:
            SYNC_STATE.update(
                state="idle", completed_at=now_rfc3339(), error=None
            )


def background_sync_loop() -> None:
    interval = max(
        int(os.environ.get("PHILOTIC_KNOWLEDGE_SYNC_INTERVAL_SECS", "30")), 5
    )
    while True:
        background_sync_once()
        time.sleep(interval)


def serve_mcp() -> int:
    threading.Thread(
        target=background_sync_loop,
        name="obsidian-knowledge-sync",
        daemon=True,
    ).start()
    for line in sys.stdin:
        if not line.strip():
            continue
        try:
            response = handle_mcp(json.loads(line))
        except (json.JSONDecodeError, KnowledgeError) as exc:
            response = {
                "jsonrpc": "2.0",
                "id": None,
                "error": {"code": -32602, "message": str(exc)},
            }
        except Exception as exc:
            response = {
                "jsonrpc": "2.0",
                "id": None,
                "error": {"code": -32603, "message": f"{type(exc).__name__}: {exc}"},
            }
        if response is not None:
            print(compact_json(response), flush=True)
    return 0


CLI_COMMANDS = {
    "sync": sync_documents,
    "search": search_documents,
    "read": read_document,
    "status": sync_status,
    "create-propose": lambda payload: propose_operation(payload, "create"),
    "patch-propose": lambda payload: propose_operation(payload, "patch"),
    "link-propose": lambda payload: propose_operation(payload, "link"),
    "review-list": list_review_queue,
}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=[*sorted(CLI_COMMANDS), "serve"])
    parser.add_argument("--input", help="JSON object; defaults to stdin")
    args = parser.parse_args()
    if args.command == "serve":
        return serve_mcp()
    try:
        raw = args.input if args.input is not None else sys.stdin.read()
        payload = json.loads(raw or "{}")
        if not isinstance(payload, dict):
            raise KnowledgeError("input must be a JSON object")
        print(compact_json(CLI_COMMANDS[args.command](payload)))
        return 0
    except (KnowledgeError, json.JSONDecodeError) as exc:
        print(compact_json({"status": "error", "error": str(exc)}))
        return 2
    except Exception as exc:
        print(
            compact_json({"status": "error", "error": f"{type(exc).__name__}: {exc}"})
        )
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
