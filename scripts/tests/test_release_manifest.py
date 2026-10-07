"""Tests for scripts/release-manifest.py.

Run: python3 -m unittest discover -s scripts/tests -p 'test_*.py'
"""

import contextlib
import hashlib
import importlib.util
import io
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "release-manifest.py"

_spec = importlib.util.spec_from_file_location("release_manifest", SCRIPT)
rm = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(rm)


def quiet(fn, *args):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        rc = fn(*args)
    return rc, out.getvalue(), err.getvalue()


class ReleaseManifestTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name) / "stage"
        (self.root / "bin").mkdir(parents=True)
        (self.root / "bin" / "aiua").write_bytes(b"aiua-binary")
        (self.root / "bin" / "philote").write_bytes(b"philote-binary")

    def tearDown(self):
        self.tmp.cleanup()

    def generate(self, *extra):
        return quiet(
            rm.main,
            ["generate", "--dir", str(self.root), "--tag", "v0.2.0-rc.1", "--sha", "a" * 40,
             "--target", "x86_64-unknown-linux-gnu", "--built-at", "2026-10-07T00:00:00Z", *extra],
        )

    def test_generate_writes_manifest_and_sums(self):
        rc, _, _ = self.generate("--require", "aiua", "philote")
        self.assertEqual(rc, 0)
        manifest = json.loads((self.root / "manifest.json").read_text())
        self.assertEqual(manifest["version"], "0.2.0-rc.1")
        self.assertEqual(manifest["tag"], "v0.2.0-rc.1")
        self.assertEqual(manifest["sha"], "a" * 40)
        self.assertEqual(manifest["target"], "x86_64-unknown-linux-gnu")
        self.assertEqual(manifest["built_at"], "2026-10-07T00:00:00Z")
        self.assertEqual([b["name"] for b in manifest["bins"]], ["aiua", "philote"])
        self.assertEqual(manifest["bins"][0]["sha256"], hashlib.sha256(b"aiua-binary").hexdigest())
        sums = (self.root / "SHA256SUMS").read_text()
        self.assertIn(f"{hashlib.sha256(b'philote-binary').hexdigest()}  bin/philote", sums)

    def test_sums_are_sha256sum_compatible(self):
        self.generate()
        tool = ["sha256sum", "-c", "--quiet", "SHA256SUMS"]
        try:
            res = subprocess.run(tool, cwd=self.root, capture_output=True, text=True)
        except FileNotFoundError:
            self.skipTest("sha256sum not installed")
        self.assertEqual(res.returncode, 0, res.stdout + res.stderr)

    def test_generate_fails_on_missing_required(self):
        rc, _, err = self.generate("--require", "aiua", "model-router")
        self.assertEqual(rc, 1)
        self.assertIn("model-router", err)
        self.assertFalse((self.root / "manifest.json").exists())

    def test_verify_ok_then_detects_tamper_and_extra(self):
        self.generate()
        self.assertEqual(quiet(rm.main, ["verify", "--dir", str(self.root)])[0], 0)
        (self.root / "bin" / "aiua").write_bytes(b"tampered")
        rc, _, err = quiet(rm.main, ["verify", "--dir", str(self.root)])
        self.assertEqual(rc, 1)
        self.assertIn("hash mismatch: aiua", err)

    def test_verify_detects_extra_and_missing(self):
        self.generate()
        (self.root / "bin" / "stowaway").write_bytes(b"x")
        (self.root / "bin" / "philote").unlink()
        rc, _, err = quiet(rm.main, ["verify", "--dir", str(self.root)])
        self.assertEqual(rc, 1)
        self.assertIn("missing binary: philote", err)
        self.assertIn("binary not in manifest: stowaway", err)

    def test_parse_sums_handles_paths_and_binary_marker(self):
        parsed = rm.parse_sums("ABC  /opt/philotic/releases/v1/bin/aiua\ndef *bin/philote\n\n# c\n")
        self.assertEqual(parsed, {"aiua": "abc", "philote": "def"})

    def test_compare_statuses(self):
        manifest = {"tag": "v1.0.0", "version": "1.0.0", "sha": "f" * 40, "target": "t",
                    "bins": [{"name": "a", "sha256": "1" * 64}, {"name": "b", "sha256": "2" * 64},
                             {"name": "c", "sha256": "3" * 64}, {"name": "d", "sha256": "4" * 64}]}
        actual = {"a": "1" * 64, "b": "9" * 64, "c": "8" * 64}
        installed = {"b": "9" * 64}
        rows, ok = rm.compare(manifest, actual, installed)
        self.assertFalse(ok)
        self.assertEqual([r[3] for r in rows], ["PASS", "RESIGNED", "FAIL", "MISSING"])
        rows, ok = rm.compare(manifest, {**actual, "c": "3" * 64, "d": "4" * 64}, installed)
        self.assertTrue(ok)

    def test_compare_cli_exit_codes(self):
        self.generate()
        manifest_path = self.root / "manifest.json"
        actual = Path(self.tmp.name) / "actual.txt"
        actual.write_text(
            subprocess.run(["sha256sum", "bin/aiua", "bin/philote"], cwd=self.root,
                           capture_output=True, text=True, check=True).stdout
        )
        rc, out, _ = quiet(rm.main, ["compare", "--manifest", str(manifest_path), "--actual", str(actual), "--label", "vps-jane"])
        self.assertEqual(rc, 0, out)
        self.assertIn("PASS", out)
        actual.write_text(actual.read_text().splitlines()[0] + "\n")
        rc, out, _ = quiet(rm.main, ["compare", "--manifest", str(manifest_path), "--actual", str(actual)])
        self.assertEqual(rc, 1)
        self.assertIn("MISSING", out)


if __name__ == "__main__":
    unittest.main()
