import copy
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import package
from test_package import elf
import verify_artifact as v


class ArtifactBytesTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        binaries = self.root / 'bin'
        binaries.mkdir()
        for name in package.TRIO:
            p = binaries / name
            p.write_bytes(elf(broker=name == 'aiua'))
            p.chmod(0o755)
        self.receipt = package.package(binaries, self.root / 'built', package.CANDIDATE, v.RUN)
        self.values = {name: (self.root / 'built' / name).read_bytes() for name in v.MEMBERS}
        for name, value in [('ARCHIVE_SHA', self.receipt['archive_sha256']), ('MANIFEST_SHA', self.receipt['manifest_sha256'])]:
            guard = patch.object(v, name, value)
            guard.start()
            self.addCleanup(guard.stop)

    def zipped(self, values=None):
        raw = io.BytesIO()
        with zipfile.ZipFile(raw, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
            for name, data in (values or self.values).items():
                archive.writestr(name, data)
        return raw.getvalue()

    def check(self, raw):
        return v.verify_zip(raw, self.root / 'verified', expected_zip_sha=hashlib.sha256(raw).hexdigest())

    def test_real_package_bytes_roundtrip_and_no_deploy_claim(self):
        result = self.check(self.zipped())
        self.assertTrue(result['artifact_bytes_verified'])
        self.assertFalse(result['deployment_authorized'])
        self.assertFalse(result['systemd_acceptance'])
        self.assertEqual(result['components'], self.receipt['components'])

    def test_outer_zip_digest_denies_before_writes(self):
        with self.assertRaises(ValueError):
            v.verify_zip(self.zipped(), self.root / 'verified')
        self.assertFalse((self.root / 'verified').exists())

    def test_extra_traversal_or_oversized_member_denies_before_writes(self):
        for values in [{**self.values, '../outside': b'forbidden'},
                       {**self.values, 'receipt.json': b'x' * 17000}]:
            with self.assertRaises(ValueError): self.check(self.zipped(values))
            self.assertFalse((self.root / 'verified').exists())

    def test_symlink_member_denies(self):
        raw = io.BytesIO()
        with zipfile.ZipFile(raw, 'w') as archive:
            for name, data in self.values.items():
                info = zipfile.ZipInfo(name)
                info.create_system = 3
                info.external_attr = 0o120777 << 16
                archive.writestr(info, data)
        with self.assertRaises(ValueError): self.check(raw.getvalue())

    def test_inner_receipt_or_archive_tampering_denies(self):
        values = copy.deepcopy(self.values)
        receipt = json.loads(values['receipt.json'])
        receipt['workflow_run_id'] += 1
        values['receipt.json'] = package.canonical(receipt)
        with self.assertRaises(ValueError): self.check(self.zipped(values))
        values = {**self.values, 'muninn-hotel-linux-x86_64.tar.gz': self.values['muninn-hotel-linux-x86_64.tar.gz'] + b'tamper'}
        with self.assertRaises(ValueError): self.check(self.zipped(values))

    def test_aliased_output_denies(self):
        alias = self.root / 'alias'
        alias.symlink_to(self.root, target_is_directory=True)
        raw = self.zipped()
        with self.assertRaises(ValueError):
            v.verify_zip(raw, alias / 'verified', expected_zip_sha=hashlib.sha256(raw).hexdigest())

    def test_provenance_cross_head_failed_expired_or_wrong_digest_denies(self):
        run = {'id': v.RUN, 'repository': {'full_name': 'likesjx/philotic-stack'},
               'head_repository': {'full_name': 'likesjx/philotic-stack'}, 'head_sha': v.HEAD,
               'path': '.github/workflows/muninn-bounded-package.yml', 'event': 'pull_request',
               'status': 'completed', 'conclusion': 'success'}
        artifact = {'id': v.ARTIFACT, 'name': 'muninn-bounded-hotel-linux-x86_64', 'expired': False,
                    'digest': 'sha256:' + v.ZIP_SHA, 'workflow_run': {'id': v.RUN, 'head_sha': v.HEAD}}
        v.provenance(run, artifact)
        for target, key, value in [('run','head_sha','f'*40), ('run','conclusion','failure'),
                                   ('artifact','expired',True), ('artifact','digest','sha256:'+'f'*64)]:
            r, a = copy.deepcopy(run), copy.deepcopy(artifact)
            (r if target == 'run' else a)[key] = value
            with self.assertRaises(ValueError): v.provenance(r, a)


if __name__ == '__main__': unittest.main()
