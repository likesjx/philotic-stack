import base64,importlib.util,os,pathlib,tempfile,unittest
s=importlib.util.spec_from_file_location('h',pathlib.Path(__file__).with_name('private_key_handoff.py'));h=importlib.util.module_from_spec(s);s.loader.exec_module(h)
class Test(unittest.TestCase):
 def pipe(self,value):
  r,w=os.pipe();os.write(w,value);os.close(w);self.addCleanup(os.close,r);return r
 def test_public_fixture_pipe(self):
  encoded=base64.b64encode(bytes([7])*32)
  self.assertEqual(h.key_line(self.pipe(encoded+b'\n')),b'PHILOTIC_VAULT_MASTER_KEY='+encoded+b'\n')
 def test_invalid_or_excess(self):
  for data in [b'bad',base64.b64encode(bytes(31)),base64.b64encode(bytes(32))+b'\nextra']:
   with self.assertRaises(ValueError):h.key_line(self.pipe(data))
 def test_regular_file_refused(self):
  with tempfile.TemporaryFile() as f:
   with self.assertRaises(ValueError):h.key_line(f.fileno())
if __name__=='__main__':unittest.main()
