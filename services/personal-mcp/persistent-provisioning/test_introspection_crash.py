"""Actual disposable process deaths; no tty, live helper, network or private keys."""
import json,os,signal,subprocess,sys,tempfile,unittest
from pathlib import Path
from provision import Journal
import introspection as i
from test_introspection import Broker,Owner
HERE=Path(__file__).resolve().parent
CHILD='''import sys,os,signal,json
from pathlib import Path
sys.path.insert(0,sys.argv[1]);import introspection as i
from provision import Journal
root=Path(sys.argv[2]);stage=sys.argv[3];m=json.loads((root/'m').read_text());j=Journal(root/'j')
class B:
 def close(self):pass
 def store_exact_acl(self,**kw):
  if stage=='before_allocate':os.kill(os.getpid(),signal.SIGKILL)
  ref=i.PREFIX+'c'*32;kw['allocation'](ref)
  if stage=='after_allocate':os.kill(os.getpid(),signal.SIGKILL)
  (root/'records').write_text(json.dumps({ref:{'verifier':i.verifier(kw['secret'])}}))
  if stage=='after_write':os.kill(os.getpid(),signal.SIGKILL)
  return {'reference':ref}
class O:
 def guard(self,*a,**k):pass
 def capture(self):return {'policy':'old','verifier':'old'}
 def action(self,step,data):
  if step=='write_policy':(root/'policy').write_text(data['introspection_secret_ref']);os.kill(os.getpid(),signal.SIGKILL)
i.execute(m,j,lambda d:'CREATE_INTROSPECTION '+d,lambda:B())
if stage=='adopt':i.transition(m,j,O())
'''
class Tests(unittest.TestCase):
 def test_sigkill_allocation_store_and_adoption_prefixes_reconcile_without_retry(self):
  for stage in('before_allocate','after_allocate','after_write','adopt'):
   with self.subTest(stage=stage),tempfile.TemporaryDirectory()as td:
    root=Path(td);m={'version':2,'scope':i.INTROSPECTION,'role':i.ROLE,'guest':i.GUEST,'helper_sha256':'a'*64};(root/'m').write_text(json.dumps(m));p=subprocess.run([sys.executable,'-I','-B','-c',CHILD,str(HERE),td,stage],capture_output=True,text=True,timeout=5);self.assertEqual(p.returncode,-signal.SIGKILL,p.stderr)
    j=Journal(root/'j')
    try:
     if stage=='adopt':
      self.assertEqual(j.data['phase'],'introspection_adoption_pending');self.assertEqual(j.data['steps']['adopt_write_policy'],'pending');o=Owner();o.policy=(root/'policy').read_text();out=i.transition(m,j,o,recover=True,prior_finished=True);self.assertEqual(out['outcome'],'introspection_restored_closed');self.assertEqual(o.loaded,'old')
     else:
      self.assertEqual(j.data['phase'],'introspection_store_pending');records=json.loads((root/'records').read_text())if(root/'records').exists()else{};b=Broker(records);out=i.reconcile(m,j,lambda:b,helper_finished=True);self.assertEqual(out['outcome'],'introspection_stored_not_adopted'if stage=='after_write'else'introspection_not_created');self.assertEqual(b.calls,0)
    finally:j.close()
if __name__=='__main__':unittest.main()
