import json
import subprocess
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

import native_container_boundary_candidate as candidate


class FakeDocker:
    def __init__(self, scratch):
        self.nonce = 'a' * 32
        self.network = 'f' * 64
        self.ids = {role: f'{i:064x}' for i, role in enumerate(sorted(candidate.ROLES), 1)}
        self.images = {role: f'sha256:{i+100:064x}' for i, role in enumerate(sorted(candidate.ROLES), 1)}
        self.containers = {}
        for role, ident in self.ids.items():
            mode = f'native-qualification-{self.nonce}-network' if role in ('anchor', 'proxy') else f'container:{self.ids["anchor"]}'
            self.containers[ident] = {
                'Id': ident, 'Image': self.images[role], 'Name': f'/native-qualification-{self.nonce}-{role}',
                'Config': {'Labels': {'com.philotic.fixture': self.nonce}},
                'HostConfig': {'Privileged': False, 'PidMode': '', 'PortBindings': {},
                               'CapDrop': ['ALL'], 'SecurityOpt': ['no-new-privileges:true'],
                               'CapAdd': None, 'Devices': [], 'RestartPolicy': {'Name': 'no', 'MaximumRetryCount': 0},
                               'NetworkMode': mode},
                'State': {'Running': True, 'Pid': 123, 'Restarting': False, 'Dead': False},
                'Mounts': [{'Type': 'bind', 'Source': str(scratch)}],
            }
        self.kills = []
        self.fail_kill = None
        self.extra_id = None
        self.fail_wait = None
        self.late_restart = False
        self.extra_network_member = False
        self.final_extra_id = None
        self.timeout_wait = None
        self.lost_inspection = None

    def __call__(self, args):
        if args[:2] == ['network', 'inspect']:
            members = {self.ids['anchor']: {}, self.ids['proxy']: {}}
            if self.extra_network_member: members['d' * 64] = {}
            data = [{'Id': self.network, 'Name': f'native-qualification-{self.nonce}-network',
                     'Internal': True, 'Labels': {'com.philotic.fixture': self.nonce},
                     'Containers': members}]
            return SimpleNamespace(returncode=0, stdout=json.dumps(data))
        if args[0] == 'ps':
            extra = self.extra_id or (self.final_extra_id if len(self.kills) == len(candidate.ROLES) else None)
            return SimpleNamespace(returncode=0, stdout='\n'.join([*self.containers, *([extra] if extra else [])]))
        if args[0] == 'inspect':
            if args[1] == self.lost_inspection and args[1] in self.kills:
                return SimpleNamespace(returncode=1, stdout='')
            return SimpleNamespace(returncode=0, stdout=json.dumps([self.containers[args[1]]]))
        if args[0] == 'kill':
            ident = args[-1]
            self.kills.append(ident)
            if ident == self.fail_kill:
                return SimpleNamespace(returncode=1, stdout='')
            self.containers[ident]['State'] = {'Running': False, 'Pid': 0, 'Restarting': False, 'Dead': False}
            if self.late_restart and ident == self.ids['anchor']:
                self.containers[self.ids['runtime']]['State'] = {'Running': True, 'Pid': 456, 'Restarting': False, 'Dead': False}
            return SimpleNamespace(returncode=0, stdout='')
        if args[0] == 'wait':
            if args[-1] == self.timeout_wait:
                raise subprocess.TimeoutExpired(args, 15)
            if args[-1] == self.fail_wait:
                return SimpleNamespace(returncode=1, stdout='')
            return SimpleNamespace(returncode=0, stdout='137\n')
        raise AssertionError(args)

    def manifest(self):
        return {'nonce': self.nonce, 'network_id': self.network, 'containers': self.ids, 'images': self.images}


