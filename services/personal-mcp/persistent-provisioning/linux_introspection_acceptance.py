#!/usr/bin/env python3
"""Disposable hosted Linux ONLY: real helper, SQLite, systemd, Docker and Owner.
Dummy issuer image / synthetic Python hotel are deliberately NOT production pins.
"""
import base64,contextlib,hashlib,io,json,os,pathlib,pwd,grp,re,shutil,sqlite3,subprocess,sys,tempfile,time,types
from unittest.mock import patch
HERE=pathlib.Path(__file__).resolve().parent;ROOT=pathlib.Path('/opt/percival-personal-mcp/provisioning')
IMAGE='nginx@sha256:5616878291a2eed594aee8db4dade5878cf7edcb475e59193904b198d9b830de'
UNITS=['philotic-hotel.service','percival-personal-mcp.service','percival-personal-vault.socket']
ROOTS=[pathlib.Path(p)for p in('/opt/philotic',str(ROOT),'/etc/percival-personal-mcp','/home/deploy/.config/jaredlikes-com','/home/deploy/jaredlikes-com','/tmp/percival-identity-build-introspection-fixture')]
HOTEL='''import socket,struct,json,sqlite3
from pathlib import Path
p=json.loads(Path('/etc/percival-personal-mcp/scoped-vault-policy.json').read_text());s=socket.socket(fileno=3)
while True:
 c,_=s.accept()
 with c:
  def read(n):
   out=b''
   while len(out)<n:
    part=c.recv(n-len(out))
    if not part:raise ValueError()
    out+=part
   return out
  try:
   n=struct.unpack('!I',read(4))[0];q=json.loads(read(n));assert q=={'operation':'get_percival_introspection_credential'}
   db=sqlite3.connect('file:/opt/philotic/data/aiua_context.db?mode=ro',uri=True);exists=db.execute('SELECT 1 FROM graph_nodes WHERE node_key=?',('secret:'+p['introspection_secret_ref'],)).fetchone();db.close()
   value={'credential':'S'*43}if exists else{'error':'credential_unavailable'}
   raw=json.dumps(value).encode();c.sendall(struct.pack('!I',len(raw))+raw)
  except Exception:pass
'''
def run(argv):return subprocess.check_output(argv,stderr=subprocess.DEVNULL,text=True,timeout=120).strip()
def fixture_mkdir(path):
 # Set modes on newly created fixture directories explicitly: hosted filesystem
 # inheritance can produce 0777 even with an explicit process umask.
 path=pathlib.Path(path);missing=[];part=path
 while not part.exists():missing.append(part);part=part.parent
 path.mkdir(parents=True,exist_ok=True)
 for part in reversed(missing):
  assert part.lstat().st_uid==os.geteuid()and not part.is_symlink();part.chmod(0o755)
def write(path,raw,mode=0o644):
 path=pathlib.Path(path);fixture_mkdir(path.parent);path.write_text(raw);path.chmod(mode)
