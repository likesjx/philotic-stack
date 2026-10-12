"""Run native synthetic acceptance only in a disposable Linux container."""
import os
from pathlib import Path
import subprocess
import sys

assert sys.platform == 'linux' and os.geteuid() == 0
assert Path('/.dockerenv').is_file()
assert os.environ.get('PERCIVAL_ISOLATED_LINUX_FIXTURE') == '1'
fixtures = Path(__file__).resolve().parent
assert Path('/hotel-target/debug/aiua').is_file()
tests = [p for p in Path('/test-target/debug/deps').glob('percival_scoped_vault_tests-*')
         if p.is_file() and os.access(p, os.X_OK) and p.suffix == '']
assert len(tests) == 1, 'Expected one source harness test executable'

def admitted_uid():
    os.setgroups([])
    os.setgid(1234)
    os.setuid(1234)

# Minimal child environment: no CI token or ambient vault/keychain configuration.
env = {'PATH': '/usr/bin:/bin', 'PERCIVAL_ISOLATED_LINUX_FIXTURE': '1',
       'PERCIVAL_BOOTSTRAP_FIXTURES': str(fixtures)}
subprocess.run([str(tests[0]), '--test-threads=1'], env=env,
               preexec_fn=admitted_uid, check=True, timeout=180)
for script in ('activation_exec_acceptance.py', 'linux_acceptance.py',
               'hotel_bootstrap_acceptance.py'):
    subprocess.run([sys.executable, str(fixtures / script)], env=env,
                   check=True, timeout=240)