class FixtureBoundaryTests(unittest.TestCase):
    def test_all_exact_containers_are_stopped_and_checked(self):
        with tempfile.TemporaryDirectory() as root:
            scratch = Path(root) / f'native-qualification-{"a" * 32}'; scratch.mkdir()
            fake = FakeDocker(scratch)
            outcomes = candidate.terminate_fixture(fake.manifest(), scratch, root, fake)
            self.assertTrue(all(outcomes.values()))
            self.assertTrue(candidate.all_observed_stopped(outcomes))
            self.assertEqual(len(fake.kills), len(candidate.ROLES))

    def test_bad_identity_aborts_before_any_stop(self):
        with tempfile.TemporaryDirectory() as root:
            scratch = Path(root) / f'native-qualification-{"a" * 32}'; scratch.mkdir()
            fake = FakeDocker(scratch)
            fake.containers[fake.ids['runtime']]['HostConfig']['PidMode'] = 'host'
            with self.assertRaises(RuntimeError):
                candidate.terminate_fixture(fake.manifest(), scratch, root, fake)
            self.assertEqual(fake.kills, [])

    def test_one_failed_stop_is_uncertain_but_others_are_attempted(self):
        with tempfile.TemporaryDirectory() as root:
            scratch = Path(root) / f'native-qualification-{"a" * 32}'; scratch.mkdir()
            fake = FakeDocker(scratch)
            fake.fail_kill = fake.ids['runtime']
            outcomes = candidate.terminate_fixture(fake.manifest(), scratch, root, fake)
            self.assertFalse(outcomes['runtime'])
            self.assertFalse(candidate.all_observed_stopped(outcomes))
            self.assertTrue(all(outcomes[r] for r in candidate.ROLES if r != 'runtime'))
            self.assertEqual(len(fake.kills), len(candidate.ROLES))

    def test_missing_fixture_role_is_rejected(self):
        with tempfile.TemporaryDirectory() as root:
            scratch = Path(root) / f'native-qualification-{"a" * 32}'; scratch.mkdir()
            fake = FakeDocker(scratch)
            manifest = fake.manifest()
            del manifest['containers']['mongo']
            with self.assertRaises(ValueError):
                candidate.terminate_fixture(manifest, scratch, root, fake)
            self.assertEqual(fake.kills, [])

    def test_restart_policy_and_extra_container_fail_preflight(self):
        for fault in ('restart', 'extra', 'image', 'restarting'):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as root:
                scratch = Path(root) / f'native-qualification-{"a" * 32}'; scratch.mkdir()
                fake = FakeDocker(scratch)
                if fault == 'restart': fake.containers[fake.ids['runtime']]['HostConfig']['RestartPolicy']['Name'] = 'always'
                if fault == 'extra': fake.extra_id = 'e' * 64
                if fault == 'image': fake.containers[fake.ids['runtime']]['Image'] = 'sha256:' + 'd' * 64
                if fault == 'restarting': fake.containers[fake.ids['runtime']]['State']['Restarting'] = True
                with self.assertRaises(RuntimeError):
                    candidate.terminate_fixture(fake.manifest(), scratch, root, fake)
                self.assertEqual(fake.kills, [])

    def test_wait_failure_is_uncertain(self):
        with tempfile.TemporaryDirectory() as root:
            scratch = Path(root) / f'native-qualification-{"a" * 32}'; scratch.mkdir()
            fake = FakeDocker(scratch)
            fake.fail_wait = fake.ids['mongo']
            outcomes = candidate.terminate_fixture(fake.manifest(), scratch, root, fake)
            self.assertFalse(outcomes['mongo'])
            self.assertEqual(len(fake.kills), len(candidate.ROLES))

    def test_wrong_scratch_rejected_before_inspection(self):
        with tempfile.TemporaryDirectory() as root:
            fake = FakeDocker(Path(root))
            with self.assertRaises(ValueError):
                candidate.terminate_fixture(fake.manifest(), '/', root, fake)
            self.assertEqual(fake.kills, [])

    def test_late_restart_is_not_reported_stopped(self):
        with tempfile.TemporaryDirectory() as root:
            scratch = Path(root) / f'native-qualification-{"a" * 32}'; scratch.mkdir()
            fake = FakeDocker(scratch)
            fake.late_restart = True
            outcomes = candidate.terminate_fixture(fake.manifest(), scratch, root, fake)
            self.assertFalse(outcomes['runtime'])

    def test_network_and_host_settings_fail_preflight(self):
        for fault in ('network', 'capadd', 'device', 'unconfined', 'mount'):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as root:
                scratch = Path(root) / f'native-qualification-{"a" * 32}'; scratch.mkdir()
                fake = FakeDocker(scratch)
                info = fake.containers[fake.ids['runtime']]
                if fault == 'network': fake.extra_network_member = True
                if fault == 'capadd': info['HostConfig']['CapAdd'] = ['SYS_ADMIN']
                if fault == 'device': info['HostConfig']['Devices'] = [{'PathOnHost': '/dev/null'}]
                if fault == 'unconfined': info['HostConfig']['SecurityOpt'].append('seccomp=unconfined')
                if fault == 'mount': info['Mounts'][0]['Source'] = root
                with self.assertRaises(RuntimeError):
                    candidate.terminate_fixture(fake.manifest(), scratch, root, fake)
                self.assertEqual(fake.kills, [])

    def test_timeout_or_lost_inspection_is_uncertain(self):
        for fault in ('wait', 'inspection'):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as root:
                scratch = Path(root) / f'native-qualification-{"a" * 32}'; scratch.mkdir()
                fake = FakeDocker(scratch)
                if fault == 'wait': fake.timeout_wait = fake.ids['mongo']
                else: fake.lost_inspection = fake.ids['mongo']
                outcomes = candidate.terminate_fixture(fake.manifest(), scratch, root, fake)
                self.assertFalse(outcomes['mongo'])
                self.assertEqual(len(fake.kills), len(candidate.ROLES))

    def test_new_final_container_invalidates_all_observations(self):
        with tempfile.TemporaryDirectory() as root:
            scratch = Path(root) / f'native-qualification-{"a" * 32}'; scratch.mkdir()
            fake = FakeDocker(scratch)
            fake.final_extra_id = 'e' * 64
            outcomes = candidate.terminate_fixture(fake.manifest(), scratch, root, fake)
            self.assertFalse(any(outcomes.values()))
            self.assertFalse(candidate.all_observed_stopped(outcomes))

    def test_missing_or_nonboolean_observation_cannot_succeed(self):
        self.assertFalse(candidate.all_observed_stopped({}))
        self.assertFalse(candidate.all_observed_stopped({role: 1 for role in candidate.ROLES}))


if __name__ == '__main__':
    unittest.main()
