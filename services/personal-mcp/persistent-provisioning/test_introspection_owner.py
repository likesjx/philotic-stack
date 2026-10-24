import json,os,tempfile,time,unittest
from pathlib import Path
from unittest.mock import patch
import introspection as i
import introspection_owner as o
from provision import Journal,digest,Refused
class Fixture:
 def __init__(self):
  self.temp=tempfile.TemporaryDirectory(dir=Path(__file__).parent);self.root=Path(self.temp.name).resolve();self.fs=o.Files(str(self.root),{0,os.geteuid()});self.calls=[];self.active=True;self.loaded='old';self.env={'PERSONAL_OAUTH_ENABLED':'false'};self.initial_env=dict(self.env);self.fail=None;self.job='Job=\n'
  self.props={k:''for k in o.PROPS};self.props.update(User='philotic',Group='philotic',ExecStart='{ argv[]=/public/fixture ; }')
  self.m={'schema':1,'host':'srv875272','observed_at':int(time.time()*1000),'policy_sha256':'','verifier_sha256':'','units':{},'source_files':{},'compose':{'project':'synthetic','working_directory':'/home/deploy/jaredlikes-com','files':[],'env_sha256':None},'access_boundary':'issuer-disabled-gateway-stopped-before-publication'}
  self.put(o.POLICY,b'{"caller_uid":1003,"hotel_uid":999,"secret_ref":"secret://hotel/default/percival-muninn-observe/unchanged", "introspection_secret_ref":"secret://hotel/default/percival-issuer-introspection/old"}\n',0o644);self.put(o.VERIFIER,b'PERSONAL_OAUTH_INTROSPECTION_VERIFIER='+b'b'*64+b'\n')
  for unit in o.UNITS:
   path='/etc/systemd/system/'+unit;self.put(path,b'[public fixture]\n',0o644);self.m['source_files'][path]=o.sha(self.fs.read(path)[0])
  compose='/home/deploy/jaredlikes-com/docker-compose.yml';self.put(compose,('services:\n  app:\n    image: '+o.REGISTRY+'\n').encode());self.m['compose']['files']=[{'path':compose,'sha256':o.sha(self.fs.read(compose)[0])}];self.m['policy_sha256']=o.sha(self.fs.read(o.POLICY)[0]);self.m['verifier_sha256']=o.sha(self.fs.read(o.VERIFIER)[0])
  for unit in o.UNITS:
   normalized=dict(self.props);normalized['FragmentPath']='/etc/systemd/system/'+unit
   for key in o.PROPS:
    if key.startswith('Exec'):normalized[key]=['/public/fixture']if key=='ExecStart'else[]
   self.m['units'][unit]=digest(normalized)
  self.owner=o.Owner(self.m,self.fs)
  self.transport=patch.object(o,'command',side_effect=self.command);self.transport.start();self.identity=patch.object(self.owner,'identity');self.identity.start();self.getter=patch.object(self.owner,'getter_verifier',side_effect=lambda:self.env.get('PERSONAL_OAUTH_INTROSPECTION_VERIFIER'));self.getter.start()
  for unit in o.UNITS:self.m['units'][unit]=digest(self.owner.unit(unit))
  self.j=Journal(self.root/'journal');self.j.data.update(phase='introspection_stored_not_adopted',steps={'store_introspection':'done'},introspection_secret_ref=i.PREFIX+'a'*32,introspection_verifier='c'*64);self.manifest={'version':2,'scope':i.INTROSPECTION,'role':i.ROLE,'guest':i.GUEST,'helper_sha256':'d'*64};self.j.data['manifest_sha256']=digest(self.manifest);self.j.save()
 def put(self,path,data,mode=0o600):p=self.fs.path(path);p.parent.mkdir(parents=True,exist_ok=True);p.write_bytes(data);p.chmod(mode)
 def close(self):self.j.close();self.getter.stop();self.identity.stop();self.transport.stop();self.temp.cleanup()
 def command(self,argv,seconds,capture=False):
  self.calls.append(argv)
  if argv[0]=='/usr/bin/timeout':argv=argv[4:]
  if argv[0]=='/usr/bin/systemctl':
   if argv[1]=='show':
    if '--property=Job'in argv:
     assert '--all'in argv and '--value'not in argv
     return self.job
    if '--property=ActiveState'in argv:return 'active\n'if self.active else'inactive\n'
    props=dict(self.props);props['FragmentPath']='/etc/systemd/system/'+argv[2];return '\n'.join(k+'='+v for k,v in props.items())
   if argv[1]=='stop':self.active=False
   elif argv[1]=='restart':self.loaded=json.loads(self.fs.read(o.POLICY)[0])['introspection_secret_ref']
   else:raise AssertionError(argv)
  elif argv[:len(o.DOCKER)]==o.DOCKER:
   if 'inspect'in argv:return json.dumps(o.IMAGE)if '{{json .Image}}'in argv else'\n'.join(json.dumps(k+'='+v)for k,v in self.env.items())+'\n\n'
   assert 'compose'in argv and '--host'in argv and '--no-build'in argv or'config'in argv
   if 'up'in argv:
    if any(p.endswith('/muninn-introspection-only.override.yml')for p in argv):self.env={'PERSONAL_OAUTH_ENABLED':'false','PERSONAL_OAUTH_INTROSPECTION_CLIENT':i.INTROSPECTION,'PERSONAL_OAUTH_INTROSPECTION_VERIFIER':'c'*64}
    else:self.env=dict(self.initial_env)
  else:raise AssertionError(argv)
  if self.fail and self.fail in argv:raise TimeoutError('synthetic')
  return ''if capture else None
