"""Public dummy key only; no real source path, key or service is accessed."""
import base64,contextlib,io,json,os,pwd,subprocess,sys,tempfile,types,unittest
from pathlib import Path
from unittest.mock import patch
import adapters as a
import introspection as i
import introspection_owner as o
from provision import Refused

KEY=base64.b64encode(bytes([7])*32)
FIELD=b'PHILOTIC_VAULT_MASTER_KEY='

class Tests(unittest.TestCase):
 def test_supported_canonical_single_line_syntax(self):
  for value in(KEY,b'"'+KEY+b'"',b"'"+KEY+b"'"):
   for newline in(b'\n',b'\r\n'):
    raw=newline.join([b'# public synthetic fixture',b'; ignored',b'OTHER=value with interior spaces',b'  PHILOTIC_VAULT_MASTER_KEY \t= \t'+value+b' \t',b''])
    with self.subTest(value=value,newline=newline):self.assertEqual(a.selected_existing_key(raw),KEY)
 def test_missing_duplicate_and_noncanonical_values_refuse_without_disclosure(self):
  for raw in(b'OTHER=SYNTHETIC_CANARY\n',FIELD+KEY+b'\n'+FIELD+KEY,FIELD+b'bad_SYNTHETIC_CANARY',FIELD+b'',FIELD+KEY[:-2]+b'B=',FIELD+KEY+b' # comment',b'export '+FIELD+KEY,FIELD+b'${OTHER}',FIELD+b'$(echo SYNTHETIC_CANARY)'):
   with self.subTest(raw=raw):
    with self.assertRaises(Refused)as caught:a.selected_existing_key(raw)
    self.assertEqual(str(caught.exception),'existing_key_source_refused');self.assertIsNone(caught.exception.__context__)
 def test_ambiguous_multiline_escape_and_comment_continuation_refuse(self):
  for prefix in(b'OTHER="first\n',b"OTHER='first\n",b'OTHER=first\\\n',b'# comment\\\n',b'; comment\\\n'):
   self.assertRaises(Refused,a.selected_existing_key,prefix+FIELD+KEY+b'\n')
  for raw in(FIELD+b'"'+KEY+b'" suffix',FIELD+b'"'+KEY[:20]+b'\n'+KEY[20:]+b'"',FIELD+KEY+b'\x00',b'\xef\xbb\xbf'+FIELD+KEY,b'\xff'+FIELD+KEY,b'x'*65537):self.assertRaises(Refused,a.selected_existing_key,raw)
 def fixture(self):
  temp=tempfile.TemporaryDirectory(dir=Path(__file__).parent);self.addCleanup(temp.cleanup)
  fs=o.Files(str(Path(temp.name).resolve()),{0,os.geteuid()});p=fs.path(a.EXISTING_KEY_SOURCE);p.parent.mkdir(parents=True);p.write_bytes(FIELD+KEY+b'\nOTHER=SYNTHETIC_PRIVATE_CANARY\n');p.chmod(0o600)
  realread=fs.read;fixture_owner=(p.stat().st_uid,p.stat().st_gid,0o600)
  def read(path,*args,**kwargs):
   self.assertEqual(path,a.EXISTING_KEY_SOURCE);self.assertEqual(args,(65536,));self.assertEqual(kwargs,{'private_identity':(999,987,0o600)})
   # Map the verified production principal to this ordinary-user fixture only.
   raw,meta=realread(path,*args,private_identity=fixture_owner);meta.update(uid=999,gid=987);return raw,meta
  fs.read=read
  flags=types.SimpleNamespace(**{k:getattr(sys.flags,k)for k in dir(sys.flags)if not k.startswith('_')and not callable(getattr(sys.flags,k))});flags.isolated=1
  self.enterContext(patch.object(a.sys,'flags',flags));self.enterContext(patch.object(a.sys,'platform','linux'));self.enterContext(patch.object(a.os,'geteuid',return_value=0))
  self.enterContext(patch.object(a,'protect_operator'));self.enterContext(patch.object(pwd,'getpwnam',side_effect=lambda n:types.SimpleNamespace(pw_uid=999 if n=='philotic'else 501,pw_gid=987 if n=='philotic'else 20)))
  self.enterContext(patch.object(o,'Files',return_value=fs));identity=self.enterContext(patch.object(o.Owner,'identity'))
  self.enterContext(patch.object(o.Owner,'query',return_value='EnvironmentFiles='+a.EXISTING_KEY_SOURCE+' (ignore_errors=no)\nUnsetEnvironment=\nNeedDaemonReload=no\n'))
  return fs,p,identity
 def test_protected_source_reader_returns_only_key_and_rechecks_identity(self):
  fs,p,identity=self.fixture();out=io.StringIO()
  with contextlib.redirect_stdout(out),contextlib.redirect_stderr(out):self.assertEqual(a.existing_hotel_master_key(),KEY)
  self.assertEqual(out.getvalue(),'');self.assertEqual(identity.call_count,2)
 def test_source_symlink_hardlink_mode_and_absence_refuse(self):
  for mode in('symlink','parent_symlink','hardlink','world_readable','absent','oversized'):
   with self.subTest(mode=mode),contextlib.ExitStack()as stack:
    # Separate test context avoids retaining nested fixture patchers.
    case=Tests();stack.callback(case.doCleanups);fs,p,_=case.fixture()
    if mode=='symlink':other=p.with_name('other');p.rename(other);p.symlink_to(other)
    elif mode=='parent_symlink':parent=p.parent;other=parent.with_name('other');parent.rename(other);parent.symlink_to(other,target_is_directory=True)
    elif mode=='hardlink':os.link(p,p.with_name('other'))
    elif mode=='world_readable':p.chmod(0o644)
    elif mode=='absent':p.unlink()
    else:p.write_bytes(b'x'*65537)
    with self.assertRaises(Refused)as caught:a.existing_hotel_master_key()
    self.assertEqual(str(caught.exception),'existing_key_source_refused');self.assertIsNone(caught.exception.__context__)
 def test_identity_or_protection_failure_precedes_source_read(self):
  fs,p,identity=self.fixture();identity.side_effect=Refused('SYNTHETIC_PRIVATE_CANARY')
  with patch.object(fs,'read',side_effect=AssertionError('must not read')):self.assertRaises(Refused,a.existing_hotel_master_key)
  identity.side_effect=None
  with patch.object(a,'protect_operator',side_effect=OSError('SYNTHETIC_PRIVATE_CANARY')),patch.object(fs,'read',side_effect=AssertionError('must not read')):self.assertRaises(Refused,a.existing_hotel_master_key)
 def test_source_owner_and_content_drift_refuse(self):
  fs,p,_=self.fixture();read=fs.read;count=0
  def drift(path,*args,**kwargs):
   nonlocal count
   count+=1;raw,meta=read(path,*args,**kwargs);return(raw+b'changed'if count>1 else raw),meta
  with patch.object(fs,'read',side_effect=drift):self.assertRaises(Refused,a.existing_hotel_master_key)
  def foreign(path,*args,**kwargs):raw,meta=read(path,*args,**kwargs);meta['uid']=1003;return raw,meta
  with patch.object(fs,'read',side_effect=foreign):self.assertRaises(Refused,a.existing_hotel_master_key)
 def test_registration_mismatch_or_final_removal_refuses_before_read(self):
  fs,_,_=self.fixture();good='EnvironmentFiles='+a.EXISTING_KEY_SOURCE+' (ignore_errors=no)\nUnsetEnvironment=\nNeedDaemonReload=no\n'
  for text in('',good.replace('ignore_errors=no','ignore_errors=yes'),good.replace(a.EXISTING_KEY_SOURCE,'/other/source'),good+'EnvironmentFiles=/other/source (ignore_errors=no)\n',good.replace('NeedDaemonReload=no','NeedDaemonReload=yes'),good.replace('UnsetEnvironment=','UnsetEnvironment=PHILOTIC_VAULT_MASTER_KEY'),good.replace('UnsetEnvironment=','UnsetEnvironment=PHILOTIC_VAULT_MASTER_KEY=SYNTHETIC_PRIVATE_CANARY')):
   with self.subTest(text=text),patch.object(o.Owner,'query',return_value=text),patch.object(fs,'read',side_effect=AssertionError('must not read key')):
    with self.assertRaises(Refused)as caught:a.existing_hotel_master_key()
    self.assertEqual(str(caught.exception),'existing_key_source_refused');self.assertIsNone(caught.exception.__context__)
  with patch.object(o.Owner,'query',return_value=good.replace('UnsetEnvironment=','UnsetEnvironment=UNRELATED')):self.assertEqual(a.existing_hotel_master_key(),KEY)
 def test_private_identity_is_checked_before_any_source_bytes(self):
  with tempfile.TemporaryDirectory(dir=Path(__file__).parent)as td:
   fs=o.Files(str(Path(td).resolve()),{0,os.geteuid()});p=fs.path(a.EXISTING_KEY_SOURCE);p.parent.mkdir(parents=True);p.write_bytes(FIELD+KEY);p.chmod(0o600)
   with patch.object(o.os,'read',side_effect=AssertionError('private bytes read before identity check')):self.assertRaises(Refused,fs.read,a.EXISTING_KEY_SOURCE,65536,private_identity=(1003,1003,0o600))
 def cli(self,confirm=True,source_failure=False,action='introspection-apply'):
  import provision
  temp=tempfile.TemporaryDirectory(dir=Path(__file__).parent);self.addCleanup(temp.cleanup);root=Path(temp.name).resolve();(root/'persistent-broker-helper').write_bytes(b'public fixture helper')
  m={'version':2,'scope':i.INTROSPECTION,'role':i.ROLE,'guest':i.GUEST,'helper_sha256':o.sha(b'public fixture helper')};manifest=root/'manifest';manifest.write_text(json.dumps(m));journal=root/'journal'
  if action=='introspection-reconcile':
   j=provision.Journal(journal);j.data.update(phase='introspection_reconciliation_required',manifest_sha256=i.digest(m),steps={'store_introspection':'pending'},introspection_secret_ref=i.PREFIX+'a'*32,introspection_verifier='b'*64);j.save();j.close()
  args=[str(root/'provision.py'),'introspection-apply','--manifest',str(manifest),'--journal',str(journal),'--master-key-source','hotel-env']
  class TtyCapture(io.StringIO):
   def isatty(self):return True
  out=TtyCapture();flags=types.SimpleNamespace(**{k:getattr(sys.flags,k)for k in dir(sys.flags)if not k.startswith('_')and not callable(getattr(sys.flags,k))});flags.isolated=1
  calls=[]
  class Broker:
   def __init__(self,key,h):calls.append('broker');self.key=key;require=provision.require;require(key==KEY)
   def store_exact_acl(self,**kw):calls.append('store');ref=i.PREFIX+'a'*32;kw['allocation'](ref);return {'reference':ref}
   def reconcile_introspection(self,ref,expected):calls.append('reconcile');return {'status':'stored'}
   def close(self):self.key=None
  def read():
   calls.append('source')
   if source_failure:raise Refused('existing_key_source_refused')
   return KEY
  def consent(prompt):calls.append('confirm');return ('HELPER_FINISHED 'if action=='introspection-reconcile'else'CREATE_INTROSPECTION ')+i.digest(m)if confirm else'cancel'
  with patch.object(sys,'argv',args),patch.object(sys,'flags',flags),patch.object(sys,'platform','linux'),patch.object(provision,'__file__',str(root/'provision.py')),patch.object(i,'__file__',str(root/'introspection.py')),patch.object(a,'BASE',root),patch.object(a,'trusted'),patch.object(a,'protect_operator'),patch.object(a,'Broker',Broker),patch.object(a,'existing_hotel_master_key',side_effect=read),patch.object(sys.stdin,'isatty',return_value=True),patch.object(sys.stdout,'isatty',return_value=True),patch('builtins.input',side_effect=consent),patch('getpass.getpass',side_effect=AssertionError('must not prompt for key')),contextlib.redirect_stdout(out),contextlib.redirect_stderr(out):
   # Actual introspection dispatcher; real disposable Journal and confirmation.
   # The production install/TTY guards remain tested by the exact CLI subprocess
   # refusal below and by the prepared disposable Linux lifecycle.
   import argparse
   parsed=argparse.Namespace(action=action,manifest=str(manifest),journal=str(journal),adoption_manifest=None,master_key_source='hotel-env')
   try:code=i.cli(parsed)
   except Refused:code=2
  self.assertNotIn(KEY.decode(),out.getvalue());self.assertNotIn('SYNTHETIC_PRIVATE_CANARY',out.getvalue());self.assertNotIn('Existing hotel master key (hidden)',out.getvalue())
  return code,calls,journal
 def test_actual_cli_source_is_after_confirmation_and_never_getpass(self):
  code,calls,journal=self.cli();self.assertEqual(code,0);self.assertEqual(calls,['confirm','source','broker','store']);self.assertNotIn(KEY.decode(),journal.read_text())
 def test_cancel_and_source_failure_never_create(self):
  code,calls,_=self.cli(confirm=False);self.assertEqual(code,2);self.assertEqual(calls,['confirm'])
  code,calls,journal=self.cli(source_failure=True);self.assertEqual(code,1);self.assertEqual(calls,['confirm','source']);self.assertEqual(json.loads(journal.read_text())['steps'],{})
 def test_reconciliation_uses_same_explicit_source_after_finished_confirmation(self):
  code,calls,journal=self.cli(action='introspection-reconcile');self.assertEqual(code,0);self.assertEqual(calls,['confirm','source','broker','reconcile']);self.assertEqual(json.loads(journal.read_text())['steps'],{'store_introspection':'observed_stored'})
 def test_argument_parser_never_accepts_arbitrary_path_or_old_action(self):
  program=str(Path(i.__file__).with_name('provision.py'))
  for action,source in(('introspection-plan','hotel-env'),('apply','hotel-env'),('introspection-apply','/tmp/unapproved')):
   result=subprocess.run([sys.executable,'-I','-B',program,action,'--manifest','/nonexistent/public-manifest','--master-key-source',source],capture_output=True,text=True,timeout=3)
   self.assertNotEqual(result.returncode,0);self.assertNotIn('Existing key input',result.stdout)
 def test_exact_entrypoint_foreign_install_refuses_before_source(self):
  with tempfile.TemporaryDirectory()as td:
   p=Path(td)/'m';p.write_text(json.dumps({'version':2,'scope':i.INTROSPECTION,'role':i.ROLE,'guest':i.GUEST,'helper_sha256':'a'*64}))
   result=subprocess.run([sys.executable,'-I','-B',str(Path(i.__file__).with_name('provision.py')),'introspection-apply','--manifest',str(p),'--journal',str(Path(td)/'j'),'--master-key-source','hotel-env'],capture_output=True,text=True,timeout=3)
   self.assertNotEqual(result.returncode,0);self.assertNotIn('Existing key input',result.stdout);self.assertFalse((Path(td)/'j').exists())

if __name__=='__main__':unittest.main()
