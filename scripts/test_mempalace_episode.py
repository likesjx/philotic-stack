#!/usr/bin/env python3

import importlib.util
import pathlib
import unittest


SCRIPT = pathlib.Path(__file__).with_name("mempalace_episode.py")
SPEC = importlib.util.spec_from_file_location("mempalace_episode", SCRIPT)
episode = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(episode)


class FakeCollection:
    def __init__(self):
        self.rows = {}

    def add(self, ids, documents, metadatas):
        for row_id, document, metadata in zip(ids, documents, metadatas):
            if row_id in self.rows:
                raise ValueError("duplicate id")
            self.rows[row_id] = (document, metadata)

    def get(self, ids=None, where=None, include=None):
        selected = []
        if ids is not None:
            selected = [(row_id, self.rows[row_id]) for row_id in ids if row_id in self.rows]
        else:
            selected = [
                (row_id, row)
                for row_id, row in self.rows.items()
                if self._matches(row[1], where or {})
            ]
        return {
            "ids": [row_id for row_id, _ in selected],
            "documents": [row[0] for _, row in selected],
            "metadatas": [row[1] for _, row in selected],
        }

    def query(self, query_texts, n_results, where, include):
        selected = [
            (row_id, row)
            for row_id, row in self.rows.items()
            if self._matches(row[1], where)
        ][:n_results]
        return {
            "ids": [[row_id for row_id, _ in selected]],
            "documents": [[row[0] for _, row in selected]],
            "metadatas": [[row[1] for _, row in selected]],
            "distances": [[0.1 for _ in selected]],
        }

    def delete(self, ids):
        for row_id in ids:
            self.rows.pop(row_id, None)

    @classmethod
    def _matches(cls, metadata, where):
        if "$and" in where:
            return all(cls._matches(metadata, child) for child in where["$and"])
        for key, expected in where.items():
            actual = metadata.get(key)
            if isinstance(expected, dict) and "$lt" in expected:
                if not actual < expected["$lt"]:
                    return False
            elif actual != expected:
                return False
        return True


def capture_payload(**overrides):
    payload = {
        "session_id": "session-123",
        "client": "codex",
        "agent_or_role": "codex",
        "captured_at": "2026-07-24T12:00:00Z",
        "source_event": "stop",
        "source_event_id": "turn-7",
        "content_or_summary": "We selected an episodic evidence boundary.",
        "privacy_class": "normal",
        "retention_class": "days90",
        "related_context_refs": ["seam:mempalace-episodic-lane"],
        "provenance": {"hook": "test"},
    }
    payload.update(overrides)
    return payload


class EpisodeAdapterTests(unittest.TestCase):
    def test_capture_is_idempotent_by_stable_episode_id(self):
        collection = FakeCollection()
        first = episode.capture_episode(capture_payload(), collection)
        second = episode.capture_episode(capture_payload(), collection)
        self.assertEqual(first["status"], "captured")
        self.assertEqual(second["status"], "duplicate")
        self.assertEqual(first["episode_id"], second["episode_id"])
        self.assertEqual(len(collection.rows), 1)

    def test_same_episode_id_with_different_content_is_conflict(self):
        collection = FakeCollection()
        episode.capture_episode(capture_payload(episode_id="episode:fixed"), collection)
        with self.assertRaisesRegex(episode.EpisodeError, "conflict"):
            episode.capture_episode(
                capture_payload(
                    episode_id="episode:fixed",
                    content_or_summary="Different content for the same source event.",
                ),
                collection,
            )

    def test_supplied_content_hash_must_match_source_content(self):
        with self.assertRaisesRegex(episode.EpisodeError, "does not match"):
            episode.capture_episode(
                capture_payload(content_hash=f"sha256:{'0' * 64}"),
                FakeCollection(),
            )

    def test_no_capture_and_secret_redaction_happen_before_storage(self):
        collection = FakeCollection()
        skipped = episode.capture_episode(
            capture_payload(content_or_summary="[[no-capture]] secret meeting"), collection
        )
        self.assertEqual(skipped["status"], "skipped")
        self.assertFalse(collection.rows)

        captured = episode.capture_episode(
            capture_payload(
                source_event_id="turn-8",
                content_or_summary="api_key=super-secret-value and [[private]]home detail[[/private]]",
            ),
            collection,
        )
        self.assertTrue(captured["redacted"])
        stored = next(iter(collection.rows.values()))[0]
        self.assertNotIn("super-secret-value", stored)
        self.assertNotIn("home detail", stored)
        self.assertIn("[REDACTED", stored)

    def test_recall_preserves_provenance_and_excludes_private_by_default(self):
        collection = FakeCollection()
        episode.capture_episode(capture_payload(), collection)
        episode.capture_episode(
            capture_payload(
                source_event_id="turn-private",
                privacy_class="private",
                content_or_summary="Private episodic material.",
            ),
            collection,
        )

        recalled = episode.recall_episodes(
            {"query": "episodic boundary", "client": "codex"}, collection
        )
        self.assertEqual(recalled["count"], 1)
        self.assertEqual(recalled["episodes"][0]["session_id"], "session-123")
        self.assertEqual(recalled["episodes"][0]["provenance"], {"hook": "test"})
        self.assertEqual(recalled["episodes"][0]["retrieval_rationale"].split()[0], "semantic")

    def test_delete_requires_scope_and_supports_session(self):
        collection = FakeCollection()
        episode.capture_episode(capture_payload(), collection)
        with self.assertRaisesRegex(episode.EpisodeError, "requires"):
            episode.delete_episodes({}, collection)
        deleted = episode.delete_episodes({"session_id": "session-123"}, collection)
        self.assertEqual(deleted["deleted_count"], 1)
        self.assertFalse(collection.rows)


if __name__ == "__main__":
    unittest.main()