class Tests(unittest.TestCase):
 def test_disposable_directory_modes_do_not_depend_on_umask(self):
  from linux_introspection_acceptance import fixture_mkdir
  import stat
  with tempfile.TemporaryDirectory()as root:
   parent=Path(root);before=stat.S_IMODE(parent.stat().st_mode);old=os.umask(0)
   try:fixture_mkdir(parent/'first/second')
   finally:os.umask(old)
   self.assertEqual(stat.S_IMODE(parent.stat().st_mode),before)
   for name in('first','first/second'):self.assertEqual(stat.S_IMODE((parent/name).stat().st_mode),0o755)
 def f(self):f=Fixture();self.addCleanup(f.close);return f
 def test_dependency_order_is_canonical_but_membership_drift_refuses(self):
  f=self.f();f.props['After']='second.target first.target'
  f.owner.m['units']={unit:digest(f.owner.unit(unit))for unit in o.UNITS}
  f.props['After']='first.target second.target';f.owner.guard(f.j.data)
  for changed in('first.target second.target extra.target','first.target'):
   f.props['After']=changed
   with self.subTest(changed=changed):self.assertRaises(Refused,f.owner.guard,f.j.data)
 def test_filtered_inspect_blank_separator_does_not_hide_malformed_data(self):
  f=self.f();self.assertEqual(f.owner.issuer_env(),{'PERSONAL_OAUTH_ENABLED':'false'})
  with patch.object(f.owner,'query',return_value=' \n'):self.assertRaises(ValueError,f.owner.issuer_env)
  with patch.object(f.owner,'query',return_value='\n'):self.assertRaises(Refused,f.owner.issuer_env)
 def test_job_requires_explicit_idle_property(self):
  f=self.f();f.owner.guard(f.j.data)
  for value in('', '0\n', '[not set]\n', 'Job=23\n', 'Other=\n'):
   f.job=value
   with self.subTest(value=value):self.assertRaises(Refused,f.owner.guard,f.j.data)
 def test_real_adapter_capture_cas_command_assembly_pair_and_recovery(self):
  f=self.f();original=f.fs.read(o.POLICY)[0];self.assertEqual(i.transition(f.manifest,f.j,f.owner)['outcome'],'introspection_adopted');self.assertEqual(f.loaded,f.j.data['introspection_secret_ref']);self.assertIn(b'unchanged',f.fs.read(o.POLICY)[0]);self.assertFalse(f.active);self.assertEqual(i.transition(f.manifest,f.j,f.owner,recover=True,prior_finished=True)['outcome'],'introspection_restored_closed');self.assertEqual(f.fs.read(o.POLICY)[0],original);self.assertEqual(f.env,{'PERSONAL_OAUTH_ENABLED':'false'});self.assertFalse(f.fs.path('/home/deploy/jaredlikes-com/muninn-introspection-only.override.yml').exists());self.assertTrue(f.fs.path('/home/deploy/jaredlikes-com/muninn-introspection-only.rollback.yml').exists())
 def test_real_adapter_refuses_file_drift_before_any_mutation(self):
  f=self.f();f.fs.path(o.POLICY).write_bytes(b'foreign');self.assertRaises(Refused,i.transition,f.manifest,f.j,f.owner);self.assertTrue(f.active)
 def test_new_dropin_refuses_before_service_commands(self):
  f=self.f();f.put('/etc/systemd/system/philotic-hotel.service.d/99-new.conf',b'new',0o644);self.assertRaises(Refused,i.transition,f.manifest,f.j,f.owner);self.assertTrue(f.active)
 def test_failed_real_command_keeps_pending_and_never_repeats(self):
  f=self.f();f.fail='restart';self.assertEqual(i.transition(f.manifest,f.j,f.owner)['outcome'],'introspection_adoption_uncertain');count=sum('restart'in argv for argv in f.calls);self.assertRaises(Refused,i.transition,f.manifest,f.j,f.owner);self.assertEqual(sum('restart'in argv for argv in f.calls),count)
