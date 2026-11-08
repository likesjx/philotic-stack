import os
from pathlib import Path
import tempfile
import unittest
from runtime_cgroup import capture

IDENT='a'*64
class CgroupTest(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();root=Path(self.temp.name)
        self.proc=root/'proc';self.cg=root/'cg';(self.proc/'42').mkdir(parents=True)
        self.group=self.cg/'system.slice'/('docker-'+IDENT+'.scope');self.group.mkdir(parents=True)
        (self.proc/'42'/'cgroup').write_text('0::/system.slice/docker-'+IDENT+'.scope\n')
        (self.group/'cgroup.events').write_text('populated 1\nfrozen 0\n')
    def tearDown(self): self.temp.cleanup()
    def capture(self): return capture(IDENT,42,str(self.proc),str(self.cg))
    def test_populated_then_empty(self):
        g=self.capture()
        try:
            (self.group/'cgroup.events').write_text('populated 0\nfrozen 0\n')
            self.assertEqual(g.populated(),0)
        finally: g.close()
    def test_wrong_identity_and_traversal_refused(self):
        for line in ['0::/system.slice/docker-'+'b'*64+'.scope\n','0::/../docker-'+IDENT+'.scope\n','1:name:/x\n']:
            (self.proc/'42'/'cgroup').write_text(line)
            with self.assertRaises(RuntimeError): self.capture()
    def test_ambiguous_or_missing_events_refused(self):
        for text in ['populated 0\npopulated 1\n','frozen 0\n']:
            (self.group/'cgroup.events').write_text(text)
            with self.assertRaises(RuntimeError): self.capture()
        (self.group/'cgroup.events').unlink()
        with self.assertRaises(RuntimeError): self.capture()
    def test_symlink_in_path_refused(self):
        events=self.group/'cgroup.events';events.unlink();events.symlink_to('/dev/null')
        with self.assertRaises(RuntimeError): self.capture()

if __name__=='__main__': unittest.main()
