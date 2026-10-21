"""Execute exact recipe bash bodies with an Ansible stub; never connect."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

from memory_activation_contract_test import inputs, c

ROOT = Path(__file__).resolve().parents[3]


class Entry(unittest.TestCase):
    def run_recipe(self, name, public, approval=None):
        with tempfile.TemporaryDirectory(prefix='memory-entry-') as directory:
            fixture = Path(directory)
            input_file = fixture/'public.json'
            input_file.write_text(json.dumps(public))
            marker = fixture/'called.json'
            stub = fixture/'ansible-playbook'
            stub.write_text('#!/usr/bin/env python3\nimport json,os,sys\nfrom pathlib import Path\n'
                'Path(__file__).with_name("public.json").write_text("REPLACED AFTER VALIDATION")\n'
                'Path(__file__).with_name("called.json").write_text(json.dumps({"argv":sys.argv[1:],"env":dict(os.environ)}))\n')
            stub.chmod(0o755)
            source = (ROOT/'justfile').read_text()
            match = re.search('^'+re.escape(name)+r'[^\n]*:\n((?:    [^\n]*\n)+)', source, re.M)
            self.assertIsNotNone(match)
            body = '\n'.join(line[4:] for line in match.group(1).splitlines())
            self.assertNotRegex(str(ROOT), r'["$`\\\n]')
            body = body.replace('{{justfile_directory()}}', str(ROOT))
            self.assertNotIn('{{', body)
            env = dict(os.environ, PATH=str(fixture)+':'+os.environ['PATH'], HOME=str(fixture),
                       SSH_AUTH_SOCK='/PUBLIC-UNUSED-AGENT', ANSIBLE_VAULT_PASSWORD_FILE='/PUBLIC-UNUSED-VAULT',
                       PUBLIC_AMBIENT_CANARY='DO_NOT_INHERIT')
            args = ['bash','-s','--',str(input_file)] + ([] if approval is None else [approval])
            result = subprocess.run(args, input=body, text=True, stdout=subprocess.PIPE,
                                    stderr=subprocess.PIPE, env=env, timeout=20)
            return result, json.loads(marker.read_text()) if marker.exists() else None

    def assert_fixed_target(self, captured, profile, expected):
        self.assertIsNotNone(captured)
        args = captured['argv']
        positions = [i for i,x in enumerate(args) if x == '--extra-vars']
        self.assertEqual(len(positions), 2)
        self.assertEqual(json.loads(args[positions[0]+1]), expected)
        self.assertFalse(args[positions[0]+1].startswith('@'))
        fixed = json.loads(args[positions[1]+1])
        self.assertEqual(fixed['ansible_host'], '31.97.130.98')
        self.assertEqual(fixed['ansible_connection'], 'ssh')
        self.assertEqual(fixed['percival_profile'], profile)
        self.assertEqual(fixed['percival_memory_fixture_root'], '')
        self.assertEqual(captured['env']['ANSIBLE_CONFIG'], '/dev/null')
        self.assertEqual(captured['env']['ANSIBLE_HOST_KEY_CHECKING'], 'True')
        self.assertEqual(captured['env']['ANSIBLE_VARS_ENABLED'], '')
        for key in ('SSH_AUTH_SOCK','ANSIBLE_VAULT_PASSWORD_FILE','PUBLIC_AMBIENT_CANARY'):
            self.assertNotIn(key, captured['env'])
        ssh = args[args.index('--ssh-common-args')+1]
        self.assertIn('-F /dev/null', ssh)
        self.assertIn('StrictHostKeyChecking=yes', ssh)
        self.assertIn('ForwardAgent=no', ssh)
        self.assertIn('ControlMaster=no', ssh)

    def test_stage_uses_closed_snapshot_and_no_shared_config(self):
        public = {'percival_memory_package_dir':'/tmp/public-package',
                  'percival_memory_config_file':'/tmp/public-config.json',
                  'percival_memory_config_sha256':'a'*64}
        result, captured = self.run_recipe('muninn-memory-stage', public)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_fixed_target(captured, 'memory-stage-v1', public)

    def test_outer_connection_injection_stops_before_ansible(self):
        public = {'percival_memory_package_dir':'/tmp/public-package',
                  'percival_memory_config_file':'/tmp/public-config.json',
                  'percival_memory_config_sha256':'a'*64, 'ansible_host':'PUBLIC-INJECTED'}
        result, captured = self.run_recipe('muninn-memory-stage', public)
        self.assertNotEqual(result.returncode, 0)
        self.assertIsNone(captured)
        self.assertNotIn('PUBLIC-INJECTED', result.stdout+result.stderr)

    def test_activation_passes_only_exact_validated_public_snapshot(self):
        manifest, qualification, _ = inputs()
        now = int(time.time())
        manifest.update(approved_at=now, expires_at=now+600)
        manifest['native_key_attestation'].update(verified_at=now, expires_at=now+3600)
        manifest['operator_sha256'] = {name:hashlib.sha256((ROOT/name).read_bytes()).hexdigest() for name in c.OPERATOR_FILES}
        approval = c.digest(manifest)
        public = {'percival_memory_manifest':manifest,'percival_memory_qualification':qualification,
                  'percival_memory_approval_sha256':approval}
        result, captured = self.run_recipe('muninn-memory-activate', public, approval)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_fixed_target(captured, 'memory-activate-v1', public)
        bad, called = self.run_recipe('muninn-memory-activate', public, '0'*64)
        self.assertNotEqual(bad.returncode, 0)
        self.assertIsNone(called)


def vars_plugin_fixture():
    """Positive/negative real loader controls in disposable public files only."""
    assert sys.platform == 'linux' and os.geteuid() == 0 and Path('/.dockerenv').is_file()
    assert os.environ.get('PERCIVAL_ISOLATED_LINUX_FIXTURE') == '1'
    executable = shutil.which('ansible-playbook')
    assert executable, 'Real Ansible is required; never substitute a fixture.'
    public = {'percival_memory_package_dir':'/tmp/public-package',
              'percival_memory_config_file':'/tmp/public-config.json',
              'percival_memory_config_sha256':'a'*64}
    result, capture = Entry().run_recipe('muninn-memory-stage', public)
    assert result.returncode == 0
    assert capture['env']['ANSIBLE_VARS_ENABLED'] == ''
    with tempfile.TemporaryDirectory(prefix='memory-vars-loader-') as directory:
        fixture = Path(directory)
        play = fixture/'public-play.json'
        play.write_text(json.dumps([{'hosts':'jane-vps','connection':'local','gather_facts':False,
                                    'tasks':[{'ansible.builtin.assert':{'that':['true']}}]}]))
        env = dict(capture['env'], HOME=str(fixture), PATH=os.environ['PATH'])
        for relative in ('host_vars/jane-vps.yml','group_vars/all.yml'):
            sentinel = fixture/relative
            sentinel.parent.mkdir(exist_ok=True)
            sentinel.write_text('[ PUBLIC-SYNTHETIC-INVALID-YAML\n')
            for enabled, success in (('host_group_vars',False),('',True)):
                observed = subprocess.run([executable,'-i','jane-vps,',str(play)],
                    cwd=fixture, env=dict(env, ANSIBLE_VARS_ENABLED=enabled),
                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=45)
                assert (observed.returncode == 0) == success, 'Public vars loader isolation failed.'
                print('PASS actual vars loader '+relative+': '+('disabled' if success else 'positive control'))
            sentinel.unlink()


if __name__ == '__main__':
    if sys.argv[1:] == ['--ansible-vars-fixture']:
        vars_plugin_fixture()
    else:
        unittest.main()
