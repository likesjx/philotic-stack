"""Introspection-only lifecycle. Existing Journal, Broker, crypto and private handoff.
No IO on import. No native API, caller-selected reference, policy hot reload or retry.
"""
import base64,hashlib,json,re,secrets,os,stat,time,copy
from provision import Journal,require,digest,ROLE,GUEST,INTROSPECTION
KIND='percival-issuer-introspection';PREFIX='secret://hotel/default/'+KIND+'/'
def validate(m):
 require(type(m)is dict and set(m)=={'version','scope','role','guest','helper_sha256'})
 require(m['version']==2 and m['scope']==INTROSPECTION and m['role']==ROLE and m['guest']==GUEST and re.fullmatch('[a-f0-9]{64}',m['helper_sha256']))
def plan(m):validate(m);return {'manifest_sha256':digest(m),'scope':INTROSPECTION,'nativeKeyCreated':False,'liveChanges':False,'allocationProtocol':'helper-fresh-id-durable-before-commit'}
def verifier(secret):return hashlib.sha256(('Basic '+base64.b64encode((INTROSPECTION+':'+secret).encode()).decode()).encode()).hexdigest()
def execute(m,journal,confirm,handoff):
 m=json.loads(json.dumps(m));validate(m);bound=digest(m);require(journal.data['phase']=='new'and not journal.data['steps']);require(confirm(bound)=='CREATE_INTROSPECTION '+bound)
 journal.data.update(phase='introspection_prepared',manifest_sha256=bound);journal.save();broker=None;secret=None
 try:
  broker=handoff();secret=secrets.token_urlsafe(32);journal.data['introspection_verifier']=verifier(secret);journal.data['phase']='introspection_store_pending';journal.save()
  def allocated(reference):
   require('introspection_secret_ref'not in journal.data and re.fullmatch(re.escape(PREFIX)+'[a-f0-9]{32}',reference))
   journal.data['introspection_secret_ref']=reference;journal.save()
  result=journal.effect('store_introspection',lambda:broker.store_exact_acl(kind=KIND,scope=INTROSPECTION,secret=secret,allowed_roles=[ROLE],allowed_guests=[GUEST],allocation=allocated))
  require(result=={'reference':journal.data.get('introspection_secret_ref')});journal.data['phase']='introspection_stored_not_adopted';journal.save();return {'outcome':journal.data['phase'],'nativeKeyCreated':False}
 except BaseException:
  journal.data['phase']='introspection_reconciliation_required';journal.save();return {'outcome':journal.data['phase'],'nativeKeyCreated':False}
 finally:
  secret=None
  if broker is not None:broker.close()
def reconcile(m,journal,handoff,*,helper_finished=False):
 validate(m);require(helper_finished is True and journal.data.get('manifest_sha256')==digest(m)and journal.data['phase']in('introspection_prepared','introspection_store_pending','introspection_reconciliation_required'))
 require(set(journal.data['steps'])<= {'store_introspection'}and all(v in('pending','done')for v in journal.data['steps'].values()))
 reference=journal.data.get('introspection_secret_ref');broker=None
 require(reference is None or journal.data['steps'].get('store_introspection')in('pending','done'))
 require(reference is not None or journal.data['steps'].get('store_introspection')!='done')
 try:
  if reference is None:status='absent' # no durable allocation ACK => helper cannot write
  else:
   broker=handoff();status=broker.reconcile_introspection(reference,journal.data['introspection_verifier'])['status'];require(status in('stored','absent','mismatch'))
  if status=='stored':journal.data['steps']['store_introspection']='observed_stored'
  journal.data['phase']={'stored':'introspection_stored_not_adopted','absent':'introspection_not_created','mismatch':'introspection_conflict'}[status];journal.save()
  return {'outcome':journal.data['phase'],'automaticRetry':False}
 finally:
  if broker is not None:broker.close()
