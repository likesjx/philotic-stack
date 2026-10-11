"""Read-only source contract for the exact installed-baseline backport."""
import hashlib
import json
from pathlib import Path
import subprocess

BASE = '57b13ba0087722a2de062ff86710ae64a510d445'
ALLOWED = {'crates/aiua/Cargo.toml', 'crates/aiua/src/main.rs',
           'crates/aiua/src/vault.rs', 'crates/aiua/src/service/mod.rs'} | {
    'crates/aiua/src/service/' + name + '.rs' for name in
    ('scoped_vault', 'scoped_vault_activation', 'scoped_vault_resolver', 'scoped_vault_shutdown')}

def check():
    subprocess.run(['git', 'merge-base', '--is-ancestor', BASE, 'HEAD'], check=True)
    changed = subprocess.check_output(['git', 'diff', '--name-only', BASE, '--', 'crates'], text=True).splitlines()
    assert set(changed) <= ALLOWED, 'unreviewed runtime dependency changes'
    assert not subprocess.check_output(['git', 'diff', BASE, '--',
        'crates/ansible-mesh-core', 'crates/philotic-client', 'crates/model-router',
        'crates/aiua/src/service/ipc.rs', 'crates/aiua/src/service/guest_manager.rs']), 'baseline compatibility source changed'
    pins = json.loads(Path('services/personal-mcp/issuer-fixture/source-pins.json').read_text())
    for name, expected in pins['files'].items():
        assert hashlib.sha256(Path(name).read_bytes()).hexdigest() == expected, 'issuer/gateway fixture provenance mismatch'
    print('PASS installed-baseline runtime boundary and exact issuer/gateway source pins')

if __name__ == '__main__':
    check()
