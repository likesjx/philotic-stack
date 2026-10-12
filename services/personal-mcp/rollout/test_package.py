import copy
import hashlib
import io
import json
from pathlib import Path
import struct
import tarfile
import tempfile
import unittest

import package


def elf(machine=62, kind=3, broker=False):
    data = bytearray(64)
    data[:7] = b'\x7fELF\x02\x01\x01'
    struct.pack_into('<HH', data, 16, kind, machine)
    if broker:
        data.extend(b'\x00'.join(package.MARKERS) + package.CANDIDATE.encode())
    return bytes(data)


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.binaries = self.root / 'binaries'
        self.binaries.mkdir()
        for name in package.TRIO:
            self.write(name, elf(broker=name == 'aiua'))

    def write(self, name, data, mode=0o755):
        file = self.binaries / name
        file.write_bytes(data)
        file.chmod(mode)

    def build(self, name='output', source=package.CANDIDATE, run=123):
        output = self.root / name
        receipt = package.package(self.binaries, output, source, run)
        return output / 'muninn-hotel-linux-x86_64.tar.gz', receipt

    def forge(self, archive, receipt, mutate):
        with tarfile.open(archive, 'r:gz') as tar:
            contents = [(copy.copy(m), tar.extractfile(m).read()) for m in tar]
        contents = mutate(contents)
        with tarfile.open(archive, 'w:gz', format=tarfile.USTAR_FORMAT) as tar:
            for member, data in contents:
                member.size = len(data)
                tar.addfile(member, io.BytesIO(data) if member.isfile() else None)
        receipt = copy.deepcopy(receipt)
        receipt['archive_sha256'] = hashlib.sha256(archive.read_bytes()).hexdigest()
        if contents[0][0].name == 'manifest.json':
            receipt['manifest_sha256'] = hashlib.sha256(contents[0][1]).hexdigest()
        return receipt

    def test_roundtrip_contains_only_three_binaries(self):
        self.write('model-controller-openrouter', b'DO_NOT_REPLACE')
        archive, receipt = self.build()
        manifest = package.verify(archive, receipt)
        self.assertEqual(set(manifest['components']), set(package.TRIO))
        self.assertTrue(all(v is False for v in manifest['activation'].values()))
        self.assertEqual(manifest['preserve_openrouter_sha256'], package.OPENROUTER)
        with tarfile.open(archive) as tar:
            self.assertNotIn('bin/model-controller-openrouter', tar.getnames())

    def test_deterministic_package(self):
        first, receipt = self.build()
        second, other = self.build('second')
        self.assertEqual(first.read_bytes(), second.read_bytes())
        self.assertEqual(receipt, other)

    def test_unreviewed_source_refused_before_output(self):
        with self.assertRaises(ValueError):
            self.build(source='a' * 40)
        self.assertFalse((self.root / 'output').exists())

    def test_no_workflow_provenance(self):
        for run in [0, -1, True]:
            with self.subTest(run=run), self.assertRaises(ValueError):
                self.build(run=run)

    def test_missing_component(self):
        (self.binaries / 'philote').unlink()
        with self.assertRaises(FileNotFoundError):
            self.build()

    def test_wrong_architecture_or_relocatable(self):
        for machine, kind in [(183, 3), (62, 1)]:
            self.write('philote', elf(machine, kind))
            with self.subTest(machine=machine, kind=kind), self.assertRaises(ValueError):
                self.build()

    def test_broker_or_build_marker_missing(self):
        for data in [elf(), elf(broker=True).replace(package.CANDIDATE.encode(), b'f' * 40)]:
            self.write('aiua', data)
            with self.assertRaises(ValueError):
                self.build()

    def test_symlink_or_setuid_component(self):
        binary = self.binaries / 'philote'
        binary.unlink()
        binary.symlink_to(self.binaries / 'aiua')
        with self.assertRaises(OSError):
            self.build()
        binary.unlink()
        self.write('philote', elf(), 0o4755)
        with self.assertRaises(ValueError):
            self.build()

    def test_non_executable(self):
        self.write('philote', elf(), 0o644)
        with self.assertRaises(ValueError):
            self.build()

    def test_existing_output_not_overwritten(self):
        self.build()
        with self.assertRaises(FileExistsError):
            self.build()

    def test_archive_byte_tampering(self):
        archive, receipt = self.build()
        archive.write_bytes(archive.read_bytes() + b'tamper')
        with self.assertRaises(ValueError):
            package.verify(archive, receipt)

    def test_symlink_archive(self):
        archive, receipt = self.build()
        alias = self.root / 'alias'
        alias.symlink_to(archive)
        with self.assertRaises(OSError):
            package.verify(alias, receipt)

    def test_parent_alias_denies_inputs_outputs_and_verification(self):
        alias = self.root / 'parent-alias'
        alias.symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(ValueError):
            package.package(alias / 'binaries', self.root / 'denied-input', package.CANDIDATE, 123)
        with self.assertRaises(ValueError):
            package.package(self.binaries, alias / 'denied-output', package.CANDIDATE, 123)
        self.assertFalse((self.root / 'denied-input').exists())
        self.assertFalse((self.root / 'denied-output').exists())
        archive, receipt = self.build()
        with self.assertRaises(ValueError):
            package.verify(alias / archive.relative_to(self.root), receipt)

    def test_extra_archive_member_even_with_rehashed_receipt(self):
        archive, receipt = self.build()
        def extra(contents):
            member = copy.copy(contents[-1][0])
            member.name = 'bin/model-controller-openrouter'
            return contents + [(member, b'forbidden')]
        receipt = self.forge(archive, receipt, extra)
        with self.assertRaises(ValueError):
            package.verify(archive, receipt)

    def test_traversal_or_link_even_with_rehashed_receipt(self):
        for link in [False, True]:
            archive, receipt = self.build('link' if link else 'traversal')
            def mutate(contents):
                member = contents[-1][0]
                if link:
                    member.type, member.linkname = tarfile.SYMTYPE, '/etc/passwd'
                else:
                    member.name = '../heal-dispatcher'
                return contents
            receipt = self.forge(archive, receipt, mutate)
            with self.subTest(link=link), self.assertRaises(ValueError):
                package.verify(archive, receipt)

    def test_activation_and_unknown_fields_rejected(self):
        archive, receipt = self.build()
        with tarfile.open(archive) as tar:
            manifest = json.load(tar.extractfile('manifest.json'))
        for edit in [lambda m: m['activation'].update(broker=True),
                     lambda m: m['activation'].update(broker=0),
                     lambda m: m.update(command='arbitrary'),
                     lambda m: m['components']['aiua'].update(path='/opt/other'),
                     lambda m: m['components']['aiua'].update(expected_current_sha256='f' * 64),
                     lambda m: m['acceptance'].update(mixed_version='passed'),
                     lambda m: m.update(preserve_openrouter_sha256='f' * 64)]:
            changed = copy.deepcopy(manifest)
            edit(changed)
            with self.assertRaises(ValueError):
                package.validate_manifest(changed)

    def test_receipt_run_binding(self):
        archive, receipt = self.build()
        receipt['workflow_run_id'] += 1
        with self.assertRaises(ValueError):
            package.verify(archive, receipt)

    def test_component_corruption_with_rehashed_archive(self):
        archive, receipt = self.build()
        receipt = self.forge(archive, receipt, lambda c: c[:-1] + [(c[-1][0], c[-1][1] + b'changed')])
        with self.assertRaises(ValueError):
            package.verify(archive, receipt)


if __name__ == '__main__':
    unittest.main()