ADOPT=('stop_gateway','write_policy','write_verifier','restart_hotel_same_binary','adopt_issuer_verifier','verify_pair')
RESTORE=('stop_gateway','restore_policy','restore_verifier','restart_hotel_same_binary','restore_issuer_verifier','verify_closed')
def transition(m,journal,owner,*,recover=False,prior_finished=False):
 """Reuse existing owner adapter for CAS/publication/service effects.
 Adapter must bind exact captured paths, same binary and guest/dependency gates;
 this module provides ordering/journaling, not an unreviewed live host executor.
 """
 validate(m);require(journal.data.get('manifest_sha256')==digest(m))
 require(prior_finished is True if recover else journal.data['phase']=='introspection_stored_not_adopted')
 if recover:require(journal.data['phase']in('introspection_adoption_prepared','introspection_adoption_pending','introspection_adoption_uncertain','introspection_adopted'))
 require(journal.data['steps'].get('store_introspection')in('done','observed_stored'))
 names=[k for k in journal.data['steps']if k!='store_introspection'];expected=['adopt_'+s for s in ADOPT];require(names==expected[:len(names)] and len(names)<=len(expected))
 for n,key in enumerate(names):require(journal.data['steps'][key]=='done'or n==len(names)-1 and journal.data['steps'][key]=='pending')
 if hasattr(owner,'bind'):owner.bind(journal)
 owner.guard(journal.data,recover=recover) # actual OAuth false; gateway closed before publication; same binary
 if not recover:
  require('pair_capture'not in journal.data);journal.data['pair_capture']=owner.capture();journal.data['phase']='introspection_adoption_prepared';journal.save()
 steps=RESTORE if recover else ADOPT
 journal.data['phase']='introspection_recovery_pending'if recover else'introspection_adoption_pending';journal.save()
 try:
  for step in steps:
   owner.guard(journal.data,recover=recover);journal.effect(('recover_'if recover else'adopt_')+step,lambda step=step:owner.action(step,journal.data))
  journal.data['phase']='introspection_restored_closed'if recover else'introspection_adopted';journal.save();return {'outcome':journal.data['phase'],'automaticRetry':False}
 except BaseException:
  journal.data['phase']='introspection_recovery_uncertain'if recover else'introspection_adoption_uncertain';journal.save();return {'outcome':journal.data['phase'],'automaticRetry':False}

def finalization_eligible(m,journal):
 """Non-mutating eligibility check; caller holds the existing journal lock."""
 validate(m)
 require(journal.data.get('manifest_sha256')==digest(m)and journal.data.get('phase')=='introspection_adoption_uncertain')
 expected={'store_introspection':'done',**{'adopt_'+name:'pending'if name=='verify_pair'else'done'for name in ADOPT}}
 require(journal.data.get('steps')==expected and type(journal.data.get('pair_capture'))is dict)
 require(re.fullmatch(re.escape(PREFIX)+'[a-f0-9]{32}',journal.data.get('introspection_secret_ref','')))
 require(type(journal.data.get('introspection_verifier'))is str and re.fullmatch('[a-f0-9]{64}',journal.data['introspection_verifier']))
 # Refuse consumed/partial finalization before any new private observation.
 history=journal.path.name+'.before-verified-finalization'
 for name in(journal.temporary,history):
  try:os.stat(name,dir_fd=journal.directory,follow_symlinks=False)
  except FileNotFoundError:pass
  else:require(False)
 def read_current():
  readfd=os.open(journal.path.name,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK,dir_fd=journal.directory)
  try:
   st=os.fstat(readfd);require(stat.S_ISREG(st.st_mode)and st.st_nlink==1 and st.st_uid==os.geteuid()and st.st_mode&0o077==0)
   raw=os.read(readfd,65537);require(0<len(raw)<=65536 and json.loads(raw)==journal.data)
   return raw
  finally:os.close(readfd)
 return read_current()

def finalize_verified_adoption(m,journal,replacement,confirm,observe,*,now_ms=None):
 """Verification-only finalization; never replay adoption or perform recovery.

 observe is a trusted private operator adapter, not a caller-supplied receipt.
 It must freshly verify the approved replacement/runtime/configuration and make
 the separately authorized single pairing comparison. No CLI accepts booleans
 as proof. The production adapter/installation is a separate qualification gate.
 """
 m=json.loads(json.dumps(m));replacement=json.loads(json.dumps(replacement));validate(m)
 require(type(replacement)is dict and set(replacement)=={'schema','host','source','runtime_sha256','baseline_sha256','expires_at_ms'})
 require(replacement['schema']==1 and replacement['host']=='srv875272')
 require(all(isinstance(replacement[k],str)and re.fullmatch('[0-9a-f]{64}'if k!='source'else'[0-9a-f]{40}',replacement[k])for k in('source','runtime_sha256','baseline_sha256')))
 clock=now_ms or(lambda:int(time.time()*1000));now=clock()
 require(type(replacement['expires_at_ms'])is int and now<replacement['expires_at_ms']<=now+1800000)
 before=finalization_eligible(m,journal)
 history=journal.path.name+'.before-verified-finalization'
 def read_current():return finalization_eligible(m,journal)
 bound=digest({'schema':1,'manifest_sha256':digest(m),'journal_sha256':hashlib.sha256(before).hexdigest(),'replacement':replacement})
 require(confirm(bound)=='FINALIZE_INTROSPECTION '+bound)
 require(clock()<replacement['expires_at_ms'])
 observed=observe(copy.deepcopy(replacement),copy.deepcopy(journal.data))
 require(type(observed)is dict and set(observed)=={'observed_at_ms','host','source','runtime_sha256','baseline_sha256','identity_verified','policy_matches','verifier_matches','pairing_matches','gateway_stopped','oauth_disabled','prior_operation_finished'})
 now=clock();require(now<replacement['expires_at_ms']and type(observed['observed_at_ms'])is int and 0<=now-observed['observed_at_ms']<=30000)
 require(all(observed[k]==replacement[k]for k in('host','source','runtime_sha256','baseline_sha256')))
 require(all(observed[k]is True for k in('identity_verified','policy_matches','verifier_matches','pairing_matches','gateway_stopped','oauth_disabled','prior_operation_finished')))
 require(read_current()==before) # CAS again after private observation.
 # Retain exact original bytes before the only state transition. O_EXCL makes
 # replay or a partial earlier history write a refusal, never an overwrite.
 fd=os.open(history,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o400,dir_fd=journal.directory)
 try:
  offset=0
  while offset<len(before):
   n=os.write(fd,before[offset:]);require(n>0);offset+=n
  os.fsync(fd)
 finally:os.close(fd)
 os.fsync(journal.directory)
 journal.data['steps']['adopt_verify_pair']='done'
 journal.data['phase']='introspection_adopted'
 journal.data['verification_reconciliation']={'schema':1,'source':replacement['source'],'runtime_sha256':replacement['runtime_sha256'],'observed_at_ms':observed['observed_at_ms'],'history_preserved':True}
 journal.save() # Failure is uncertain; preserve history/.pending and do not retry.
 return {'outcome':'introspection_adopted','historyPreserved':True,'adoptionReplayed':False,'automaticRetry':False}