def main():
 assert sys.argv[1:]==['--disposable-public-fixture']and sys.platform=='linux'and os.geteuid()==0 and os.environ.get('GITHUB_ACTIONS')=='true'
 assert not any(p.exists()or p.is_symlink()for p in ROOTS)
 assert all(not os.path.lexists(pathlib.Path('/etc/systemd/system',u))for u in UNITS)
 assert subprocess.run(['docker','inspect','jaredlikes-com-production'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode!=0
 assert pathlib.Path('/proc/sys/fs/suid_dumpable').read_text().strip()in('0','2')
 parent_mode=ROOT.parent.stat().st_mode&0o7777 if ROOT.parent.exists()else None
 opt=pathlib.Path('/opt');opt_stat=opt.lstat();assert opt_stat.st_uid==0 and not opt.is_symlink()
 opt_mode=opt_stat.st_mode&0o7777
 original_umask=os.umask(0o022)
 created_users=[];created_groups=[];work=pathlib.Path(tempfile.mkdtemp(prefix='introspection-owner-ci-',dir='/run'));project='muninn-introspection-fixture';cases=[]
 try:
  for name,uid,gid in [('philotic',999,987),('percival-gateway',1003,1003)]:
   try:account=pwd.getpwnam(name);assert(account.pw_uid,account.pw_gid)==(uid,gid)
   except KeyError:
    try:grp.getgrgid(gid);raise AssertionError('unexpected existing gid')
    except KeyError:subprocess.run(['groupadd','--gid',str(gid),name],check=True);created_groups.append(name)
    try:pwd.getpwuid(uid);raise AssertionError('unexpected existing uid')
    except KeyError:subprocess.run(['useradd','--system','--uid',str(uid),'--gid',str(gid),'--no-create-home','--shell','/usr/sbin/nologin',name],check=True);created_users.append(name)
  try:pwd.getpwnam('deploy')
  except KeyError:subprocess.run(['useradd','--system','--no-create-home','--shell','/usr/sbin/nologin','deploy'],check=True);created_users.append('deploy')
  fixture_mkdir(ROOT)
  # mkdir(parents=True) does not apply its mode to intermediate directories.
  # The preceding disposable handoff fixture may leave this root-owned parent
  # at the runner's group-writable umask. Normalize only this fixture parent,
  # preserve its original mode, and keep the production source guard unchanged.
  assert ROOT.parent.lstat().st_uid==0 and not ROOT.parent.is_symlink()
  ROOT.parent.chmod(0o755)
  # Hosted runners expose /opt as 0777. Protect this disposable installation
  # for the test rather than relaxing the production ancestry requirement.
  opt.chmod(opt_mode&~0o022)
  for part in [ROOT,*ROOT.parents]:
   st=part.lstat();assert st.st_uid==0 and not part.is_symlink()and not st.st_mode&0o022,('untrusted_fixture_install_parent',str(part),st.st_uid,oct(st.st_mode&0o777))
  for name in('provision.py','adapters.py','introspection.py','introspection_owner.py','introspection_compose.py','private_key_handoff.py'):shutil.copyfile(HERE/name,ROOT/name);(ROOT/name).chmod(0o644)
  binary=HERE.parent/'synthetic-broker-build/broker-helper/target/x86_64-unknown-linux-gnu/release/persistent-broker-helper';shutil.copyfile(binary,ROOT/'persistent-broker-helper');(ROOT/'persistent-broker-helper').chmod(0o755)
  helper_sha=hashlib.sha256(binary.read_bytes()).hexdigest()
  data=pathlib.Path('/opt/philotic/data');fixture_mkdir(data);os.chown(data,999,987);db=sqlite3.connect(data/'aiua_context.db');db.execute('CREATE TABLE graph_nodes(node_key TEXT PRIMARY KEY,kind TEXT NOT NULL,label TEXT,data_json TEXT NOT NULL,updated_at DATETIME DEFAULT CURRENT_TIMESTAMP)');db.commit();db.close();os.chown(data/'aiua_context.db',999,987);(data/'aiua_context.db').chmod(0o600)
  key_source=pathlib.Path('/opt/philotic/etc/secrets.env');write(key_source,'# disposable public fixture only\nPHILOTIC_VAULT_MASTER_KEY='+base64.b64encode(bytes([7])*32).decode()+'\n',0o600);os.chown(key_source,999,987)
  policy='/etc/percival-personal-mcp/scoped-vault-policy.json';before_policy=json.dumps({'caller_uid':1003,'hotel_uid':999,'secret_ref':'secret://hotel/default/percival-muninn-observe/untouched','introspection_secret_ref':'secret://hotel/default/percival-issuer-introspection/missing'})+'\n';write(policy,before_policy)
  verifier='/home/deploy/.config/jaredlikes-com/personal-oauth-verifier.env';before_verifier='PERSONAL_OAUTH_INTROSPECTION_VERIFIER='+'b'*64+'\n';write(verifier,before_verifier,0o600)
  hotel=pathlib.Path('/opt/philotic/bin/aiua');fixture_mkdir(hotel.parent);shutil.copyfile(pathlib.Path('/usr/bin/python3').resolve(),hotel);hotel.chmod(0o755);write('/opt/philotic/fixture_hotel.py',HOTEL)
  write('/etc/systemd/system/philotic-hotel.service','[Unit]\nRequires=percival-personal-vault.socket\nAfter=percival-personal-vault.socket\n[Service]\nUser=philotic\nGroup=philotic\nEnvironmentFile=/opt/philotic/etc/secrets.env\nExecStart=/opt/philotic/bin/aiua -I -B /opt/philotic/fixture_hotel.py\n')
  write('/etc/systemd/system/percival-personal-vault.socket','[Socket]\nListenStream=/run/percival-personal-vault-broker.sock\nSocketUser=percival-gateway\nSocketGroup=percival-gateway\nSocketMode=0600\nRemoveOnStop=yes\nService=philotic-hotel.service\n')
  write('/etc/systemd/system/percival-personal-mcp.service','[Service]\nUser=percival-gateway\nGroup=percival-gateway\nExecStart=/usr/bin/sleep 600\n')
  run(['systemctl','daemon-reload']);run(['systemctl','start','percival-personal-vault.socket','philotic-hotel.service','percival-personal-mcp.service'])
  compose=pathlib.Path('/home/deploy/jaredlikes-com/docker-compose.yml');write(compose,'services:\n  app:\n    image: '+IMAGE+'\n    container_name: jaredlikes-com-production\n    network_mode: none\n    environment:\n      PERSONAL_OAUTH_ENABLED: "false"\n')
  override=pathlib.Path('/tmp/percival-identity-build-introspection-fixture/production-fixture.override.yml');write(override,'services:\n  app:\n    environment:\n      PUBLIC_FIXTURE: "true"\n');override.parent.chmod(0o700)
  base=['docker','compose','--project-name',project,'--project-directory',str(compose.parent),'-f',str(compose),'-f',str(override)]
  run(base+['up','-d','--no-deps','--no-build','--pull','never','--wait','app']);image_id=run(['docker','inspect','--format','{{.Image}}','jaredlikes-com-production'])
  sys.path.insert(0,str(ROOT));import provision,introspection as intro,introspection_owner as owner
  # Pins replaced only in this disposable fixture process, never source/CLI flags.
  fixture_sha=hashlib.sha256(hotel.read_bytes()).hexdigest();owner.B7=fixture_sha;owner.IMAGE=image_id;owner.REGISTRY=IMAGE
  # Preserve actual adapter behavior; report only code locations on fixture
  # refusals, never exception values, credentials, command arguments or files.
  def observed(method):
   def call(*args,**kwargs):
    try:return method(*args,**kwargs)
    except BaseException as error:
     frames=[];tb=error.__traceback__
     while tb is not None:
      name=pathlib.Path(tb.tb_frame.f_code.co_filename).name
      if name in('introspection_owner.py','introspection_compose.py','provision.py'):frames.append({'source':name,'function':tb.tb_frame.f_code.co_name,'line':tb.tb_lineno})
      tb=tb.tb_next
     print(json.dumps({'fixtureAdapterRefusal':method.__name__,'exceptionClass':type(error).__name__,'locations':frames}),file=sys.stderr,flush=True)
     for unit,before in unit_values.items():
      try:
       after=args[0].unit(unit);keys=[key for key in owner.PROPS if after[key]!=before[key]]
       print(json.dumps({'fixtureUnit':unit,'changedPropertyNames':keys}),file=sys.stderr,flush=True)
      except Exception:print(json.dumps({'fixtureUnit':unit,'comparison':'unavailable'}),file=sys.stderr,flush=True)
     raise
   return call
  owner.Owner.guard=observed(owner.Owner.guard);owner.Owner.action=observed(owner.Owner.action)
  manifest={'version':2,'scope':intro.INTROSPECTION,'role':intro.ROLE,'guest':intro.GUEST,'helper_sha256':helper_sha};manifest_path=work/'manifest';write(manifest_path,json.dumps(manifest),0o600);journal=work/'journal'
  def consent(prompt):
   m=re.search(r'(CREATE_INTROSPECTION|ADOPT_INTROSPECTION|RECOVER_FINISHED|HELPER_FINISHED) ([a-f0-9]{64}):',prompt);assert m;return m[1]+' '+m[2]
  def cli(action,adoption=None,key_source=None):
   argv=[str(ROOT/'provision.py'),action,'--manifest',str(manifest_path),'--journal',str(journal)]
   if adoption:argv+=['--adoption-manifest',str(adoption)]
   if key_source:argv+=['--master-key-source',key_source]
   class TtyCapture(io.StringIO):
    def isatty(self):return True
   out=TtyCapture()
   with patch.object(sys,'argv',argv),patch.object(sys.stdin,'isatty',return_value=True),patch.object(sys.stdout,'isatty',return_value=True),patch('builtins.input',side_effect=consent),patch('getpass.getpass',side_effect=AssertionError('fixture must use the fixed file source')),patch.object(intro.secrets,'token_urlsafe',return_value='S'*43),patch.object(owner.socket,'gethostname',return_value='srv875272'),contextlib.redirect_stdout(out):code=provision.main()
   if code!=0:
    state=json.loads(journal.read_text())if journal.exists()else{}
    print(json.dumps({'fixtureCliFailed':action,'phase':state.get('phase'),'steps':state.get('steps',{})}),flush=True)
   assert code==0,out.getvalue();return json.loads(out.getvalue().splitlines()[-1])
  broker=owner.Owner.__new__(owner.Owner);units={};files={};unit_values={}
  for unit in owner.UNITS:
   value=broker.unit(unit);unit_values[unit]=value;units[unit]=provision.digest(value)
   for path in[value['FragmentPath'],*value['DropInPaths'].split()]:files[path]=hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()
  baseline={'schema':1,'host':'srv875272','observed_at':int(time.time()*1000),'policy_sha256':hashlib.sha256(pathlib.Path(policy).read_bytes()).hexdigest(),'verifier_sha256':hashlib.sha256(pathlib.Path(verifier).read_bytes()).hexdigest(),'units':units,'source_files':files,'compose':{'project':project,'working_directory':str(compose.parent),'files':[{'path':str(p),'sha256':hashlib.sha256(p.read_bytes()).hexdigest()}for p in(compose,override)],'env_sha256':None},'access_boundary':'issuer-disabled-gateway-stopped-before-publication'}
  adoption=work/'adoption';write(adoption,json.dumps(baseline),0o600)
  # Validate the complete disposable installation before creating even a public
  # fixture credential. Only fixed fixture path metadata may be diagnostic output.
  checked=set()
  for target in [str(hotel),policy,verifier,*files,str(compose),str(override)]:
   for path in [pathlib.Path(target),*pathlib.Path(target).parents]:
    if str(path)not in checked:
     st=path.lstat();checked.add(str(path));print(json.dumps({'fixturePath':str(path),'uid':st.st_uid,'mode':oct(st.st_mode&0o7777),'symlink':path.is_symlink()}),flush=True)
   owner.Files().read(target,268435456 if target==str(hotel)else 1048576)
  with patch.object(owner.socket,'gethostname',return_value='srv875272'):owner.Owner(baseline).guard({'steps':{}})
  cases.append('complete_real_owner_preflight_before_create')
  import adapters
  with patch.object(owner.socket,'gethostname',return_value='srv875272'):
   key_source.chmod(0o644)
   try:
    try:adapters.existing_hotel_master_key();raise AssertionError('source mode drift accepted')
    except provision.Refused:cases.append('real_fixed_source_mode_refusal')
   finally:key_source.chmod(0o600)
   assert adapters.existing_hotel_master_key()==base64.b64encode(bytes([7])*32);cases.append('real_fixed_source_identity_and_owner_read')
  assert cli('introspection-apply',key_source='hotel-env')['outcome']=='introspection_stored_not_adopted';cases+=['actual_cli_fixed_source_protected_helper_create','no_key_prompt_required']
  j=json.loads(journal.read_text());ref=j['introspection_secret_ref'];assert 'S'*43 not in journal.read_text();assert pathlib.Path(policy).read_text()==before_policy
  extra=pathlib.Path('/etc/systemd/system/philotic-hotel.service.d/99-unapproved-fixture.conf');write(extra,'[Service]\nEnvironment=PUBLIC_FIXTURE=true\n')
  try:
   try:cli('introspection-adopt',adoption);raise AssertionError('source drift accepted')
   except provision.Refused:cases.append('real_source_drift_refused_before_mutation')
   assert pathlib.Path(policy).read_text()==before_policy
  finally:extra.unlink();extra.parent.rmdir()
  assert cli('introspection-adopt',adoption)['outcome']=='introspection_adopted';cases+=['real_policy_verifier_cas','actual_same_fixture_binary_systemd_restart','real_docker_compose_verifier_adoption','protected_uid1003_getter_pair']
  assert json.loads(pathlib.Path(policy).read_text())['introspection_secret_ref']==ref;assert run(['systemctl','show','percival-personal-mcp.service','--property=ActiveState','--value'])=='inactive'
  assert cli('introspection-recover',adoption)['outcome']=='introspection_restored_closed';assert pathlib.Path(policy).read_text()==before_policy and pathlib.Path(verifier).read_text()==before_verifier;cases+=['real_rollback_policy_verifier_issuer','closed_gateway_preserved']
  print(json.dumps({'syntheticOnly':True,'cases':cases,'helperSha256':helper_sha,'fixtureHotelSha256':fixture_sha,'fixtureIssuerImage':IMAGE,'productionB7OrIssuerQualified':False,'productionUsed':False,'ttyConsentSimulated':True}))
 finally:
  subprocess.run(['docker','rm','-f','jaredlikes-com-production'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
  subprocess.run(['systemctl','stop',*UNITS],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
  for unit in UNITS:
   p=pathlib.Path('/etc/systemd/system',unit)
   if p.exists():p.unlink()
  subprocess.run(['systemctl','daemon-reload'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=True)
  assert subprocess.run(['docker','inspect','jaredlikes-com-production'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode!=0
  for unit in UNITS:assert subprocess.run(['systemctl','is-active','--quiet',unit]).returncode in(3,4)
  for p in reversed(ROOTS):
   if p.exists():shutil.rmtree(p)
  shutil.rmtree(work)
  for name in reversed(created_users):subprocess.run(['userdel',name],check=True)
  for name in reversed(created_groups):
   # userdel may already remove the user's now-empty private group.
   try:grp.getgrnam(name)
   except KeyError:continue
   subprocess.run(['groupdel',name],check=True)
  if parent_mode is not None:ROOT.parent.chmod(parent_mode)
  elif ROOT.parent.exists():ROOT.parent.rmdir()
  opt.chmod(opt_mode);assert opt.stat().st_mode&0o7777==opt_mode
  os.umask(original_umask)
  assert not pathlib.Path('/run/percival-personal-vault-broker.sock').exists()and not list(pathlib.Path('/run').glob('persistent-key-*'))
  print(json.dumps({'fixtureCleanupCompleted':True,'productionUsed':False}))
if __name__=='__main__':main()
