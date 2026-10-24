"""Offline guidance checks; no harness apply, services, credentials or memory I/O.

Compile the actual pure renderer functions and their in-source Rust regression
test in a temporary crate, avoiding the application's integration/build hooks.
"""
import pathlib
import re
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]


class GuidanceContract(unittest.TestCase):
    def test_actual_rendered_client_instructions(self):
        source = (ROOT / "crates/philotic-web/src/harness.rs").read_text()
        constant = re.search(
            r'const MUNINN_HARNESS_INSTRUCTIONS: &str = .*?^";',
            source, re.S | re.M,
        ).group()
        names = ["short_harness_id", "render_codex_markdown",
                 "render_claude_code_markdown", "render_antigravity_markdown",
                 "render_windsurf_rule_markdown"]
        functions = [re.search(r"^fn " + name + r"\(.*?^}", source,
                               re.S | re.M).group() for name in names]
        test = re.search(
            r"    fn rendered_clients_preserve_bounded_memory_recovery\(\).*?^    }",
            source, re.S | re.M,
        ).group()
        with tempfile.TemporaryDirectory(prefix="guidance-contract-") as tmp:
            path = pathlib.Path(tmp)
            (path / "test.rs").write_text(
                constant + "\n" + "\n".join(functions) + "\n#[test]\n" + test)
            subprocess.run(["rustc", "--edition=2021", "--test", str(path / "test.rs"),
                            "-o", str(path / "test")], check=True)
            subprocess.run([str(path / "test")], check=True)

    def test_claude_discovery_resolves_canonical_skills(self):
        for name in ["runtime-debugger", "verification-ladder", "runtime-rollout-watch",
                     "philotic-slice-closeout", "muninn-memory-habit"]:
            projected = ROOT / ".claude/skills" / name / "SKILL.md"
            self.assertEqual(projected.resolve(), ROOT / "skills" / name / "SKILL.md")
            self.assertTrue(projected.is_file())

    def test_no_unconditional_replacement_gate(self):
        for file in ["AGENTS.md", "skills/runtime-debugger/SKILL.md",
                     "skills/verification-ladder/SKILL.md",
                     "skills/runtime-rollout-watch/SKILL.md"]:
            source = (ROOT / file).read_text()
            for obsolete in ["verify the installed binary path actually changed",
                             "verify file timestamp or checksum changed",
                             "- the installed binary changed",
                             "Verify the installed artifact actually changed."]:
                self.assertNotIn(obsolete, source, file)

    def test_agent_bootstrap_points_to_single_recovery_contract(self):
        for file in ["AGENTS.md", "CLAUDE.md", "CODEX.md", "GEMINI.md",
                     "docs/process/WORKFLOW.md"]:
            self.assertIn("#recovery-and-temporary-defer", (ROOT / file).read_text(), file)


if __name__ == "__main__":
    unittest.main()
