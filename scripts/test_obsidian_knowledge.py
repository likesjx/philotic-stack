#!/usr/bin/env python3

import importlib.util
import pathlib
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("obsidian_knowledge.py")
SPEC = importlib.util.spec_from_file_location("obsidian_knowledge", SCRIPT)
knowledge = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(knowledge)


class KnowledgeIndexTests(unittest.TestCase):
    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.tempdir.name)
        self.vault = self.root / "Brain"
        self.scope = self.vault / "Efforts" / "Ongoing"
        self.scope.mkdir(parents=True)
        (self.vault / ".obsidian").mkdir()
        self.db = self.root / "knowledge.sqlite3"
        self.payload = {
            "vault_root": str(self.vault),
            "db_path": str(self.db),
            "vault_id": "Brain",
            "scope": "Efforts/Ongoing",
        }

    def tearDown(self):
        self.tempdir.cleanup()

    def write(self, name, content):
        path = self.scope / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        return path

    def test_incremental_create_edit_rename_delete_preserves_identity(self):
        piano = self.write(
            "Piano.md",
            "---\ntags: [music, creative]\n---\n# Piano Practice\nBuild tiny motifs.\n",
        )
        self.write("Second Brain.md", "# Organizing My Second Brain\nLink useful ideas.\n")
        first = knowledge.sync_documents(self.payload)
        self.assertEqual(first["created_count"], 2)
        self.assertEqual(first["active_documents"], 2)

        unchanged = knowledge.sync_documents(self.payload)
        self.assertEqual(unchanged["unchanged_count"], 2)
        self.assertEqual(unchanged["created_count"], 0)
        search = knowledge.search_documents({**self.payload, "query": "piano motifs"})
        document_id = search["documents"][0]["document_id"]
        self.assertEqual(search["authority"], "authored_knowledge")
        self.assertEqual(
            search["context_packet"]["refs"][0]["authority"], "authored_knowledge"
        )

        piano.write_text("# Piano Practice\nBuild tiny motifs into songs.\n", encoding="utf-8")
        self.assertEqual(knowledge.sync_documents(self.payload)["updated_count"], 1)
        renamed = self.scope / "Playing the Piano.md"
        piano.rename(renamed)
        self.assertEqual(knowledge.sync_documents(self.payload)["renamed_count"], 1)
        recalled = knowledge.search_documents({**self.payload, "query": "songs piano"})
        self.assertEqual(recalled["documents"][0]["document_id"], document_id)
        self.assertEqual(
            recalled["documents"][0]["relative_path"],
            "Efforts/Ongoing/Playing the Piano.md",
        )

        renamed.unlink()
        self.assertEqual(knowledge.sync_documents(self.payload)["tombstoned_count"], 1)
        status = knowledge.sync_status(self.payload)
        self.assertEqual(status["active_documents"], 1)
        self.assertEqual(status["tombstoned_documents"], 1)

    def test_read_uses_live_markdown_without_storing_body(self):
        note = self.write(
            "Learning.md", "# Learning Loop\nQuestion → experiment → artifact.\n"
        )
        knowledge.sync_documents(self.payload)
        result = knowledge.search_documents(
            {**self.payload, "query": "learning loop"}
        )
        document_id = result["documents"][0]["document_id"]
        note.write_text("# Learning Loop\nA newly edited live note body.\n", encoding="utf-8")
        read = knowledge.read_document({**self.payload, "document_id": document_id})
        self.assertIn("newly edited live note body", read["content"])

        connection = knowledge.connect(self.db)
        columns = {
            row["name"]
            for row in connection.execute("PRAGMA table_info(documents)").fetchall()
        }
        connection.close()
        self.assertNotIn("content", columns)
        self.assertNotIn("body", columns)

    def test_link_proposal_is_review_only_and_idempotent(self):
        note = self.write("Piano.md", "# Piano\nPractice deliberately.\n")
        original = note.read_text(encoding="utf-8")
        knowledge.sync_documents(self.payload)
        result = knowledge.search_documents(
            {**self.payload, "query": "piano practice"}
        )
        proposal = {
            **self.payload,
            "document_id": result["documents"][0]["document_id"],
            "target_ref": "life:goal:creative-practice",
            "relation": "SUPPORTS",
            "rationale": "The note describes the practice loop.",
            "evidence": "Practice deliberately.",
        }
        first = knowledge.propose_operation(proposal, "link")
        second = knowledge.propose_operation(proposal, "link")
        self.assertEqual(first["proposal_id"], second["proposal_id"])
        self.assertFalse(first["operator_approved"])
        self.assertFalse(first["applied"])
        self.assertEqual(note.read_text(encoding="utf-8"), original)
        queue = knowledge.list_review_queue(self.payload)
        self.assertEqual(len(queue["proposals"]), 1)
        self.assertEqual(queue["proposals"][0]["status"], "pending")

    def test_mcp_surface_exposes_narrow_governed_tools(self):
        response = knowledge.handle_mcp(
            {"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}
        )
        names = [tool["name"] for tool in response["result"]["tools"]]
        self.assertEqual(
            names,
            [
                "knowledge.search",
                "knowledge.read",
                "knowledge.sync.status",
                "knowledge.create.propose",
                "knowledge.patch.propose",
                "knowledge.link.propose",
                "knowledge.review.list",
            ],
        )
        self.assertNotIn("bash.exec", names)
        self.assertNotIn("knowledge.patch.apply", names)

        self.write("Live.md", "# Live projection\nReady for recall.\n")
        knowledge.background_sync_once(self.payload)
        status = knowledge.handle_mcp(
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "knowledge.sync.status",
                    "arguments": self.payload,
                },
            }
        )
        self.assertEqual(
            status["result"]["structuredContent"]["active_documents"], 1
        )
        self.assertEqual(
            status["result"]["structuredContent"]["background_refresh"]["state"],
            "idle",
        )


if __name__ == "__main__":
    unittest.main()
