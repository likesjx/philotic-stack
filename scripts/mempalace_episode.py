#!/usr/bin/env python3
"""Governed MemPalace episodic capture, recall, status, and deletion adapter.

The adapter writes directly to MemPalace's collection. Intel Graph may invoke
it as a compatibility endpoint, but does not own the episode data.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import re
import shutil
import sys
from datetime import datetime, timezone
from typing import Any


EPISODE_TYPE = "episodic_episode"
DEFAULT_ROOM = "episodes"
MAX_CONTENT_CHARS = 200_000
MAX_EXCERPT_CHARS = 2_000
VALID_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9:._-]{0,199}$")
NO_CAPTURE_MARKERS = ("[[no-capture]]", "<no-memory>", "<!-- no-memory -->")
PRIVATE_BLOCKS = (
    re.compile(r"\[\[private\]\].*?\[\[/private\]\]", re.IGNORECASE | re.DOTALL),
    re.compile(r"<private>.*?</private>", re.IGNORECASE | re.DOTALL),
)
SECRET_PATTERNS = (
    re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----", re.DOTALL),
    re.compile(r"\bsk-[A-Za-z0-9_-]{16,}\b"),
    re.compile(r"\bgh[opusr]_[A-Za-z0-9]{20,}\b"),
    re.compile(r"\bAKIA[0-9A-Z]{16}\b"),
    re.compile(
        r"(?im)\b(api[_-]?key|access[_-]?token|auth[_-]?token|password|secret)"
        r"\s*[:=]\s*([\"']?)[^\s,\"']+\2"
    ),
)
PRIVACY_CLASSES = {"normal", "sensitive", "private"}
RETENTION_CLASSES = {"session", "days30", "days90", "durable", "user_managed"}


class EpisodeError(ValueError):
    """A caller-visible episodic contract error."""


def _json_line(value: dict[str, Any]) -> None:
    print(json.dumps(value, sort_keys=True, separators=(",", ":")))


def _required(payload: dict[str, Any], key: str) -> str:
    value = str(payload.get(key, "")).strip()
    if not value:
        raise EpisodeError(f"{key} is required")
    return value


def _safe_id(value: str, field: str) -> str:
    if not VALID_ID.fullmatch(value):
        raise EpisodeError(
            f"{field} must match {VALID_ID.pattern} and be at most 200 characters"
        )
    return value


def _safe_slug(value: str) -> str:
    slug = re.sub(r"[^a-z0-9_-]+", "-", value.lower()).strip("-")
    return slug[:80] or "unknown"


def _sha256(content: str) -> str:
    return f"sha256:{hashlib.sha256(content.encode('utf-8')).hexdigest()}"


def redact_content(content: str) -> tuple[str | None, list[str]]:
    lowered = content.lower()
    if any(marker in lowered for marker in NO_CAPTURE_MARKERS):
        return None, ["no_capture_marker"]

    redacted = content
    reasons: list[str] = []
    for pattern in PRIVATE_BLOCKS:
        redacted, count = pattern.subn("[REDACTED:private-block]", redacted)
        if count:
            reasons.append("private_block")
    for pattern in SECRET_PATTERNS:
        redacted, count = pattern.subn("[REDACTED:secret]", redacted)
        if count:
            reasons.append("secret_pattern")
    return redacted, sorted(set(reasons))


def _collection(create: bool = False):
    try:
        import chromadb
        from mempalace.config import MempalaceConfig
    except ImportError as exc:
        raise EpisodeError(
            "MemPalace dependencies are unavailable; install mempalace and chromadb"
        ) from exc

    config = MempalaceConfig()
    client = chromadb.PersistentClient(path=config.palace_path)
    if create:
        return client.get_or_create_collection(config.collection_name)
    try:
        return client.get_collection(config.collection_name)
    except Exception as exc:
        raise EpisodeError(
            f"no MemPalace collection at {config.palace_path}; initialize MemPalace first"
        ) from exc


def _normalize_capture(payload: dict[str, Any]) -> tuple[dict[str, Any], str | None, list[str]]:
    content = _required(payload, "content_or_summary")
    if len(content) > MAX_CONTENT_CHARS:
        raise EpisodeError(f"content_or_summary exceeds {MAX_CONTENT_CHARS} characters")

    session_id = _safe_id(_required(payload, "session_id"), "session_id")
    client = _required(payload, "client").lower()
    agent_or_role = _required(payload, "agent_or_role")
    source_event = _required(payload, "source_event")
    captured_at = str(payload.get("captured_at") or datetime.now(timezone.utc).isoformat())
    try:
        captured_dt = datetime.fromisoformat(captured_at.replace("Z", "+00:00"))
    except ValueError as exc:
        raise EpisodeError("captured_at must be RFC3339") from exc
    if captured_dt.tzinfo is None:
        raise EpisodeError("captured_at must include a timezone")

    privacy_class = str(payload.get("privacy_class", "normal")).lower()
    retention_class = str(payload.get("retention_class", "days90")).lower()
    if privacy_class not in PRIVACY_CLASSES:
        raise EpisodeError(f"privacy_class must be one of {sorted(PRIVACY_CLASSES)}")
    if retention_class not in RETENTION_CLASSES:
        raise EpisodeError(f"retention_class must be one of {sorted(RETENTION_CLASSES)}")

    redacted, redaction_reasons = redact_content(content)
    source_hash = _sha256(content)
    supplied_hash = str(payload.get("content_hash", "")).strip()
    if supplied_hash and supplied_hash != source_hash:
        raise EpisodeError("content_hash does not match content_or_summary")
    source_event_id = str(payload.get("source_event_id", "")).strip()
    identity_material = "\0".join(
        (client, session_id, source_event, source_event_id or source_hash)
    )
    default_id = f"episode:{_safe_slug(client)}:{hashlib.sha256(identity_material.encode()).hexdigest()[:32]}"
    episode_id = _safe_id(str(payload.get("episode_id") or default_id), "episode_id")

    normalized = {
        "episode_id": episode_id,
        "session_id": session_id,
        "client": client,
        "agent_or_role": agent_or_role,
        "captured_at": captured_dt.astimezone(timezone.utc).isoformat().replace("+00:00", "Z"),
        "captured_epoch": captured_dt.timestamp(),
        "source_event": source_event,
        "source_event_id": source_event_id,
        "privacy_class": privacy_class,
        "retention_class": retention_class,
        "related_context_refs": [
            str(value).strip()
            for value in payload.get("related_context_refs", [])
            if str(value).strip()
        ],
        "provenance": payload.get("provenance", {}),
        "metadata": payload.get("metadata", {}),
        "source_content_hash": supplied_hash or source_hash,
    }
    return normalized, redacted, redaction_reasons


def capture_episode(payload: dict[str, Any], collection=None) -> dict[str, Any]:
    normalized, content, redaction_reasons = _normalize_capture(payload)
    if content is None:
        return {
            "status": "skipped",
            "reason": "no_capture_marker",
            "episode_id": normalized["episode_id"],
        }

    normalized["content_hash"] = _sha256(content)
    col = collection or _collection(create=True)
    existing = col.get(ids=[normalized["episode_id"]], include=["metadatas"])
    if existing.get("ids"):
        existing_meta = existing.get("metadatas", [{}])[0] or {}
        if existing_meta.get("content_hash") == normalized["content_hash"]:
            return {
                "status": "duplicate",
                "episode_id": normalized["episode_id"],
                "content_hash": normalized["content_hash"],
            }
        raise EpisodeError(
            f"episode_id conflict: {normalized['episode_id']} already exists with different content"
        )

    metadata = {
        "type": EPISODE_TYPE,
        "wing": f"wing_{_safe_slug(normalized['client'])}",
        "room": DEFAULT_ROOM,
        "episode_id": normalized["episode_id"],
        "session_id": normalized["session_id"],
        "client": normalized["client"],
        "agent_or_role": normalized["agent_or_role"],
        "captured_at": normalized["captured_at"],
        "captured_epoch": normalized["captured_epoch"],
        "source_event": normalized["source_event"],
        "source_event_id": normalized["source_event_id"],
        "content_hash": normalized["content_hash"],
        "source_content_hash": normalized["source_content_hash"],
        "privacy_class": normalized["privacy_class"],
        "retention_class": normalized["retention_class"],
        "related_context_refs_json": json.dumps(normalized["related_context_refs"]),
        "provenance_json": json.dumps(normalized["provenance"], sort_keys=True),
        "metadata_json": json.dumps(normalized["metadata"], sort_keys=True),
        "redaction_reasons_json": json.dumps(redaction_reasons),
        "filed_at": datetime.now(timezone.utc).isoformat(),
    }
    col.add(ids=[normalized["episode_id"]], documents=[content], metadatas=[metadata])
    return {
        "status": "captured",
        "episode_id": normalized["episode_id"],
        "session_id": normalized["session_id"],
        "client": normalized["client"],
        "content_hash": normalized["content_hash"],
        "privacy_class": normalized["privacy_class"],
        "retention_class": normalized["retention_class"],
        "redacted": bool(redaction_reasons),
        "redaction_reasons": redaction_reasons,
    }


def _where(filters: list[dict[str, Any]]) -> dict[str, Any]:
    if len(filters) == 1:
        return filters[0]
    return {"$and": filters}


def recall_episodes(payload: dict[str, Any], collection=None) -> dict[str, Any]:
    query = _required(payload, "query")
    requested = min(max(int(payload.get("results", 5)), 1), 20)
    excerpt_chars = min(
        max(int(payload.get("excerpt_chars", MAX_EXCERPT_CHARS)), 100),
        MAX_EXCERPT_CHARS,
    )
    filters: list[dict[str, Any]] = [{"type": EPISODE_TYPE}]
    for field in ("client", "session_id"):
        if value := str(payload.get(field, "")).strip():
            filters.append({field: value.lower() if field == "client" else value})

    col = collection or _collection()
    result = col.query(
        query_texts=[query],
        n_results=min(requested * 4, 80),
        where=_where(filters),
        include=["documents", "metadatas", "distances"],
    )
    ids = (result.get("ids") or [[]])[0]
    docs = (result.get("documents") or [[]])[0]
    metas = (result.get("metadatas") or [[]])[0]
    distances = (result.get("distances") or [[]])[0]
    include_private = bool(payload.get("include_private", False))

    episodes = []
    for episode_id, document, metadata, distance in zip(ids, docs, metas, distances):
        if metadata.get("privacy_class") == "private" and not include_private:
            continue
        episodes.append(
            {
                "episode_id": episode_id,
                "session_id": metadata.get("session_id", ""),
                "client": metadata.get("client", ""),
                "agent_or_role": metadata.get("agent_or_role", ""),
                "captured_at": metadata.get("captured_at", ""),
                "source_event": metadata.get("source_event", ""),
                "excerpt": document[:excerpt_chars],
                "content_hash": metadata.get("content_hash", ""),
                "score": round(max(0.0, min(1.0, 1.0 - float(distance))), 4),
                "retrieval_rationale": "semantic similarity within authorized episodic filters",
                "privacy_class": metadata.get("privacy_class", "normal"),
                "retention_class": metadata.get("retention_class", "days90"),
                "related_context_refs": json.loads(
                    metadata.get("related_context_refs_json", "[]")
                ),
                "provenance": json.loads(metadata.get("provenance_json", "{}")),
                "metadata": json.loads(metadata.get("metadata_json", "{}")),
            }
        )
        if len(episodes) >= requested:
            break

    return {
        "status": "ok",
        "query": query,
        "filters": {
            "client": payload.get("client"),
            "session_id": payload.get("session_id"),
            "include_private": include_private,
        },
        "episodes": episodes,
        "count": len(episodes),
    }


def delete_episodes(payload: dict[str, Any], collection=None) -> dict[str, Any]:
    col = collection or _collection()
    if episode_id := str(payload.get("episode_id", "")).strip():
        _safe_id(episode_id, "episode_id")
        existing = col.get(ids=[episode_id], include=["metadatas"])
        ids = existing.get("ids", [])
    else:
        filters: list[dict[str, Any]] = [{"type": EPISODE_TYPE}]
        scoped = False
        for field in ("client", "session_id", "source_event"):
            if value := str(payload.get(field, "")).strip():
                filters.append({field: value.lower() if field == "client" else value})
                scoped = True
        if before := str(payload.get("before", "")).strip():
            try:
                cutoff = datetime.fromisoformat(before.replace("Z", "+00:00")).timestamp()
            except ValueError as exc:
                raise EpisodeError("before must be RFC3339") from exc
            filters.append({"captured_epoch": {"$lt": cutoff}})
            scoped = True
        if not scoped:
            raise EpisodeError(
                "deletion requires episode_id, client, session_id, source_event, or before"
            )
        ids = col.get(where=_where(filters), include=["metadatas"]).get("ids", [])

    if ids:
        col.delete(ids=ids)
    return {"status": "deleted", "deleted_count": len(ids), "episode_ids": ids}


def episode_status(payload: dict[str, Any], collection=None) -> dict[str, Any]:
    col = collection or _collection()
    where = {"type": EPISODE_TYPE}
    metadatas = col.get(where=where, include=["metadatas"]).get("metadatas", [])
    clients: dict[str, int] = {}
    sessions: set[str] = set()
    for metadata in metadatas:
        client = metadata.get("client", "unknown")
        clients[client] = clients.get(client, 0) + 1
        if metadata.get("session_id"):
            sessions.add(metadata["session_id"])
    return {
        "status": "ok",
        "episode_count": len(metadatas),
        "session_count": len(sessions),
        "clients": clients,
        "checked_at": datetime.now(timezone.utc).isoformat(),
    }


COMMANDS = {
    "capture": capture_episode,
    "recall": recall_episodes,
    "delete": delete_episodes,
    "status": episode_status,
}


def _ensure_mempalace_runtime() -> None:
    """Re-exec with the installed MemPalace interpreter when needed.

    uv tool installs commonly isolate chromadb from the system python. The
    `mempalace` console script's shebang is the authoritative interpreter for
    that installation.
    """
    if importlib.util.find_spec("mempalace") and importlib.util.find_spec("chromadb"):
        return
    if os.environ.get("PHILOTIC_EPISODIC_REEXEC") == "1":
        return
    executable = shutil.which("mempalace")
    if not executable:
        return
    try:
        with open(executable, "r", encoding="utf-8") as handle:
            shebang = handle.readline().strip()
    except OSError:
        return
    if not shebang.startswith("#!"):
        return
    interpreter = shebang[2:].strip()
    if not os.path.isfile(interpreter):
        return
    environment = os.environ.copy()
    environment["PHILOTIC_EPISODIC_REEXEC"] = "1"
    os.execve(
        interpreter,
        [interpreter, os.path.abspath(__file__), *sys.argv[1:]],
        environment,
    )


def main() -> int:
    _ensure_mempalace_runtime()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=sorted(COMMANDS))
    parser.add_argument(
        "--input",
        help="JSON payload; defaults to a JSON object read from stdin",
    )
    args = parser.parse_args()
    try:
        raw = args.input if args.input is not None else sys.stdin.read()
        payload = json.loads(raw or "{}")
        if not isinstance(payload, dict):
            raise EpisodeError("input must be a JSON object")
        result = COMMANDS[args.command](payload)
        _json_line(result)
        return 0
    except (EpisodeError, json.JSONDecodeError) as exc:
        _json_line({"status": "error", "error": str(exc)})
        return 2
    except Exception as exc:
        _json_line({"status": "error", "error": f"{type(exc).__name__}: {exc}"})
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
