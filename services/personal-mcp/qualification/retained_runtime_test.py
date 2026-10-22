"""Disposable fake artifact contract tests. No native execution or connections."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import retained_runtime as m


class Retained(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name)/'fixture.zip'
        self.binary = b'\x7fELF\x02\x01' + b'\0'*12 + b'\x3e\x00' + b'PUBLIC FAKE'
        self.digest = hashlib.sha256(self.binary).hexdigest()
        self.receipt = {'source_commit':m.SOURCE,'aiua_sha256':self.digest,'production_selection':False}
        self.report = {'migration_units_passed':True,'state_suite_returncode':0,'broker_suite_returncode':0,'installed_equivalence':False}
        self.run = {'id':m.RUN,'head_sha':m.WORKFLOW,'conclusion':'success','event':'push'}
        self.artifact = {'id':m.ARTIFACT,'workflow_run':{'id':m.RUN},'expired':False}

    def check(self, extra=None, receipt=None):
        with zipfile.ZipFile(self.path,'w') as z:
            z.writestr('aiua',self.binary)
            z.writestr('candidate-receipt.json',receipt or json.dumps(self.receipt))
            z.writestr('combined-results.json',json.dumps(self.report))
            if extra:
                z.writestr(*extra)
        digest = hashlib.sha256(self.path.read_bytes()).hexdigest()
        self.artifact.setdefault('digest','sha256:'+digest)
        with patch.object(m,'ARCHIVE',digest), patch.object(m,'BINARY',self.digest):
            return m.verify(self.path,self.run,self.artifact)

    def test_exact_synthetic_artifact(self):
        binary,receipt=self.check();self.assertEqual(binary,self.binary)
        self.assertFalse(receipt['installed_equivalence']);self.assertFalse(receipt['activation_authorized'])

    def test_source_mismatch(self):
        self.receipt['source_commit']='0'*40
        with self.assertRaises(ValueError):self.check()

    def test_failed_or_wrong_producer(self):
        for field,value in (('conclusion','failure'),('head_sha','0'*40),('id',1),('event','workflow_dispatch')):
            before=self.run[field];self.run[field]=value
            with self.assertRaises(ValueError):self.check()
            self.run[field]=before;self.artifact.pop('digest',None)

    def test_artifact_identity_expiry_and_digest(self):
        for field,value in (('id',1),('expired',True),('digest','sha256:'+'0'*64)):
            before=self.artifact.get(field);self.artifact[field]=value
            with self.assertRaises(ValueError):self.check()
            self.artifact[field]=before;self.artifact.pop('digest',None)

    def test_qualification_failure_and_false_live_claim(self):
        self.report['state_suite_returncode']=1
        with self.assertRaises(ValueError):self.check()
        self.report['state_suite_returncode']=0;self.artifact.pop('digest',None);self.receipt['production_selection']=True
        with self.assertRaises(ValueError):self.check()

    def test_closed_names_and_symlinks(self):
        for name in ('../aiua','nested/aiua','nested\\aiua'):
            self.artifact.pop('digest',None)
            with self.assertRaises(ValueError):self.check((name,b'PUBLIC'))
        info=zipfile.ZipInfo('link');info.external_attr=(0o120777<<16)
        self.artifact.pop('digest',None)
        with self.assertRaises(ValueError):self.check((info,b'aiua'))

    def test_duplicate_json(self):
        with self.assertRaises(ValueError):self.check(receipt='{"source_commit":"a","source_commit":"b"}')

    def test_binary_digest_substitution(self):
        self.receipt['aiua_sha256']='0'*64
        with self.assertRaises(ValueError):self.check()

    def test_actual_binary_bytes_substitution(self):
        self.binary += b'ALTERED'
        with self.assertRaises(ValueError):self.check()


if __name__ == '__main__':unittest.main()
