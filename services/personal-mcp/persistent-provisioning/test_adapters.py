import base64,hashlib,json,os,subprocess,tempfile,unittest
from pathlib import Path
from unittest.mock import patch
import adapters
from provision import ROLE,GUEST,INTROSPECTION,Refused
KEY=base64.b64encode(bytes([7])*32)
class Tests(unittest.TestCase):
    def test_broker_wire_contains_exact_scope_acl_and_kind(self):
        def receive(req,key,digest):
            self.assertEqual(set(req),{'operation','kind','scope','secret','allowed_roles','allowed_guests'})
            self.assertEqual(req['operation'],'create');self.assertEqual(req['scope'],'fixture_vault')
            self.assertEqual(req['allowed_roles'],[ROLE]);self.assertEqual(req['allowed_guests'],[GUEST]);self.assertEqual(key,KEY)
            return {'reference':'secret://hotel/default/percival-muninn-observe/'+'a'*32}
        b=adapters.Broker(KEY,'a'*64)
        with patch.object(adapters,'run_helper',receive):
            b.store_exact_acl(kind='percival-muninn-observe',scope='fixture_vault',secret='mk_'+'s'*40,allowed_roles=[ROLE],allowed_guests=[GUEST])
        b.close();self.assertEqual(b.key,b'')
    def test_invalid_scope_never_launches_helper(self):
        b=adapters.Broker(KEY,'a'*64)
        with patch.object(adapters,'run_helper',side_effect=AssertionError('launch')):
            for scope in ['', '../default','a b','Uppercase','a'*65]:
                with self.assertRaises(Refused):b.store_exact_acl(kind='percival-muninn-observe',scope=scope,secret='s'*40,allowed_roles=[ROLE],allowed_guests=[GUEST])
    def test_real_subprocess_private_pipe_wire_and_exit(self):
        with tempfile.TemporaryDirectory() as td:
            helper=Path(td)/'helper';helper.write_bytes(b'public artifact')
            handoff=Path(td)/'handoff.py';handoff.write_text("import sys,os,json\nfd=int(sys.argv[-1]);key=os.read(fd,100);os.close(fd)\nr=json.load(sys.stdin)\nassert len(key)==45 and r['operation']=='create'\nprint(json.dumps({'reference':'secret://hotel/default/'+r['kind']+'/'+'a'*32}))\n")
            with patch.object(adapters,'HELPER',helper),patch.object(adapters,'HANDOFF',handoff),patch.object(adapters,'trusted'):
                out=adapters.run_helper({'operation':'create','kind':'percival-muninn-observe'},KEY,hashlib.sha256(helper.read_bytes()).hexdigest())
            self.assertEqual(set(out),{'reference'})
    def test_timeout_waits_for_helper_and_does_not_retry(self):
        with tempfile.TemporaryDirectory() as td:
            helper=Path(td)/'helper';helper.write_bytes(b'public artifact')
            handoff=Path(td)/'handoff.py';handoff.write_text('import time\ntime.sleep(60)\n')
            # Observe process creation directly. A cold interpreter may not run
            # Python code/write a marker before the deliberately short timeout.
            real_spawn=subprocess.Popen;children=[]
            def spawn(*args,**kwargs):
                child=real_spawn(*args,**kwargs);children.append(child);return child
            with patch.object(adapters,'HELPER',helper),patch.object(adapters,'HANDOFF',handoff),patch.object(adapters,'trusted'),patch.object(adapters.subprocess,'Popen',side_effect=spawn):
                with self.assertRaises(subprocess.TimeoutExpired):adapters.run_helper({},KEY,hashlib.sha256(helper.read_bytes()).hexdigest(),timeout=.5)
            self.assertEqual(len(children),1)
            self.assertIsNotNone(children[0].returncode)
            with self.assertRaises(ProcessLookupError):os.kill(children[0].pid,0)
    def test_wrong_helper_hash_stops_before_process(self):
        with tempfile.TemporaryDirectory() as td:
            helper=Path(td)/'helper';helper.write_bytes(b'public artifact')
            with patch.object(adapters,'HELPER',helper),patch.object(adapters,'trusted'),patch.object(adapters.subprocess,'Popen',side_effect=AssertionError('process')):
                with self.assertRaises(Refused):adapters.run_helper({},KEY,'a'*64)
if __name__=='__main__':unittest.main()