def cli(a):
 """Existing entrypoint extension; only plan is available without private owner IO."""
 from pathlib import Path
 m=json.loads(Path(a.manifest).read_bytes());prepared=plan(m)
 if a.action=='introspection-plan':print(json.dumps(prepared,sort_keys=True));return 0
 import os,sys,getpass,warnings
 from adapters import Broker,protect_operator,trusted,BASE,existing_hotel_master_key,EXISTING_KEY_SOURCE
 require(Path(__file__).absolute()==BASE/'introspection.py'and sys.flags.isolated and sys.stdin.isatty()and sys.stdout.isatty()and a.journal)
 for file in('provision.py','introspection.py','introspection_owner.py','introspection_compose.py','adapters.py','private_key_handoff.py','persistent-broker-helper'):trusted(BASE/file)
 require(hashlib.sha256((BASE/'persistent-broker-helper').read_bytes()).hexdigest()==m['helper_sha256']);protect_operator();warnings.simplefilter('error',getpass.GetPassWarning)
 source=getattr(a,'master_key_source',None);require(source in(None,'hotel-env')and(source is None or a.action in('introspection-apply','introspection-reconcile')))
 if source=='hotel-env':print('Existing key input: fixed server-private '+EXISTING_KEY_SOURCE+'; no key output')
 def confirm(bound):
  return input('Type CREATE_INTROSPECTION '+bound+': ')
 def handoff():
  key=existing_hotel_master_key()if source=='hotel-env'else getpass.getpass('Existing hotel master key (hidden): ').encode('ascii')
  try:return Broker(key,m['helper_sha256'])
  finally:key=None
 import signal
 signals=(signal.SIGALRM,signal.SIGTERM,signal.SIGHUP,signal.SIGINT);old={s:signal.getsignal(s)for s in signals}
 def cancelled(*_):raise TimeoutError()
 for s in signals:signal.signal(s,cancelled)
 signal.alarm(600)
 j=None
 try:
  j=Journal(a.journal)
  if a.action=='introspection-apply':result=execute(m,j,confirm,handoff)
  elif a.action in('introspection-adopt','introspection-recover'):
   from introspection_owner import Owner
   require(a.adoption_manifest);owner=Owner(json.loads(Path(a.adoption_manifest).read_bytes()));bound=digest({'manifest':m,'owner':owner.m,'journal':j.data})
   recovering=a.action=='introspection-recover';word='RECOVER_FINISHED'if recovering else'ADOPT_INTROSPECTION';require(input('Type '+word+' '+bound+': ')==word+' '+bound)
   result=transition(m,j,owner,recover=recovering,prior_finished=recovering)
  else:
   require(input('Type HELPER_FINISHED '+prepared['manifest_sha256']+': ')=='HELPER_FINISHED '+prepared['manifest_sha256']);result=reconcile(m,j,handoff,helper_finished=True)
  print(json.dumps(result,sort_keys=True));return 0 if result['outcome']in('introspection_stored_not_adopted','introspection_adopted','introspection_restored_closed')else 1
 finally:
  if j is not None:j.close()
  signal.alarm(0)
  for s,h in old.items():signal.signal(s,h)
