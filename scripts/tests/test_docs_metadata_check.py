"""Unit tests for scripts/docs-metadata-check.py status-vocabulary checks.

Run: python3 -m unittest discover -s scripts/tests -p 'test_*.py'
"""
from __future__ import annotations

import contextlib
import importlib.util
import io
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "docs-metadata-check.py"
_spec = importlib.util.spec_from_file_location("docs_metadata_check", SCRIPT)
dmc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(dmc)

DEFECTS_HEADER = (
    "# Defects\n\n"
    "| ID | Title | Severity | Status | Pts | Found | Fixed by |\n"
    "|---|---|---|---|---|---|---|\n"
)


def proposal(status: str, disposition: str | None = None) -> str:
    lines = ["---", "title: T", "doc_type: proposal", f"status: {status}"]
    if disposition is not None:
        lines.append(f"disposition: {disposition}")
    lines += ["tags:", "- x", "---", "", "# T", ""]
    return "\n".join(lines)


class ProposalStatusTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self._tmp.name)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def write(self, name: str, text: str) -> None:
        (self.dir / name).write_text(text)

    def test_known_statuses_pass(self) -> None:
        for i, status in enumerate(sorted(dmc.PROPOSAL_STATUSES)):
            self.write(f"OK{i}_PROPOSAL.md", proposal(status))
        errors, warnings = dmc.check_proposal_statuses(self.dir)
        self.assertEqual(errors, [])
        self.assertEqual(warnings, [])

    def test_unknown_and_misspelled_statuses_fail(self) -> None:
        self.write("A_PROPOSAL.md", proposal("in_progress"))
        self.write("B_PROPOSAL.md", proposal("draft"))
        self.write("C_PROPOSAL.md", proposal("accepted for current slice"))
        errors, _ = dmc.check_proposal_statuses(self.dir)
        self.assertEqual(len(errors), 3)
        self.assertTrue(any("A_PROPOSAL.md" in e and "in_progress" in e for e in errors))

    def test_quoted_status_is_accepted(self) -> None:
        self.write("Q_PROPOSAL.md", proposal('"implemented"'))
        errors, _ = dmc.check_proposal_statuses(self.dir)
        self.assertEqual(errors, [])

    def test_non_proposal_and_frontmatterless_docs_are_skipped(self) -> None:
        self.write("NOTES.md", proposal("whatever"))
        self.write("BARE_PROPOSAL.md", "# no frontmatter\n")
        errors, warnings = dmc.check_proposal_statuses(self.dir)
        self.assertEqual((errors, warnings), ([], []))

    def test_contradictory_disposition_warns(self) -> None:
        self.write("X_PROPOSAL.md", proposal("proposed", "implemented"))
        errors, warnings = dmc.check_proposal_statuses(self.dir)
        self.assertEqual(errors, [])
        self.assertEqual(len(warnings), 1)
        self.assertIn("contradicts", warnings[0])

    def test_adjacent_or_spelling_variant_disposition_does_not_warn(self) -> None:
        self.write("Y_PROPOSAL.md", proposal("proposed", "accepted-current-slice"))
        self.write("Z_PROPOSAL.md", proposal("accepted-current-slice", "accepted for current slice"))
        self.write("W_PROPOSAL.md", proposal("proposed", "deferred"))
        _, warnings = dmc.check_proposal_statuses(self.dir)
        self.assertEqual(warnings, [])


class DefectStatusTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.path = Path(self._tmp.name) / "DEFECTS.md"

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_vocabulary_rows_pass(self) -> None:
        rows = "".join(
            f"| DEF-{i:03d} | t | low | {status} | 1 | 2026-10-01 | x |\n"
            for i, status in enumerate(sorted(dmc.DEFECT_STATUSES))
        )
        self.path.write_text(DEFECTS_HEADER + rows)
        self.assertEqual(dmc.check_defect_statuses(self.path), [])

    def test_compound_status_fails(self) -> None:
        self.path.write_text(
            DEFECTS_HEADER
            + "| DEF-001 | t | low | fixed (pending live confirmation) | 1 | 2026-08-01 | x |\n"
            + "| DEF-002 | t | low | fixed | 1 | 2026-08-01 | x |\n"
        )
        errors = dmc.check_defect_statuses(self.path)
        self.assertEqual(len(errors), 2)
        self.assertIn("DEF-001", errors[0])

    def test_escaped_pipes_in_title_do_not_shift_columns(self) -> None:
        self.path.write_text(
            DEFECTS_HEADER + "| DEF-003 | a ` \\| kind=x \\| ` b | high | open | 1 | 2026-10-01 | y |\n"
        )
        self.assertEqual(dmc.check_defect_statuses(self.path), [])


class MainExitCodeTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        arch = self.root / "docs" / "architecture"
        arch.mkdir(parents=True)
        (arch / "BAD_PROPOSAL.md").write_text(proposal("draft"))
        self._saved = dmc.REQUIRED_DOCS
        dmc.REQUIRED_DOCS = {}

    def tearDown(self) -> None:
        dmc.REQUIRED_DOCS = self._saved
        self._tmp.cleanup()

    def run_main(self, *args: str) -> int:
        with contextlib.redirect_stderr(io.StringIO()), contextlib.redirect_stdout(io.StringIO()):
            return dmc.main(["--root", str(self.root), *args])

    def test_vocabulary_errors_are_fatal_by_default(self) -> None:
        self.assertEqual(self.run_main(), 1)

    def test_warn_only_makes_vocabulary_errors_non_fatal(self) -> None:
        self.assertEqual(self.run_main("--warn-only"), 0)

    def test_warn_only_keeps_required_key_failures_fatal(self) -> None:
        dmc.REQUIRED_DOCS = {"docs/MISSING.md": {"title"}}
        self.assertEqual(self.run_main("--warn-only"), 1)


if __name__ == "__main__":
    unittest.main()
