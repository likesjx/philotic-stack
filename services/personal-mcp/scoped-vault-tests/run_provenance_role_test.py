"""Exercise the actual role gate with a temporary systemctl stub; no services."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parents[3]
with tempfile.TemporaryDirectory(prefix='percival-provenance-') as directory:
    fixture = Path(directory)
    approved = fixture / 'approved-aiua'
    approved.write_bytes(b'public synthetic approved executable')
    approved.chmod(0o755)
    legacy = fixture / 'legacy-aiua'
    legacy.write_bytes(b'public synthetic legacy executable')
    legacy.chmod(0o755)
    link = fixture / 'versioned-current'
    link.symlink_to(approved)
    stub = fixture / 'systemctl'
    stub.write_text('#!' + sys.executable + '\nimport os, sys, pathlib\n'
                    "args=sys.argv[1:]\n"
                    "if 'restart' in args: sys.exit(1)\n"
                    "if 'stop' in args: pathlib.Path(os.environ['PERCIVAL_SYNTHETIC_STOP_MARKER']).touch(); sys.exit(0)\n"
                    "if 'daemon-reload' in args: sys.exit(0)\n"
                    "if not any(a.startswith('--property=') for a in args): print('LoadState=loaded\\nActiveState=active'); sys.exit(0)\n"
                    "print('ExecStart={ path=' + os.environ['PERCIVAL_SYNTHETIC_EXECUTABLE'] + ' ; argv[]=aiua ; ignore_errors=no ; }')\n"
                    "print('MainPID=0')\nprint('NeedDaemonReload=no')\n")
    stub.chmod(0o755)
    config = fixture / 'ansible.cfg'
    config.write_text('[defaults]\nlocal_tmp=' + str(fixture / 'local') + '\nremote_tmp=' + str(fixture / 'remote') + '\n')
    variables = fixture / 'inputs.json'
    variables.write_text(json.dumps({
        'fixture_path': str(fixture) + ':' + os.environ['PATH'],
        'fixture_stop_marker': str(fixture / 'gateway-stopped'),
        'fixture_legacy': str(legacy), 'fixture_approved_link': str(link),
        'percival_hotel_binary': str(approved),
        'percival_hotel_sha256': hashlib.sha256(approved.read_bytes()).hexdigest(),
        'ansible_python_interpreter': sys.executable,
    }))
    env = dict(os.environ, ANSIBLE_CONFIG=str(config),
               ANSIBLE_LIBRARY=str(root / 'ansible/library'),
               ANSIBLE_MODULE_UTILS=str(root / 'ansible/module_utils'))
    subprocess.run(['ansible-playbook', '-i', 'localhost,',
                    str(Path(__file__).with_name('provenance_role_test.yml')),
                    '-e', '@' + str(variables)], env=env, check=True, timeout=60)
