import hashlib
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

import native_fixture_preflight_candidate as preflight
from test_native_container_boundary_candidate import FakeDocker


class PreflightDocker(FakeDocker):
    def __init__(self, scratch):
        super().__init__(scratch)
        self.scratch = scratch
        self.volumes = {}
        self.image_volumes = {image: {} for image in self.images.values()}
        self.extra_network = False
        self.extra_volume = None
        for role, ident in self.ids.items():
            info = self.containers[ident]
            info['State'] = {'Running': False, 'Pid': 0, 'Restarting': False, 'Dead': False,
                             'Status': 'created', 'StartedAt': '0001-01-01T00:00:00Z'}
            info['RestartCount'] = 0
            info['HostConfig']['Tmpfs'] = {}
            info['HostConfig']['PublishAllPorts'] = False
            info['NetworkSettings'] = {'Networks': ({f'native-qualification-{self.nonce}-network': {'NetworkID': self.network}}
                                                   if role in ('anchor', 'proxy') else {})}
            info['Config'].update({'Env': ['CI=true'], 'Entrypoint': ['/bin/true'], 'Cmd': []})
            info['Mounts'] = [{'Type': 'bind', 'Source': str(scratch), 'Destination': '/fixture',
                               'RW': False, 'Propagation': 'rprivate'}]

    def __call__(self, args):
        if args[:2] == ['network', 'inspect']:
            containers = {'e' * 64: {}} if self.extra_network else {}
            info = {'Id': self.network, 'Name': f'native-qualification-{self.nonce}-network',
                    'Internal': True, 'Driver': getattr(self, 'network_driver', 'bridge'),
                    'Scope': getattr(self, 'network_scope', 'local'),
                    'Labels': {'com.philotic.fixture': self.nonce}, 'Containers': containers}
            return SimpleNamespace(returncode=0, stdout=json.dumps([info]))
        if args[:2] == ['image', 'inspect']:
            return SimpleNamespace(returncode=0, stdout=json.dumps([{'Id': args[2], 'Config': {'Volumes': self.image_volumes[args[2]]}}]))
        if args[:2] == ['volume', 'inspect']:
            return SimpleNamespace(returncode=0, stdout=json.dumps([self.volumes[args[2]]]))
        if args[:2] == ['volume', 'ls']:
            return SimpleNamespace(returncode=0, stdout='\n'.join([*self.volumes, *([self.extra_volume] if self.extra_volume else [])]))
        return super().__call__(args)

    def inputs(self):
        artifact = self.scratch / 'fixture.bin'
        artifact.write_bytes(b'synthetic')
        env_hash = hashlib.sha256(b'["CI=true"]').hexdigest()
        role = {'declared_volumes': [], 'mounts': [{'type': 'bind', 'source': str(self.scratch.resolve()),
                 'destination': '/fixture', 'rw': False, 'propagation': 'rprivate'}],
                'tmpfs': {}, 'entrypoint': ['/bin/true'], 'command': [], 'env_sha256': env_hash}
        contract = {'images': dict(self.images), 'roles': {r: dict(role) for r in self.ids},
                    'artifacts': {'fixture.bin': hashlib.sha256(b'synthetic').hexdigest()}}
        manifest = {'nonce': self.nonce, 'containers': dict(self.ids), 'network_id': self.network}
        return manifest, contract


