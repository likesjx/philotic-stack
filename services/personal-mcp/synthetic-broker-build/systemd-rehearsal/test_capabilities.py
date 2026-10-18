"""Packaged contract tests; no claim to emulate Linux/systemd execution."""
import contextlib
import io
import pathlib
import tarfile
import types
import unittest
from unittest.mock import patch

HERE = pathlib.Path(__file__).resolve().parent
with tarfile.open(HERE/'vps-broker-dummy-e43bb385-r3.tar.gz') as archive:
    source = archive.extractfile('root/opt/muninn-claude-synthetic/validate.py').read()
    unit = archive.extractfile('muninn-broker-dummy-e43bb385-r3.service').read().decode()
validator = types.ModuleType('packaged_validator')
exec(compile(source, 'packaged_validate.py', 'exec'), validator.__dict__)
EXPECTED = 0x2000e3

def status(**changes):
    values = dict(CapEff=EXPECTED, CapPrm=EXPECTED, CapBnd=EXPECTED, CapInh=0, CapAmb=0)
    values.update(changes)
    return '\n'.join(f'{key}: {value:x}' for key,value in values.items())+'\nNoNewPrivs: 1'

class CapabilityContract(unittest.TestCase):
    def check(self, text):
        with contextlib.redirect_stdout(io.StringIO()), patch.object(validator.pathlib.Path, 'read_text', return_value=text):
            return validator.check_supervisor_caps()

    def test_exact_five_sets_pass(self):
        self.assertEqual(self.check(status()), hex(EXPECTED))

    def test_each_required_set_rejects_missing_and_extra_bits(self):
        for key in ('CapEff','CapPrm','CapBnd'):
            for value in (EXPECTED & ~(1<<7), EXPECTED | (1<<19)):
                with self.subTest(key=key,value=value), self.assertRaises(AssertionError):
                    self.check(status(**{key:value}))

    def test_inheritable_and_ambient_must_each_be_empty(self):
        for key in ('CapInh','CapAmb'):
            with self.subTest(key=key), self.assertRaises(AssertionError):
                self.check(status(**{key:1<<21}))

    def test_observed_failed_run_is_rejected(self):
        with self.assertRaises(AssertionError):
            self.check(status(CapEff=0x200063,CapPrm=0x200063,CapInh=0x200000))

    def test_privilege_drop_order_and_failure_stop(self):
        calls=[]
        with patch.object(validator.os,'setgroups',side_effect=lambda x:calls.append(('groups',x))), patch.object(validator.os,'setgid',side_effect=lambda x:calls.append(('gid',x))), patch.object(validator.os,'setuid',side_effect=lambda x:calls.append(('uid',x))):
            validator.drop()
        self.assertEqual(calls,[('groups',[]),('gid',987),('uid',999)])
        with patch.object(validator.os,'setgroups',side_effect=PermissionError), patch.object(validator.os,'setgid') as gid, patch.object(validator.os,'setuid') as uid:
            with self.assertRaises(PermissionError): validator.drop()
            gid.assert_not_called(); uid.assert_not_called()
        with patch.object(validator.os,'setgroups'), patch.object(validator.os,'setgid',side_effect=PermissionError), patch.object(validator.os,'setuid') as uid:
            with self.assertRaises(PermissionError): validator.drop()
            uid.assert_not_called()

    def test_unit_preserves_sandbox_and_uses_system_default_uid(self):
        config=dict(line.split('=',1) for line in unit.splitlines() if '=' in line and not line.startswith('#'))
        self.assertNotIn('User',config)
        self.assertEqual(config['Group'],'root')
        self.assertEqual(config['CapabilityBoundingSet'],'CAP_SYS_ADMIN CAP_SETUID CAP_SETGID CAP_CHOWN CAP_DAC_OVERRIDE CAP_KILL')
        self.assertEqual(config['AmbientCapabilities'],'')
        for key in ('NoNewPrivileges','PrivateNetwork','PrivateDevices','ProtectKernelTunables','ProtectKernelModules','ProtectControlGroups'):
            self.assertEqual(config[key],'yes')
        self.assertEqual(config['RestrictAddressFamilies'],'AF_UNIX')
        self.assertEqual(config['ProtectSystem'],'strict')

if __name__ == '__main__': unittest.main()
