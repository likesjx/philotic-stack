#!/usr/bin/env python3
"""Execute the real Ansible role in an explicitly disposable Linux container.

Only public source files and public fixture configuration are used. The Node
input must match the qualified package digest; it is copied, never executed here.
No systemd, remote hosts, credentials or production files are accessed.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def tree(root):
    result = {}
    for path in sorted(root.rglob('*')):
        relative = str(path.relative_to(root))
        if path.is_symlink():
            result[relative] = ('link', os.readlink(path))
        elif path.is_file():
            result[relative] = ('file', digest(path), path.stat().st_mode & 0o777)
        else:
            result[relative] = ('directory', path.stat().st_mode & 0o777)
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--node', required=True)
    args = parser.parse_args()
    if (sys.platform != 'linux' or os.geteuid() != 0 or not Path('/.dockerenv').exists() or
            os.environ.get('PERCIVAL_ISOLATED_LINUX_FIXTURE') != '1'):
        raise SystemExit('Requires explicit root disposable Linux container fixture.')
    import yaml  # Installed alongside Ansible in the disposable CI container only.
    repo = Path(__file__).resolve().parents[3]
    role = repo / 'ansible/roles/percival_personal_mcp'
    tasks = yaml.safe_load((role / 'tasks/memory_stage.yml').read_text())
    contract = next(task['vars'] for task in tasks if 'vars' in task)
    hashes = contract['memory_files']
    node = Path(args.node)
    assert node.is_file() and not node.is_symlink()
    assert digest(node) == hashes['bin/node'], 'Qualified Node byte drift; no substitution allowed.'
    assert node.read_bytes()[:4] == b'\x7fELF'
    ansible = shutil.which('ansible-playbook')
    assert ansible, 'Ansible required; this fixture never installs it.'

    with tempfile.TemporaryDirectory(prefix='muninn-memory-input-', dir='/tmp') as inputs, tempfile.TemporaryDirectory(prefix='muninn-memory-stage-', dir='/tmp') as target:
        inputs, target = Path(inputs), Path(target)
        target.chmod(0o755)
        package = inputs / 'package'
        (package / 'gateway').mkdir(parents=True)
        (package / 'bin').mkdir()
        for name, expected in hashes.items():
            source = node if name == 'bin/node' else repo / 'services/personal-mcp' / name[8:]
            assert digest(source) == expected, 'Candidate source drift: ' + name
            shutil.copyfile(source, package / name)
        config = inputs / 'gateway.json'
        config.write_text(json.dumps(contract['memory_expected_config'], sort_keys=True) + '\n')
        approved_config = config.read_bytes()
        # Sentinels represent surrounding live surfaces, never actual /etc paths.
        for name in ('live-gateway.json', 'broker-policy.json', 'accounts', 'gateway.service', 'hotel.service', 'vault.socket'):
            (target / name).write_text('unchanged synthetic sentinel\n')
        (target / 'current').symlink_to('unchanged-release')
        sentinels = tree(target)
        play = inputs / 'play.json'
        play.write_text(json.dumps([dict(hosts='localhost', connection='local', gather_facts=False,
                                        roles=[dict(role=str(role))])]))
        ansible_config = inputs / 'ansible.cfg'
        ansible_config.write_text('[defaults]\nretry_files_enabled = False\ninterpreter_python = auto_silent\n')
        env = {'PATH': os.environ['PATH'], 'HOME': str(inputs), 'LANG': 'C.UTF-8',
               'ANSIBLE_CONFIG': str(ansible_config), 'ANSIBLE_LOCAL_TEMP': str(inputs / 'ansible-local'),
               'ANSIBLE_REMOTE_TEMP': str(inputs / 'ansible-remote'),
               'ANSIBLE_LIBRARY': str(repo / 'ansible/library'),
               'ANSIBLE_MODULE_UTILS': str(repo / 'ansible/module_utils'),
               'PERCIVAL_ISOLATED_LINUX_FIXTURE': '1'}
        base_vars = dict(percival_enabled=True, percival_profile='memory-stage-v1',
                         percival_memory_package_dir=str(package), percival_memory_config_file=str(config),
                         percival_memory_config_sha256=digest(config), percival_memory_fixture_root=str(target))
        def run(label, success, changes=None, unchanged=False):
            before = tree(target)
            variables = inputs / 'vars.json'
            variables.write_text(json.dumps(dict(base_vars, **(changes or {}))))
            result = subprocess.run([ansible, '-i', 'localhost,', str(play), '--extra-vars', '@' + str(variables)],
                                    env=env, cwd=inputs, text=True, stdout=subprocess.PIPE,
                                    stderr=subprocess.STDOUT, timeout=180)
            if (result.returncode == 0) != success:
                print(result.stdout[-18000:])
                raise AssertionError(label + ': unexpected Ansible result')
            after = tree(target)
            if unchanged:
                assert before == after, label + ': staging tree unexpectedly changed'
            for name, original in sentinels.items():
                assert after[name] == original, label + ': live sentinel changed: ' + name
            print('PASS ' + label)

        run('disabled default creates nothing', True, {'percival_enabled': False}, unchanged=True)
        run('unknown profile rejected', False, {'percival_profile': 'unknown'}, unchanged=True)
        run('activation rejected before writes', False, {'percival_memory_operation': 'activate'}, unchanged=True)
        missing = package / 'gateway/whole-vault.mjs'
        saved = missing.read_bytes()
        missing.unlink()
        run('missing whole-vault rejected before writes', False, unchanged=True)
        missing.write_bytes(saved)
        source = package / 'gateway/server.mjs'
        original_source = source.read_bytes()
        source.write_bytes(original_source + b'\n// synthetic drift\n')
        run('source hash drift rejected before writes', False, unchanged=True)
        source.write_bytes(original_source)
        config.write_bytes(approved_config + b' ')
        run('configuration byte drift rejected before writes', False, unchanged=True)
        altered = json.loads(approved_config)
        altered['enabledTools'] = ['muninn_remember']
        config.write_text(json.dumps(altered))
        run('configuration scope expansion rejected with matching digest', False,
            {'percival_memory_config_sha256': digest(config)}, unchanged=True)
        config.write_bytes(approved_config)
        run('fresh exact inert stage', True)
        release = target / 'staged' / ('memory-v1-' + base_vars['percival_memory_config_sha256'])
        for name, expected in dict(hashes, **{'gateway.json': digest(config)}).items():
            assert digest(release / name) == expected
        run('identical replay is read-only', True, unchanged=True)
        staged = release / 'gateway/server.mjs'
        staged.write_bytes(original_source + b'\n// synthetic destination drift\n')
        run('pre-existing release drift is refused not repaired', False, unchanged=True)
        staged.write_bytes(original_source)
        extra = release / 'extra'
        extra.write_text('unexpected public file')
        run('pre-existing extra file refused', False, unchanged=True)
        extra.unlink()
        staged.unlink()
        staged.symlink_to(source)
        run('pre-existing symlink refused', False, unchanged=True)
        # Remove only fixture symlink; no role cleanup or deletion implementation.
        staged.unlink()
        staged.write_bytes(original_source)
        run('activation remains refused after exact staging', False,
            {'percival_memory_operation': 'activate'}, unchanged=True)
    print('13 real Ansible memory-stage cases passed; no activation contract exercised.')


if __name__ == '__main__':
    main()
