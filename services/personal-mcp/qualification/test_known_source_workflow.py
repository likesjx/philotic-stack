"""Execute the exact workflow acceptance shell with a disposable fake Docker CLI."""
from pathlib import Path
import subprocess
import tempfile
import unittest

WORKFLOW = Path(__file__).resolve().parents[3] / '.github/workflows/muninn-known-source.yml'


def shell():
    text = WORKFLOW.read_text()
    block = text.split('      - name: Execute synthetic contracts without network or host mounts\n', 1)[1]
    block = block.split('      - uses:', 1)[0].split('        run: |\n', 1)[1]
    return '\n'.join(line[10:] for line in block.splitlines() if line.strip()) + '\n'


class Workflow(unittest.TestCase):
    def exercise(self, scenario):
        with tempfile.TemporaryDirectory(prefix='public-ci-shell-regression-') as td:
            root = Path(td)
            bindir = root / 'bin'
            bindir.mkdir()
            docker = bindir / 'docker'
            docker.write_text('''#!/bin/sh
set -eu
case "$1" in
create) exit 0 ;;
start) case "$SCENARIO" in container_failure|attach_failure) exit 1 ;; *) exit 0 ;; esac ;;
inspect)
 if test "${2:-}" != -f; then printf '{}\\n'; exit 0; fi
 case "$3" in
  *NetworkMode*) printf 'none\\n' ;;
  *Mounts*) printf '0\\n' ;;
  *Privileged*) printf 'false\\n' ;;
  *ExitCode*) if test "$SCENARIO" = container_failure; then printf '1\\n'; else printf '0\\n'; fi ;;
  *) exit 2 ;;
 esac ;;
cp)
 test "$SCENARIO" != copy_failure || exit 1
 : > evidence/runtime-receipt-collected ;;
*) exit 2 ;;
esac
''')
            docker.chmod(0o700)
            (root / 'evidence').mkdir()
            result = subprocess.run(['/bin/bash'], input=shell(), text=True, cwd=root,
                                    env={'PATH': str(bindir) + ':/usr/bin:/bin', 'SCENARIO': scenario},
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)
            return result.returncode, (root / 'evidence/runtime-receipt-collected').exists()

    def test_success_collects_receipts(self):
        self.assertEqual(self.exercise('success'), (0, True))

    def test_failed_container_collects_receipts_and_still_fails(self):
        code, copied = self.exercise('container_failure')
        self.assertNotEqual(code, 0)
        self.assertTrue(copied)

    def test_attach_failure_collects_receipts_and_still_fails(self):
        code, copied = self.exercise('attach_failure')
        self.assertNotEqual(code, 0)
        self.assertTrue(copied)

    def test_copy_failure_cannot_pass(self):
        code, copied = self.exercise('copy_failure')
        self.assertNotEqual(code, 0)
        self.assertFalse(copied)


if __name__ == '__main__':
    unittest.main()
