import copy
import gzip
import importlib.util
import io
import json
from pathlib import Path
import struct
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('package_release', Path(__file__).with_name('package_release.py'))
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
SOURCE = 'a' * 40


def public_elf():
    raw = bytearray(64); raw[:6] = b'\x7fELF\x02\x01'
    struct.pack_into('<HH', raw, 16, 3, 62)
    return bytes(raw) + SOURCE.encode()


def manifest():
    raw = public_elf()
    return {'schema': 1, 'bundle': 'beacon-controller-diagnostics-v1', 'source_sha': SOURCE,
            'installed_source_baseline': m.BASE, 'target': 'x86_64-unknown-linux-gnu',
            'activation_authorized': False,
            'components': {name: {'sha256': m.digest(raw), 'bytes': len(raw), 'roles': roles,
                'needed_libraries': ['libc.so.6'], 'glibc_minimum': '2.39'} for name, roles in m.COMPONENTS.items()}}


def receipt():
    name = 'model-controller-openrouter'
    return {'schema': 1, 'manifest_sha256': m.digest(m.canonical(manifest())),
            'selected': {name: {'installed_sha256': 'b' * 64, 'running_sha256': 'b' * 64,
                'guest_binding_manifest_sha256': 'c' * 64, 'guest_count': 1,
                'roles': ['model.openrouter'], 'all_consumers_reviewed': True}},
            'preserved': dict.fromkeys(m.PRESERVED, 'd' * 64),
            'existing_gates': dict.fromkeys(m.contract()['required_existing_gates'], 'e' * 64),
            'journal': {'policy_manifest_sha256': 'f' * 64, 'storage_mode': 'persistent',
                'retention_seconds': 0, 'maximum_bytes': 1024, 'access_audience_reviewed': True,
                'export_destinations_reviewed': True}}


class ControllerPackage(unittest.TestCase):
    def test_selection_produces_exact_rollback_without_authorizing_install(self):
        value = m.plan(manifest(), receipt())
        self.assertFalse(value['activation_authorized'])
        self.assertEqual(set(value['selected']), {'model-controller-openrouter'})
        self.assertEqual(value['selected']['model-controller-openrouter']['rollback_sha256'], 'b' * 64)
        self.assertFalse(value['contract']['memory_activation'])

    def test_cross_source_components_guest_binding_and_unsafe_receipts_refuse(self):
        for mutate in (lambda r: r.update(manifest_sha256='a' * 64),
                       lambda r: r['selected'].update(aiua={}),
                       lambda r: r['selected']['model-controller-openrouter'].update(running_sha256='c' * 64),
                       lambda r: r['selected']['model-controller-openrouter'].update(roles=['model']),
                       lambda r: r['selected']['model-controller-openrouter'].update(guest_count=True),
                       lambda r: r['selected']['model-controller-openrouter'].update(private_target='PRIVATE_PATH'),
                       lambda r: r.update(selected={}),
                       lambda r: r['existing_gates'].update(trusted_install_ancestry=True),
                       lambda r: r['journal'].update(access_audience_reviewed=False),
                       lambda r: r['journal'].update(export_destinations_reviewed=False),
                       lambda r: r['journal'].update(retention_seconds='unknown'),
                       lambda r: r['preserved'].pop('aiua')):
            value = copy.deepcopy(receipt()); mutate(value)
            with self.subTest(value=value), self.assertRaises((ValueError, TypeError)): m.plan(manifest(), value)

    def test_manifest_architecture_authority_extra_and_boolean_bounds_refuse(self):
        for mutate in (lambda v: v.update(schema=True), lambda v: v.update(target='aarch64'),
                       lambda v: v.update(activation_authorized=True),
                       lambda v: v['components']['model-router'].update(bytes=True),
                       lambda v: v['components']['model-router'].update(needed_libraries=['private.so']),
                       lambda v: v['components']['model-router'].update(glibc_minimum='2.40'),
                       lambda v: v['components']['model-router'].update(extra='PRIVATE')):
            value = copy.deepcopy(manifest()); mutate(value)
            with self.assertRaises(ValueError): m.validate_manifest(value)

    def test_elf_requires_x86_64_little_endian_executable(self):
        m.elf(public_elf())
        for raw in (b'x' * 64, public_elf().replace(b'\x02\x01', b'\x01\x01', 1),
                    public_elf()[:18] + b'\xb7\x00' + public_elf()[20:]):
            with self.assertRaises(ValueError): m.elf(raw)

    def test_dependency_and_glibc_projection_fail_closed(self):
        result = subprocess.CompletedProcess([], 0, b'(NEEDED) Shared library: [libc.so.6]\nName: GLIBC_2.39')
        with patch.object(m.subprocess, 'run', return_value=result):
            self.assertEqual(m.dependencies(Path('PUBLIC_ELF'))['glibc_minimum'], '2.39')
        for raw in (b'(NEEDED) Shared library: [private.so]',
                    b'(NEEDED) Shared library: [/opt/private/libfoo.so]',
                    b'(NEEDED) malformed', b'Name: GLIBC_2.40'):
            with patch.object(m.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, raw)), self.assertRaises(ValueError):
                m.dependencies(Path('PUBLIC_ELF'))

    def test_real_package_archive_pins_and_source_match(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); binaries = root / 'binaries'; binaries.mkdir()
            for name in m.COMPONENTS: (binaries / name).write_bytes(public_elf())
            with patch.object(m.subprocess, 'check_output', side_effect=[SOURCE, '']), \
                 patch.object(m.subprocess, 'run'), patch.object(m, 'dependencies', return_value={'needed_libraries': ['libc.so.6'], 'glibc_minimum': '2.39'}):
                m.assemble(binaries, root / 'dist', SOURCE)
            path = root / 'dist' / 'beacon-controller-diagnostics-x86_64.tar.gz'
            recorded = json.loads((root / 'dist' / 'package-receipt.json').read_bytes())
            self.assertEqual(m.verify(path, recorded['archive_sha256'], recorded['manifest_sha256'], SOURCE), manifest())
            for archive_sha, manifest_sha, source in [('b' * 64, recorded['manifest_sha256'], SOURCE),
                    (recorded['archive_sha256'], 'b' * 64, SOURCE),
                    (recorded['archive_sha256'], recorded['manifest_sha256'], 'b' * 40)]:
                with self.assertRaises(ValueError): m.verify(path, archive_sha, manifest_sha, source)
            # Even an explicitly supplied archive digest cannot bless traversal.
            output = io.BytesIO()
            with tarfile.open(fileobj=output, mode='w:gz') as tar:
                item = tarfile.TarInfo('../PRIVATE'); item.size = 1; tar.addfile(item, io.BytesIO(b'x'))
            path.write_bytes(output.getvalue())
            with self.assertRaises(ValueError): m.verify(path, m.digest(path.read_bytes()), recorded['manifest_sha256'], SOURCE)

    def test_dirty_or_mismatched_source_refuses_before_build(self):
        for replies in ([SOURCE, ' M PUBLIC_SOURCE.py\n'], ['b' * 40]):
            with patch.object(m.subprocess, 'check_output', side_effect=replies), \
                 patch.object(m.subprocess, 'run') as ancestor, self.assertRaises(ValueError):
                m.source_guard(SOURCE)
            ancestor.assert_not_called()


if __name__ == '__main__': unittest.main()
