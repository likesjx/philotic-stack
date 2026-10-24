import base64,hashlib,importlib.util,json,os,subprocess,sys,tempfile,unittest
from pathlib import Path
from unittest.mock import patch
from provision import Journal,Refused
import introspection as i
class Broker:
 def __init__(self,records,mode='ok'):self.records=records;self.mode=mode;self.calls=0;self.closed=False
 def store_exact_acl(self,**kw):
  self.calls+=1;ref=i.PREFIX+'a'*32;kw['allocation'](ref)
  if self.mode=='before':raise TimeoutError()
  self.records[ref]={'verifier':i.verifier(kw['secret']),'role':kw['allowed_roles'],'guest':kw['allowed_guests']}
  if self.mode=='after':raise TimeoutError()
  return {'reference':ref}
 def reconcile_introspection(self,ref,expected):return {'status':'absent'if ref not in self.records else'stored'if self.records[ref]['verifier']==expected else'mismatch'}
 def close(self):self.closed=True
class Owner:
 def __init__(self):self.policy='old';self.verifier='old';self.loaded='old';self.calls=[];self.fail=None
 def guard(self,data,recover=False):pass
 def capture(self):return {'policy':self.policy,'verifier':self.verifier}
 def action(self,step,data):
  self.calls.append(step)
  if step=='write_policy':self.policy=data['introspection_secret_ref']
  if step=='write_verifier':self.verifier=data['introspection_verifier']
  if step=='restart_hotel_same_binary':self.loaded=self.policy
  if step=='restore_policy':self.policy=data['pair_capture']['policy']
  if step=='restore_verifier':self.verifier=data['pair_capture']['verifier']
  if step==self.fail:raise TimeoutError('synthetic_only')
class Tests(unittest.TestCase):
 def fixture(self,mode='ok'):
  t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup);j=Journal(Path(t.name)/'j');self.addCleanup(j.close);m={'version':2,'scope':i.INTROSPECTION,'role':i.ROLE,'guest':i.GUEST,'helper_sha256':'a'*64};b=Broker({},mode);return m,j,b
 def execute(self,m,j,b):return i.execute(m,j,lambda d:'CREATE_INTROSPECTION '+d,lambda:b)
 def test_introspection_only_allocation_ack_is_durable_no_plaintext_and_no_native(self):
  m,j,b=self.fixture();seen=[]
  with patch.object(i.secrets,'token_urlsafe',return_value='PUBLIC_SYNTHETIC_SECRET_'+'s'*32):out=self.execute(m,j,b)
  self.assertEqual(out['outcome'],'introspection_stored_not_adopted');self.assertFalse(out['nativeKeyCreated']);self.assertTrue(b.closed);self.assertEqual(j.data['steps'],{'store_introspection':'done'});self.assertNotIn('PUBLIC_SYNTHETIC',j.path.read_text());self.assertNotIn('native',j.path.read_text());self.assertRaises(Refused,self.execute,m,j,b)
 def test_lost_reply_reconciles_known_allocation_without_second_create(self):
  for mode,phase in [('before','introspection_not_created'),('after','introspection_stored_not_adopted')]:
   m,j,b=self.fixture(mode);self.assertEqual(self.execute(m,j,b)['outcome'],'introspection_reconciliation_required');self.assertIn('introspection_secret_ref',j.data);self.assertRaises(Refused,i.reconcile,m,j,lambda:b);out=i.reconcile(m,j,lambda:b,helper_finished=True);self.assertEqual(out['outcome'],phase);self.assertEqual(b.calls,1)
 def test_allocation_journal_failure_never_acknowledges_write(self):
  m,j,b=self.fixture();save=j.save
  def fail():
   if 'introspection_secret_ref'in j.data:raise OSError('synthetic')
   save()
  j.save=fail
  with self.assertRaises(OSError):self.execute(m,j,b)
  self.assertEqual(b.records,{})
 def test_unknown_and_conflicting_records_never_allow_adoption_or_recreate(self):
  m,j,b=self.fixture('after');self.execute(m,j,b);b.records[j.data['introspection_secret_ref']]['verifier']='different';self.assertEqual(i.reconcile(m,j,lambda:b,helper_finished=True)['outcome'],'introspection_conflict');self.assertRaises(Refused,i.transition,m,j,Owner());self.assertEqual(b.calls,1)
 def test_policy_cache_requires_restart_and_explicit_recovery_closes_baseline(self):
  m,j,b=self.fixture();self.execute(m,j,b);o=Owner();o.fail='write_verifier';self.assertEqual(i.transition(m,j,o)['outcome'],'introspection_adoption_uncertain');self.assertEqual(o.loaded,'old');self.assertRaises(Refused,i.transition,m,j,o);self.assertRaises(Refused,i.transition,m,j,o,recover=True);o.fail=None;out=i.transition(m,j,o,recover=True,prior_finished=True);self.assertEqual(out['outcome'],'introspection_restored_closed');self.assertEqual((o.policy,o.verifier,o.loaded),('old','old','old'));self.assertEqual(o.calls.count('write_policy'),1)
 def test_success_uses_same_binary_restart_not_hotel_replacement(self):
  m,j,b=self.fixture();self.execute(m,j,b);o=Owner();self.assertEqual(i.transition(m,j,o)['outcome'],'introspection_adopted');self.assertEqual(o.loaded,j.data['introspection_secret_ref']);self.assertEqual(tuple(o.calls),i.ADOPT)
 def test_exact_cli_plan_has_no_journal_handoff_or_effect(self):
  m,j,b=self.fixture();path=j.path.parent/'manifest';path.write_text(json.dumps(m));program=Path(i.__file__).with_name('provision.py');p=subprocess.run([sys.executable,'-I','-B',str(program),'introspection-plan','--manifest',str(path)],capture_output=True,text=True);self.assertEqual(p.returncode,0,p.stderr);out=json.loads(p.stdout);self.assertFalse(out['nativeKeyCreated']);self.assertFalse(out['liveChanges']);self.assertFalse(j.path.exists())
 def test_exact_cli_apply_refuses_local_foreign_installation_before_private_input(self):
  m,j,b=self.fixture();path=j.path.parent/'manifest';path.write_text(json.dumps(m));program=Path(i.__file__).with_name('provision.py');p=subprocess.run([sys.executable,'-I','-B',str(program),'introspection-apply','--manifest',str(path),'--journal',str(j.path)],capture_output=True,text=True,timeout=3);self.assertNotEqual(p.returncode,0);self.assertFalse(j.path.exists());self.assertNotIn('Existing hotel master key',p.stdout)
if __name__=='__main__':unittest.main()
