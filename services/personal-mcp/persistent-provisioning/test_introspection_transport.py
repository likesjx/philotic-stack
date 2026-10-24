import base64,hashlib,json,os,tempfile,unittest
from pathlib import Path
from unittest.mock import patch
import adapters
class Tests(unittest.TestCase):
 def test_real_child_allocation_ack_and_lost_reply(self):
  for fail in(False,True):
   with tempfile.TemporaryDirectory()as td:
    root=Path(td);helper=root/'helper';helper.write_text('public fixture');handoff=root/'handoff.py';marker=root/'stored'
    handoff.write_text("import os,sys,json\nkey=os.read(int(sys.argv[-1]),100)\nrequest=json.loads(sys.stdin.readline())\nref='secret://hotel/default/percival-issuer-introspection/'+'b'*32\nprint(json.dumps({'allocated_reference':ref}),flush=True)\nack=json.loads(sys.stdin.readline());assert ack=={'commit':ref}\nfrom pathlib import Path\nassert Path("+repr(str(root/'allocated'))+").read_text()==ref\nPath("+repr(str(marker))+").write_text('stored')\n"+("raise SystemExit(1)\n"if fail else"print(json.dumps({'reference':ref}),flush=True)\n"))
    seen=[]
    def allocation(ref):(root/'allocated').write_text(ref);seen.append(ref)
    with patch.object(adapters,'HELPER',helper),patch.object(adapters,'HANDOFF',handoff),patch.object(adapters,'trusted'):
     if fail:
      with self.assertRaises(Exception):adapters.run_helper({'operation':'create_introspection'},base64.b64encode(bytes(32)),hashlib.sha256(helper.read_bytes()).hexdigest(),allocation=allocation)
     else:self.assertEqual(adapters.run_helper({'operation':'create_introspection'},base64.b64encode(bytes(32)),hashlib.sha256(helper.read_bytes()).hexdigest(),allocation=allocation),{'reference':seen[0]})
    self.assertEqual(len(seen),1);self.assertTrue(marker.exists())
if __name__=='__main__':unittest.main()
