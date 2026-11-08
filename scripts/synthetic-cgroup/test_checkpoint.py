import os
import tempfile
import unittest
import traceback
from pathlib import Path
from checkpoint import NAME, create, load
N='a'*32
M='b'*64
SECRET='synthetic-token-never-print'

class CheckpointTest(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.path=Path(self.temp.name);self.path.chmod(0o700)
    def tearDown(self): self.temp.cleanup()
    def test_roundtrip_and_exclusive_create(self):
        create(self.path,N,M,8,SECRET)
        self.assertEqual((self.path/NAME).stat().st_mode & 0o777,0o600)
        self.assertEqual(load(self.path,N,M)['revision'],8)
        with self.assertRaises(FileExistsError): create(self.path,N,M,8,SECRET)
    def test_wrong_binding_and_malformed_do_not_echo_token(self):
        create(self.path,N,M,8,SECRET)
        for nonce,mongo in [('c'*32,M),(N,'d'*64)]:
            with self.assertRaises(ValueError) as error: load(self.path,nonce,mongo)
            self.assertNotIn(SECRET,str(error.exception))
        (self.path/NAME).write_text('{"schema":1,"schema":1}')
        with self.assertRaises(ValueError): load(self.path,N,M)
    def test_invalid_utf8_traceback_does_not_echo_input(self):
        file=self.path/NAME
        file.write_bytes(b"SYNTHETIC_SECRET_\xff")
        file.chmod(0o600)
        try:
            load(self.path,N,M)
        except ValueError as error:
            self.assertNotIn("SYNTHETIC_SECRET", "".join(traceback.format_exception(error)))
        else:
            self.fail("invalid UTF-8 was accepted")
    def test_unsafe_permissions_and_hardlink_refused(self):
        create(self.path,N,M,8,SECRET);file=self.path/NAME
        file.chmod(0o644)
        with self.assertRaises(ValueError): load(self.path,N,M)
        file.chmod(0o600);os.link(file,self.path/'other')
        with self.assertRaises(ValueError): load(self.path,N,M)
    def test_symlink_and_unsafe_directory_refused(self):
        (self.path/NAME).symlink_to('/dev/null')
        with self.assertRaises(OSError): load(self.path,N,M)
        (self.path/NAME).unlink();self.path.chmod(0o755)
        with self.assertRaises(ValueError): create(self.path,N,M,8,SECRET)
    def test_invalid_input_creates_no_file(self):
        for revision,token in [(-1,SECRET),(True,SECRET),(8,'a\nb')]:
            with self.assertRaises(ValueError): create(self.path,N,M,revision,token)
            self.assertFalse((self.path/NAME).exists())

if __name__=='__main__': unittest.main()