if __name__=='__main__':unittest.main()

class BoundaryTests(unittest.TestCase):
 def test_listener_filter_rejects_duplicate_listeners_and_ignores_connected_rows(self):
  header=b'Num RefCount Protocol Flags Type St Inode Path\n';path='/run/percival-personal-vault-broker.sock';listen=b'0: 2 0 00010000 0001 01 123 '+path.encode()+b'\n';connected=b'0: 2 0 00000000 0001 03 456 '+path.encode()+b'\n'
  self.assertEqual(o.listener_inodes(header+listen+connected,path),[b'123'])
  for table in(header+connected,header+listen+listen):self.assertRaises(Refused,o.listener_inodes,table,path)
 def test_only_root_sticky_tmp_is_exception(self):
  import types
  self.assertTrue(o.parent_allowed(Path('/tmp'),types.SimpleNamespace(st_uid=0,st_mode=0o41777),{0}))
  for path,uid,mode in[('/other',0,0o41777),('/tmp',1,0o41777),('/tmp',0,0o40777)]:self.assertFalse(o.parent_allowed(Path(path),types.SimpleNamespace(st_uid=uid,st_mode=mode),{0,1}))
 def test_streaming_overflow_and_exited_leader_descendant_cleanup(self):
  import subprocess,sys,time
  with self.assertRaises(Exception):o.command([sys.executable,'-I','-B','-c','import os;os.write(1,b"x"*65536)'],2,True)
  with tempfile.TemporaryDirectory(dir=Path(__file__).parent)as td:
   path=Path(td)/'heartbeat';code='import subprocess,sys,os;subprocess.Popen([sys.executable,"-c",'+repr('import time;from pathlib import Path;p=Path('+repr(str(path.resolve()))+');\nwhile True:p.write_text(str(time.monotonic()));time.sleep(.01)')+']);os._exit(0)'
   with self.assertRaises(Exception):o.command([sys.executable,'-I','-B','-c',code],.2,True)
   before=path.read_text()if path.exists()else None;time.sleep(.1);self.assertEqual(path.read_text()if path.exists()else None,before)

class DurabilityTests(unittest.TestCase):
 def test_interrupted_owned_override_restores_only_recorded_inode(self):
  f=Fixture();self.addCleanup(f.close);create=f.fs.create
  def interrupted(path,raw,allocated=None):
   if path.endswith('/muninn-introspection-only.override.yml'):create(path,raw[:12],allocated);raise TimeoutError()
   return create(path,raw,allocated)
  f.fs.create=interrupted;self.assertEqual(i.transition(f.manifest,f.j,f.owner)['outcome'],'introspection_adoption_uncertain');self.assertEqual(f.j.data['steps']['adopt_adopt_issuer_verifier'],'pending');f.fs.create=create
  self.assertEqual(i.transition(f.manifest,f.j,f.owner,recover=True,prior_finished=True)['outcome'],'introspection_restored_closed');self.assertFalse(f.fs.path('/home/deploy/jaredlikes-com/muninn-introspection-only.override.yml').exists())
 def test_handlers_restore_when_journal_cannot_open(self):
  import signal,sys,types,adapters
  with tempfile.TemporaryDirectory(dir=Path(__file__).parent)as td:
   root=Path(td).resolve();helper=root/'persistent-broker-helper';helper.write_bytes(b'public helper');m={'version':2,'scope':i.INTROSPECTION,'role':i.ROLE,'guest':i.GUEST,'helper_sha256':o.sha(helper.read_bytes())};mp=root/'manifest';mp.write_text(json.dumps(m));a=types.SimpleNamespace(action='introspection-apply',manifest=str(mp),journal=str(root/'journal'))
   flags=types.SimpleNamespace(**{k:getattr(sys.flags,k)for k in dir(sys.flags)if not k.startswith('_')and not callable(getattr(sys.flags,k))});flags.isolated=1;old={s:signal.getsignal(s)for s in(signal.SIGALRM,signal.SIGTERM,signal.SIGHUP,signal.SIGINT)}
   with patch.object(i,'__file__',str(root/'introspection.py')),patch.object(sys,'flags',flags),patch.object(sys.stdin,'isatty',return_value=True),patch.object(sys.stdout,'isatty',return_value=True),patch.object(adapters,'BASE',root),patch.object(adapters,'trusted'),patch.object(adapters,'protect_operator'),patch.object(i,'Journal',side_effect=Refused('synthetic')):
    with self.assertRaises(Refused):i.cli(a)
   self.assertEqual(signal.alarm(0),0)
   for s,h in old.items():self.assertEqual(signal.getsignal(s),h)
