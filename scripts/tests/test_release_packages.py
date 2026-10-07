"""Tests for release/packages.toml and scripts/release-packages.sh.

Run: python3 -m unittest discover -s scripts/tests -p 'test_*.py'
"""

import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "release-packages.sh"
ANSIBLE_DEFAULTS = ROOT / "ansible" / "roles" / "philotic_hotel" / "defaults" / "main.yml"


def run(*args, env=None, check=True):
    return subprocess.run(
        ["bash", str(SCRIPT), *args],
        capture_output=True,
        text=True,
        env={**os.environ, **(env or {})},
        check=check,
    )


def lines(*args):
    return [l for l in run(*args).stdout.splitlines() if l.strip()]


def ansible_binaries():
    out, active = [], False
    for line in ANSIBLE_DEFAULTS.read_text().splitlines():
        if line.startswith("philotic_binaries:"):
            active = True
            continue
        if active:
            m = re.match(r"^\s+-\s+(\S+)\s*$", line)
            if m:
                out.append(m.group(1))
            elif line.strip() and not line.lstrip().startswith("#"):
                break
    return out


class ReleasePackagesTest(unittest.TestCase):
    def test_cargo_flags_shape(self):
        flags = run("cargo-flags", "--platform", "linux").stdout.split()
        self.assertGreater(len(flags), 0)
        self.assertEqual(flags[0::2], ["-p"] * (len(flags) // 2))
        self.assertIn("aiua", flags[1::2])
        self.assertIn("philotic-web", flags[1::2])

    def test_packages_match_cargo_flags(self):
        flags = run("cargo-flags", "--platform", "linux").stdout.split()
        self.assertEqual(flags[1::2], lines("packages", "--platform", "linux"))

    def test_required_subset_of_bins(self):
        for platform in ("linux", "darwin"):
            bins = set(lines("bins", "--platform", platform))
            required = set(lines("required-bins", "--platform", platform))
            self.assertTrue(required <= bins, required - bins)

    def test_core_bins_present(self):
        bins = set(lines("bins"))
        for b in ("aiua", "philote", "philote-worker", "life-graph-runner", "philotic-web"):
            self.assertIn(b, bins)

    def test_ansible_binaries_are_shipped(self):
        names = ansible_binaries()
        self.assertGreater(len(names), 10, "could not parse philotic_binaries")
        shipped = set(lines("bins", "--platform", "linux"))
        missing = [n for n in names if n not in shipped]
        self.assertEqual(missing, [], "ansible philotic_binaries not in release/packages.toml")
        self.assertNotIn("agent-core", names)
        self.assertNotIn("hegemon", names)

    def test_workflows_read_the_manifest(self):
        # Workflow edits may ship as patches under docs/process/workflow-patches/
        # (the cloud GitHub App cannot push .github/workflows). Skip until applied.
        pending = []
        for wf in ("build-linux.yml", "pr-check.yml", "release.yml"):
            text = (ROOT / ".github" / "workflows" / wf).read_text()
            if "scripts/release-packages.sh" not in text:
                pending.append(wf)
        if pending:
            self.skipTest(f"workflow patch not applied yet: {', '.join(pending)}")

    def test_bad_input_exits_2(self):
        self.assertEqual(run("bogus", check=False).returncode, 2)
        self.assertEqual(run("bins", "--platform", "windows", check=False).returncode, 2)

    def test_platform_filter_and_validation(self):
        with tempfile.TemporaryDirectory() as td:
            manifest = Path(td) / "p.toml"
            manifest.write_text(
                '[[package]]\nname = "a"\nlinux = true\ndarwin = false\nbins = ["a1"]\nrequired = ["a1"]\n'
                '[[package]]\nname = "b"\nlinux = false\ndarwin = true\nbins = ["b1", "b2"]\nrequired = []\n'
            )
            env = {"PHILOTIC_RELEASE_PACKAGES": str(manifest)}
            self.assertEqual(run("cargo-flags", "--platform", "linux", env=env).stdout.strip(), "-p a")
            self.assertEqual(run("bins", "--platform", "darwin", env=env).stdout.split(), ["b1", "b2"])
            self.assertEqual(run("required-bins", env=env).stdout.split(), ["a1"])

            manifest.write_text('[[package]]\nname = "a"\nlinux = true\nbins = ["x"]\nrequired = ["y"]\n')
            res = run("bins", env=env, check=False)
            self.assertNotEqual(res.returncode, 0)
            self.assertIn("required bins not in bins", res.stderr)


if __name__ == "__main__":
    unittest.main()