class PreflightTests(unittest.TestCase):
    def setup_fixture(self, root):
        scratch = Path(root) / f'native-qualification-{"a" * 32}'
        scratch.mkdir()
        fake = PreflightDocker(scratch)
        return scratch, fake, *fake.inputs()

    def test_exact_created_fixture_is_read_only(self):
        with tempfile.TemporaryDirectory() as root:
            scratch, fake, manifest, contract = self.setup_fixture(root)
            result = preflight.preflight_fixture(manifest, contract, scratch, root, fake)
            self.assertEqual(result, {'containers': 6, 'volumes': 0, 'state': 'created-not-started'})
            self.assertEqual(fake.kills, [])

    def test_unexpected_inputs_fail_before_execution(self):
        for fault in ('image', 'network', 'network-driver', 'network-scope', 'extra-attachment', 'wrong-network-id', 'missing-network-settings', 'shared-extra-attachment', 'publish-all', 'missing-publish-all', 'started', 'mount', 'destination', 'propagation', 'duplicate', 'artifact', 'artifact-symlink', 'environment', 'declared-volume', 'unknown-volume'):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as root:
                scratch, fake, manifest, contract = self.setup_fixture(root)
                info = fake.containers[fake.ids['runtime']]
                if fault == 'image': contract['images']['runtime'] = 'sha256:' + 'd' * 64
                if fault == 'network': fake.extra_network = True
                if fault == 'network-driver': fake.network_driver = 'overlay'
                if fault == 'network-scope': fake.network_scope = 'swarm'
                if fault == 'extra-attachment': fake.containers[fake.ids['anchor']]['NetworkSettings']['Networks']['unexpected'] = {'NetworkID': 'e' * 64}
                if fault == 'wrong-network-id': fake.containers[fake.ids['proxy']]['NetworkSettings']['Networks'][f'native-qualification-{fake.nonce}-network']['NetworkID'] = 'e' * 64
                if fault == 'missing-network-settings': del fake.containers[fake.ids['anchor']]['NetworkSettings']
                if fault == 'shared-extra-attachment': info['NetworkSettings']['Networks']['unexpected'] = {'NetworkID': 'e' * 64}
                if fault == 'publish-all': fake.containers[fake.ids['anchor']]['HostConfig']['PublishAllPorts'] = True
                if fault == 'missing-publish-all': del fake.containers[fake.ids['proxy']]['HostConfig']['PublishAllPorts']
                if fault == 'started': info['State'].update(Running=True, Pid=123, Status='running')
                if fault == 'mount': info['Mounts'][0]['RW'] = True
                if fault == 'destination': info['Mounts'][0]['Destination'] = '/unexpected'
                if fault == 'propagation': info['Mounts'][0]['Propagation'] = 'shared'
                if fault == 'duplicate': info['Mounts'].append(dict(info['Mounts'][0]))
                if fault == 'artifact': (scratch / 'fixture.bin').write_bytes(b'changed')
                if fault == 'artifact-symlink':
                    (scratch / 'fixture.bin').unlink()
                    (scratch / 'fixture.bin').symlink_to('/dev/null')
                if fault == 'environment': info['Config']['Env'].append('UNEXPECTED=value')
                if fault == 'declared-volume': fake.image_volumes[fake.images['runtime']] = {'/unknown': {}}
                if fault == 'unknown-volume': fake.extra_volume = 'unowned-volume'
                with self.assertRaises((RuntimeError, ValueError, KeyError)):
                    preflight.preflight_fixture(manifest, contract, scratch, root, fake)
                self.assertEqual(fake.kills, [])

    def test_labelled_volume_is_checked_but_not_yet_qualified_for_teardown(self):
        with tempfile.TemporaryDirectory() as root:
            scratch, fake, manifest, contract = self.setup_fixture(root)
            name = f'native-qualification-{fake.nonce}-mongo-data'
            info = fake.containers[fake.ids['mongo']]
            info['Mounts'].append({'Type': 'volume', 'Name': name, 'Destination': '/data/db', 'RW': True, 'Propagation': ''})
            contract['roles']['mongo']['mounts'] = [*contract['roles']['mongo']['mounts'],
                {'type': 'volume', 'source': name, 'destination': '/data/db', 'rw': True, 'propagation': ''}]
            contract['roles']['mongo']['declared_volumes'] = ['/data/db']
            fake.image_volumes[fake.images['mongo']] = {'/data/db': {}}
            fake.volumes[name] = {'Name': name, 'Driver': 'local', 'Scope': 'local',
                                  'Labels': {'com.philotic.fixture': fake.nonce}, 'Options': {}}
            with self.assertRaisesRegex(RuntimeError, 'volume teardown not qualified'):
                preflight.preflight_fixture(manifest, contract, scratch, root, fake)
            fake.volumes[name]['Labels'] = {}
            with self.assertRaises(RuntimeError):
                preflight.preflight_fixture(manifest, contract, scratch, root, fake)


if __name__ == '__main__':
    unittest.main()
