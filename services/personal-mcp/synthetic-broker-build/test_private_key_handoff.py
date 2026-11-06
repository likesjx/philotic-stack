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

class DelayedDelivery(unittest.TestCase):
 def test_key_withheld_until_exact_ready(self):
  import socket,threading,select
  parent,peer=socket.socketpair();r,w=os.pipe();encoded=base64.b64encode(bytes([7])*32);os.write(w,encoded+b'\n');os.close(w)
  errors=[]
  def deliver():
   try:h.deliver_after_ready(parent,r)
   except Exception as e:errors.append(type(e).__name__)
  t=threading.Thread(target=deliver);t.start()
  try:
   self.assertTrue(select.select([r],[],[],0)[0])
   self.assertFalse(select.select([peer],[],[],0.05)[0])
   peer.sendall(b'REA');self.assertFalse(select.select([peer],[],[],0.05)[0]);self.assertTrue(select.select([r],[],[],0)[0])
   peer.sendall(b'DY\n');data=b''
   while True:
    part=peer.recv(130)
    if not part:break
    data+=part
   t.join(2);self.assertFalse(t.is_alive());self.assertEqual(errors,[])
   self.assertEqual(data,b'PHILOTIC_VAULT_MASTER_KEY='+encoded+b'\n')
  finally:parent.close();peer.close();os.close(r)
 def test_invalid_handshake_does_not_consume_key(self):
  import socket
  parent,peer=socket.socketpair();r,w=os.pipe();os.write(w,b'public-unread');os.close(w)
  try:
   peer.sendall(b'BAD!!\n')
   with self.assertRaises(ValueError):h.deliver_after_ready(parent,r)
   self.assertEqual(os.read(r,100),b'public-unread')
  finally:parent.close();peer.close();os.close(r)

if __name__=='__main__':unittest.main()
