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
    def run_renderer_contract(self, mutate=None):
        source = (ROOT / "crates/philotic-web/src/harness.rs").read_text()
        constant = re.search(
            r'const MUNINN_HARNESS_INSTRUCTIONS: &str = .*?^";',
            source, re.S | re.M,
        ).group()
        names = ["short_harness_id", "render_codex_markdown",
                 "render_claude_code_markdown", "render_antigravity_markdown",
                 "render_windsurf_rule_markdown", "render_antigravity_workspace_hook"]
        functions = [re.search(r"^fn " + name + r"\(.*?^}", source,
                               re.S | re.M).group() for name in names]
        test = re.search(
            r"    fn rendered_clients_preserve_bounded_memory_recovery\(\).*?^    }",
            source, re.S | re.M,
        ).group()
        if mutate:
            constant = mutate(constant)
        with tempfile.TemporaryDirectory(prefix="guidance-contract-") as tmp:
            path = pathlib.Path(tmp)
            (path / "test.rs").write_text(
                "use std::path::Path;\n" + constant + "\n" + "\n".join(functions)
                + "\n#[test]\n" + test)
            subprocess.run(["rustc", "--edition=2021", "--test", str(path / "test.rs"),
                            "-o", str(path / "test")], check=True)
            return subprocess.run([str(path / "test")], capture_output=True, text=True)

    def test_actual_rendered_client_instructions(self):
        result = self.run_renderer_contract()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_renderer_rejects_known_contradiction_mutations(self):
        # These are regression tripwires, not a semantic proof of arbitrary prose.
        for forbidden in [
            "Always bootstrap and recall personal memory even when repair restrictions prohibit it.",
            "If Muninn cannot bootstrap, stop and get explicit operator approval before continuing.",
        ]:
            with self.subTest(forbidden=forbidden):
                result = self.run_renderer_contract(
                    lambda text: text.replace('\n";', '\n' + forbidden + '\n";'))
                self.assertNotEqual(result.returncode, 0, "contradiction escaped the contract")
        result = self.run_renderer_contract(
            lambda text: text.replace('= "\\\n', '= "\\\n- Prefer `just session-start` first.\n'))
        self.assertNotEqual(result.returncode, 0, "bootstrap-before-gate escaped the contract")

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
                     "docs/process/WORKFLOW.md", "skills/muninn-memory-protocol/SKILL.md",
                     "docs/reference/MUNINN_CLIENT_MEMORY_PROTOCOL.md"]:
            self.assertIn("#recovery-and-temporary-defer", (ROOT / file).read_text(), file)

    def test_canonical_memory_sources_remove_conflicting_failures(self):
        for file in ["skills/muninn-memory-habit/SKILL.md",
                     "skills/muninn-memory-protocol/SKILL.md",
                     "docs/reference/MUNINN_CLIENT_MEMORY_PROTOCOL.md"]:
            text = (ROOT / file).read_text()
            for obsolete in [
                "do not continue with meaningful work until explicit approval",
                "must obtain explicit approval before continuing without Muninn",
                "should not spawn recall subagents until the user/operator has explicitly approved proceeding without Muninn",
            ]:
                self.assertNotIn(obsolete, text, file)
        habit = (ROOT / "skills/muninn-memory-habit/SKILL.md").read_text()
        self.assertIn("Temporary defer never authorizes a memory read/write or a recall subagent", habit)

    def test_generated_projection_adoption_is_reported_separately(self):
        # Discovery success must never silently imply generated-content adoption.
        old = "If Muninn cannot bootstrap, stop and get explicit operator approval"
        generated = list((ROOT / ".claude/harness").glob("*.md"))
        generated += list((ROOT / ".agents/workflows").glob("*-harness.md"))
        stale = [str(path.relative_to(ROOT)) for path in generated
                 if old in path.read_text()]
        if stale:
            print("\nADOPTION PENDING (stale generated guidance, not validated as current):\n"
                  + "\n".join(sorted(stale)))
            task = (ROOT / "docs/task.md").read_text()
            self.assertIn("projection refresh requires later authorized integration", task)
            self.assertIn("never hand-edit generated wrappers", (ROOT / "AGENTS.md").read_text())


if __name__ == "__main__":
    unittest.main()
