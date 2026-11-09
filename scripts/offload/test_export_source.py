"""Committed snapshot exporter checks using generated archives, without Git calls."""
import hashlib
import importlib.util
import io
import json
import pathlib
import tarfile
import tempfile
import types
import unittest
from unittest.mock import patch

HERE = pathlib.Path(__file__).parent
SPEC = importlib.util.spec_from_file_location("fallback_exporter", HERE / "export_source.py")
exporter = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(exporter)


class ExportTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.base = pathlib.Path(self.tmp.name)
        self.destination = self.base / "snapshot"
        self.sha = "a" * 40

    def fake_git(self, _repo, *args, **_kwargs):
        if args[0] == "rev-parse":
            return types.SimpleNamespace(stdout=self.sha + "\n")
        self.assertEqual(args[0], "archive")
        path = pathlib.Path(next(arg.split("=", 1)[1] for arg in args if arg.startswith("--output=")))
        with tarfile.open(path, "w") as archive:
            content = b"synthetic committed source"
            member = tarfile.TarInfo("source.txt")
            member.size = len(content)
            archive.addfile(member, io.BytesIO(content))
        return types.SimpleNamespace(returncode=0)

    def test_export_records_independent_manifest_digest(self):
        with patch.object(exporter, "git", side_effect=self.fake_git):
            result = exporter.export(self.base, "refs/heads/main", "likesjx/philotic-stack", self.destination)
        manifest_path = self.destination / "SOURCE_IDENTITY.json"
        self.assertEqual(result["manifest_sha256"], hashlib.sha256(manifest_path.read_bytes()).hexdigest())
        manifest = json.loads(manifest_path.read_text())
        self.assertEqual(manifest["source_sha"], self.sha)
        self.assertEqual(manifest["files"]["source.txt"], hashlib.sha256(b"synthetic committed source").hexdigest())

    def test_existing_destination_and_invalid_ref_refuse_before_git(self):
        self.destination.mkdir()
        with patch.object(exporter, "git") as git:
            with self.assertRaises(ValueError):
                exporter.export(self.base, "main", "likesjx/philotic-stack", self.destination)
            with self.assertRaises(ValueError):
                exporter.export(self.base, "--bad", "likesjx/philotic-stack", self.base / "new")
        git.assert_not_called()

    def test_links_refused_before_destination_created(self):
        def linked_git(repo, *args, **kwargs):
            result = self.fake_git(repo, *args, **kwargs)
            if args[0] == "archive":
                path = pathlib.Path(next(arg.split("=", 1)[1] for arg in args if arg.startswith("--output=")))
                with tarfile.open(path, "a") as archive:
                    member = tarfile.TarInfo("link")
                    member.type = tarfile.SYMTYPE
                    member.linkname = "/outside"
                    archive.addfile(member)
            return result
        with patch.object(exporter, "git", side_effect=linked_git), self.assertRaises(ValueError):
            exporter.export(self.base, "main", "likesjx/philotic-stack", self.destination)
        self.assertFalse(self.destination.exists())

    def test_native_selection_contains_test_and_compile_inputs(self):
        selected = exporter.SELECTIONS["likesjx/philotic-stack"]
        for path in ("ansible/roles/percival_personal_mcp", "rust-toolchain.toml", "vendor/neo4rs"):
            self.assertIn(path, selected)


if __name__ == "__main__":
    unittest.main()
