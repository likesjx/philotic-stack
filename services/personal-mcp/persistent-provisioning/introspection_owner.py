"""Fixed policy/verifier CAS and existing service/Compose command adapter.
No IO on import/constructor. Production entrypoint has no fixture switch.
Patterns reuse percival_memory_host's private capture/inode-CAS and the issuer
adoption contract's pinned Docker target/Compose chain and bounded commands.
"""
import selectors
import base64,hashlib,json,os,pwd,re,signal,socket,stat,subprocess,tempfile,time
from pathlib import Path
from provision import require,digest
from introspection_compose import baseline_compose
from introspection import PREFIX,INTROSPECTION,verifier
POLICY='/etc/percival-personal-mcp/scoped-vault-policy.json'
VERIFIER='/home/deploy/.config/jaredlikes-com/personal-oauth-verifier.env'
HOTEL='/opt/philotic/bin/aiua';B7='b7d0c9703b3670efc5b93cfc8cb2561b3f80fe7d0d94be65c400987751eb7a17'
IMAGE='sha256:9c218f8fd65bb2a888976658edf3aded3ab0978f5435fe93e08740e1bb1dcf1f'
REGISTRY='ghcr.io/likesjx/jaredlikes-com@sha256:3c2dd00a5133f79bcf82230d64b543d037aac934008e1cadda1729cc20b76107'
UNITS=('philotic-hotel.service','percival-personal-mcp.service','percival-personal-vault.socket')
PROPS=('User','Group','Requires','Wants','After','TriggeredBy','FragmentPath','DropInPaths','ExecStart','ExecStartPre','ExecStartPost','ExecStop','ExecStopPost','ExecReload','KillMode','KillSignal','TimeoutStopUSec','Restart')
ENV={'PATH':'/usr/bin:/bin','LANG':'C','HOME':'/nonexistent'}
DOCKER=['/usr/bin/docker','--config','/nonexistent/muninn-introspection-config','--host','unix:///var/run/docker.sock']
def sha(raw):return hashlib.sha256(raw).hexdigest()
def parent_allowed(path,s,owners):return s.st_uid in owners and(not s.st_mode&0o022 or str(path)=='/tmp'and s.st_uid==0 and stat.S_IMODE(s.st_mode)==0o1777)
def sync_dir(path):
 fd=os.open(path,os.O_RDONLY|os.O_DIRECTORY)
 try:os.fsync(fd)
 finally:os.close(fd)
def exact(value,keys):require(type(value)is dict and set(value)==set(keys.split()))
def hex64(v):return type(v)is str and re.fullmatch('[a-f0-9]{64}',v)
def validate(m):
 exact(m,'schema host observed_at policy_sha256 verifier_sha256 units source_files compose access_boundary')
 require(type(m['schema'])is int and m['schema']==1 and m['host']=='srv875272'and type(m['observed_at'])is int)
 for k in('policy_sha256','verifier_sha256'):require(hex64(m[k]))
 require(m['access_boundary']=='issuer-disabled-gateway-stopped-before-publication')
 require(set(m['units'])==set(UNITS)and all(hex64(v)for v in m['units'].values()))
 require(1<=len(m['source_files'])<=32)
 for p,h in m['source_files'].items():require(re.fullmatch(r'/(?:etc|usr/lib|run)/systemd/system/(?:(?:philotic-hotel\.service|percival-personal-mcp\.service|percival-personal-vault\.socket)(?:\.d/[A-Za-z0-9_.-]{1,128}\.conf)?|(?:service|socket|philotic-\.service|percival-\.service|percival-personal-\.service)\.d/[A-Za-z0-9_.-]{1,128}\.conf)',p)and hex64(h))
 c=m['compose'];exact(c,'project working_directory files env_sha256')
 require(re.fullmatch('[a-z0-9][a-z0-9_-]{0,62}',c['project'])and c['working_directory']in('/home/deploy/jaredlikes-com','/home/deploy/jaredlikes-com-production'))
 require(c['env_sha256']is None or hex64(c['env_sha256']));require(1<=len(c['files'])<=8)
 for n,f in enumerate(c['files']):
  exact(f,'path sha256');require(hex64(f['sha256'])and(f['path']==c['working_directory']+'/docker-compose.yml'if n==0 else re.fullmatch(r'/tmp/percival-identity-build-[A-Za-z0-9_-]+/production-[A-Za-z0-9_.-]+\.override\.yml',f['path'])))
 return m
class Files:
 def __init__(self,root='',owners=None):self.root=root;self.owners=owners
 def path(self,path):return Path(self.root+path)
 def read(self,path,limit=1048576):
  p=self.path(path);owners=self.owners if self.owners is not None else{0,pwd.getpwnam('deploy').pw_uid};d=os.open('/',os.O_RDONLY|os.O_DIRECTORY)
  try:
   for n,part in enumerate(p.parts[1:]):
    last=n==len(p.parts)-2;fd=os.open(part,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK|(0 if last else os.O_DIRECTORY),dir_fd=d);os.close(d);d=fd;s=os.fstat(d);require((s.st_uid in owners and not s.st_mode&0o022 and stat.S_ISREG(s.st_mode))if last else stat.S_ISDIR(s.st_mode)and parent_allowed(Path('/').joinpath(*p.parts[1:n+2]),s,owners))
   a=os.fstat(d);require(a.st_nlink==1 and a.st_size<=limit);raw=b''
   while len(raw)<=limit:
    value=os.read(d,min(65536,limit+1-len(raw)))
    if not value:break
    raw+=value
   b=os.fstat(d);require((a.st_dev,a.st_ino,a.st_size,a.st_mtime_ns,a.st_ctime_ns)==(b.st_dev,b.st_ino,b.st_size,b.st_mtime_ns,b.st_ctime_ns)and len(raw)==a.st_size)
   return raw,{'dev':a.st_dev,'ino':a.st_ino,'mode':stat.S_IMODE(a.st_mode),'uid':a.st_uid,'gid':a.st_gid}
  finally:os.close(d)
 def replace(self,path,before,after):
  raw,identity=self.read(path);require(raw==before);p=self.path(path);fd,tmp=tempfile.mkstemp(prefix='.introspection-owned-',dir=p.parent)
  try:
   os.fchmod(fd,identity['mode']);os.fchown(fd,identity['uid'],identity['gid']);offset=0
   while offset<len(after):n=os.write(fd,after[offset:]);require(n>0);offset+=n
   os.fsync(fd);os.close(fd);fd=None;current,now=self.read(path);require(current==before and now==identity);os.replace(tmp,p)
   directory=os.open(p.parent,os.O_RDONLY|os.O_DIRECTORY)
   try:os.fsync(directory)
   finally:os.close(directory)
  finally:
   if fd is not None:os.close(fd)
   if os.path.lexists(tmp):os.unlink(tmp)
 def create(self,path,raw,allocated=None):
  p=self.path(path);fd=os.open(p,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
  try:
   if allocated:allocated(os.fstat(fd))
   offset=0
   while offset<len(raw):n=os.write(fd,raw[offset:]);require(n>0);offset+=n
   os.fsync(fd)
  finally:os.close(fd)
  sync_dir(p.parent)
 def remove(self,path,expected):
  raw,identity=self.read(path);require(raw==expected);now=self.path(path).lstat();require((now.st_dev,now.st_ino)==(identity['dev'],identity['ino']));self.path(path).unlink();sync_dir(self.path(path).parent)
def listener_inodes(table,path):
 require(len(table)<=1048576)
 values=[v[6]for line in table.splitlines()[1:]if len(v:=line.split())==8 and v[3:6]==[b'00010000',b'0001',b'01']and v[7]==path.encode()];require(len(values)==1);return values

def command(argv,seconds,capture=False):
 # Reuses the reviewed streaming/process-group pattern from adoption_command.
 p=subprocess.Popen(argv,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE if capture else subprocess.DEVNULL,stderr=subprocess.DEVNULL,start_new_session=True,env=ENV,cwd='/');out=[];size=0;end=time.monotonic()+seconds
 try:
  if capture:
   os.set_blocking(p.stdout.fileno(),False)
   with selectors.DefaultSelector()as sel:
    sel.register(p.stdout,selectors.EVENT_READ)
    while True:
     require(time.monotonic()<end)
     if not sel.select(min(.1,max(.001,end-time.monotonic()))):continue
     value=os.read(p.stdout.fileno(),4096)
     if not value:break
     size+=len(value);require(size<=32768);out.append(value)
  code=p.wait(timeout=max(.001,end-time.monotonic()));require(code==0);return b''.join(out).decode()if capture else None
 finally:
  try:os.killpg(p.pid,signal.SIGKILL)
  except ProcessLookupError:pass
  p.wait()
  if p.stdout is not None:p.stdout.close()
class Owner:
 def __init__(self,m,files=None):self.m=json.loads(json.dumps(validate(m)));self.fs=files or Files()
 def bind(self,journal):self.journal=journal
 def allocated(self,data,path,info):data.setdefault('pair_created',{})[path]={'dev':info.st_dev,'ino':info.st_ino};self.journal.save()
 def query(self,argv):return command(argv,10,True)
 def run(self,argv,seconds):return command(['/usr/bin/timeout','--signal=TERM','--kill-after=5',str(seconds),*argv],seconds+8)
 def unit(self,name):
  raw=self.query(['/usr/bin/systemctl','show',name,'--no-pager','--property='+','.join(PROPS)]);v={k:None for k in PROPS};seen=set()
  for line in raw.splitlines():k,sep,value=line.partition('=');require(sep and k in PROPS and k not in seen);seen.add(k);v[k]=value
  require(v['FragmentPath']is not None and v['DropInPaths']is not None)
  for k in PROPS:
   if k.startswith('Exec')and v[k]is not None:
    values=re.findall(r'argv\[\]=(.*?) ;',v[k]);require(not v[k]or values);v[k]=values
  return v
 def identity(self):
  require(os.geteuid()==0 and socket.gethostname()=='srv875272')
  raw,_=self.fs.read(HOTEL,268435456);require(sha(raw)==B7)
  require(self.unit('philotic-hotel.service')['User']=='philotic'and self.unit('philotic-hotel.service')['Group']=='philotic')
  pid=self.query(['/usr/bin/systemctl','show','philotic-hotel.service','--property=MainPID','--value']).strip();require(re.fullmatch('[1-9][0-9]{0,9}',pid));exe=Path('/proc/'+pid+'/exe').stat();on_disk=self.fs.path(HOTEL).stat();require((exe.st_dev,exe.st_ino)==(on_disk.st_dev,on_disk.st_ino))
 def issuer_env(self):
  fmt='{{range .Config.Env}}{{if or (eq (index (split . "=") 0) "PERSONAL_OAUTH_ENABLED") (eq (index (split . "=") 0) "PERSONAL_OAUTH_INTROSPECTION_CLIENT") (eq (index (split . "=") 0) "PERSONAL_OAUTH_INTROSPECTION_VERIFIER")}}{{json .}}{{println}}{{end}}{{end}}'
  out={}
  for line in self.query(DOCKER+['inspect','--format',fmt,'jaredlikes-com-production']).splitlines():
   k,sep,value=json.loads(line).partition('=');require(sep and k not in out);out[k]=value
  require(out.get('PERSONAL_OAUTH_ENABLED')=='false');return out
 def guard(self,data,recover=False):
  self.identity()
  for unit in UNITS:
   # systemd v255 renders no queued job as an empty property. --all and the
   # property name distinguish an explicit idle result from missing output.
   # https://github.com/systemd/systemd/blob/v255/src/systemctl/systemctl-show.c#L1023
   require(self.query(['/usr/bin/systemctl','show',unit,'--property=Job','--all']).strip()=='Job=')
  require(not self.fs.path('/nonexistent/muninn-introspection-config').exists())
  if not recover:require(0<=time.time()*1000-self.m['observed_at']<1800000)
  listed=set()
  for name in UNITS:
   value=self.unit(name);require(digest(value)==self.m['units'][name]);listed.update([value['FragmentPath'],*value['DropInPaths'].split()])
  require(listed==set(self.m['source_files']))
  for base in('/etc/systemd/system','/run/systemd/system','/usr/lib/systemd/system'):
   for name in(*[u+'.d'for u in UNITS],'service.d','socket.d','philotic-.service.d','percival-.service.d','percival-personal-.service.d'):
    directory=self.fs.path(base+'/'+name)
    if directory.exists():
     require(directory.is_dir()and not directory.is_symlink())
     for item in directory.iterdir():
      if item.name.endswith('.conf'):require(base+'/'+name+'/'+item.name in listed)

  for path,h in self.m['source_files'].items():require(sha(self.fs.read(path)[0])==h)
  if any(data.get('steps',{}).get(key)=='done'for key in('adopt_stop_gateway','recover_stop_gateway')):require(self.query(['/usr/bin/systemctl','show','percival-personal-mcp.service','--property=ActiveState','--value']).strip()=='inactive')
  c=self.m['compose']
  for f in c['files']:
   raw=self.fs.read(f['path'])[0];require(sha(raw)==f['sha256']);baseline_compose(raw)
  env=c['working_directory']+'/.env'
  if c['env_sha256']is None:require(not self.fs.path(env).exists())
  else:require(sha(self.fs.read(env)[0])==c['env_sha256'])
  require(json.loads(self.query(DOCKER+['inspect','--format','{{json .Image}}','jaredlikes-com-production']))==IMAGE);self.issuer_env()
  if 'pair_capture'in data:
   require(data['pair_capture']['owner_manifest_sha256']==digest(self.m))
   original=self.original(data);target=self.target(data)
   for path in(POLICY,VERIFIER):require(self.fs.read(path)[0]in(original[path],target[path]))
  else:
   require(sha(self.fs.read(POLICY)[0])==self.m['policy_sha256']and sha(self.fs.read(VERIFIER)[0])==self.m['verifier_sha256'])
 def capture(self):
  values={}
  for path in(POLICY,VERIFIER):
   raw,meta=self.fs.read(path);values[path]={'bytes':base64.b64encode(raw).decode(),'identity':meta}
  require(not os.path.lexists(self.fs.path(self.m['compose']['working_directory']+'/muninn-introspection-only.override.yml'))and not os.path.lexists(self.fs.path(self.m['compose']['working_directory']+'/muninn-introspection-only.rollback.yml')))
  require(re.fullmatch(b'PERSONAL_OAUTH_INTROSPECTION_VERIFIER=[a-f0-9]{64}\n?',base64.b64decode(values[VERIFIER]['bytes'])))
  return {'owner_manifest_sha256':digest(self.m),'files':values,'issuer_env':self.issuer_env()}
 def original(self,data):return {p:base64.b64decode(v['bytes'],validate=True)for p,v in data['pair_capture']['files'].items()}
 def target(self,data):
  before=self.original(data);raw=before[POLICY];pairs=[]
  def unique(items):
   out={}
   for k,v in items:require(k not in out);out[k]=v
   return out
  value=json.loads(raw,object_pairs_hook=unique);exact(value,'caller_uid hotel_uid secret_ref introspection_secret_ref');require(value['caller_uid']==1003 and value['hotel_uid']==999 and re.fullmatch('secret://hotel/default/percival-muninn-observe/[A-Za-z0-9_-]+',value['secret_ref']))
  ref=data['introspection_secret_ref'];require(re.fullmatch(re.escape(PREFIX)+'[a-f0-9]{32}',ref));pattern=rb'("introspection_secret_ref"\s*:\s*)"(?:[^"\\]|\\.)*"';require(len(re.findall(pattern,raw))==1)
  after=re.sub(pattern,lambda m:m[1]+json.dumps(ref).encode(),raw);value['introspection_secret_ref']=ref;require(json.loads(after)==value)
  require(re.fullmatch('[a-f0-9]{64}',data['introspection_verifier']));return {POLICY:after,VERIFIER:('PERSONAL_OAUTH_INTROSPECTION_VERIFIER='+data['introspection_verifier']+'\n').encode()}
 def compose(self,override):
  c=self.m['compose'];args=DOCKER+['compose','--env-file',c['working_directory']+'/.env'if c['env_sha256']is not None else'/dev/null','--project-name',c['project'],'--project-directory',c['working_directory']]
  for f in c['files']:args+=['-f',f['path']]
  if override:args+=['-f',override]
  return args
 def override(self,data):return ('services:\n  app:\n    image: '+REGISTRY+'\n    environment:\n      PERSONAL_OAUTH_ENABLED: "false"\n      PERSONAL_OAUTH_INTROSPECTION_CLIENT: '+json.dumps(INTROSPECTION)+'\n      PERSONAL_OAUTH_INTROSPECTION_VERIFIER: '+json.dumps(data['introspection_verifier'])+'\n').encode()
 def action(self,step,data):
  original=self.original(data);target=self.target(data);override=self.m['compose']['working_directory']+'/muninn-introspection-only.override.yml'
  if step=='stop_gateway':self.run(['/usr/bin/systemctl','stop','percival-personal-mcp.service'],95)
  elif step in('write_policy','restore_policy','write_verifier','restore_verifier'):
   p=POLICY if step.endswith('policy')else VERIFIER;before,after=(target[p],original[p])if step.startswith('restore')else(original[p],target[p]);current=self.fs.read(p)[0]
   if current!=after:self.fs.replace(p,before,after)
  elif step=='restart_hotel_same_binary':
   self.run(['/usr/bin/systemctl','restart','philotic-hotel.service'],95);self.identity()
  elif step in('adopt_issuer_verifier','restore_issuer_verifier'):
   path=override if step=='adopt_issuer_verifier'else None
   if path:self.fs.create(path,self.override(data),lambda st:self.allocated(data,path,st))
   else:
    path=self.m['compose']['working_directory']+'/muninn-introspection-only.rollback.yml';self.fs.create(path,('services:\n  app:\n    image: '+REGISTRY+'\n').encode(),lambda st:self.allocated(data,path,st))
   self.run(self.compose(path)+['config','--quiet'],20)
   self.run(self.compose(path)+['up','-d','--no-deps','--no-build','--force-recreate','--pull','never','--wait','--wait-timeout','90','app'],100)
   env=self.issuer_env()
   if step=='adopt_issuer_verifier':require(env.get('PERSONAL_OAUTH_INTROSPECTION_CLIENT')==INTROSPECTION and env.get('PERSONAL_OAUTH_INTROSPECTION_VERIFIER')==data['introspection_verifier'])
   else:
    require(env==data['pair_capture']['issuer_env'])
    if self.fs.path(override).exists():
     raw,meta=self.fs.read(override);owned=data.get('pair_created',{}).get(override);require(owned=={'dev':meta['dev'],'ino':meta['ino']}and self.override(data).startswith(raw));self.fs.remove(override,raw)
  elif step=='verify_pair':require(self.getter_verifier()==data['introspection_verifier']);require(self.issuer_env().get('PERSONAL_OAUTH_INTROSPECTION_VERIFIER')==data['introspection_verifier']);require(self.query(['/usr/bin/systemctl','show','percival-personal-mcp.service','--property=ActiveState','--value']).strip()=='inactive') # actual getter check below
  elif step=='verify_closed':require(self.query(['/usr/bin/systemctl','show','percival-personal-mcp.service','--property=ActiveState','--value']).strip()=='inactive');self.issuer_env()
  else:require(False)

 def getter_verifier(self):
  import ctypes,select,struct
  path='/run/percival-personal-vault-broker.sock';s=self.fs.path(path).lstat();require(stat.S_ISSOCK(s.st_mode)and s.st_uid==1003 and s.st_gid==1003 and stat.S_IMODE(s.st_mode)==0o600)
  require(Path('/proc/sys/fs/suid_dumpable').read_text().strip()in('0','2'))
  before=self.peer();r,w=os.pipe();pid=os.fork()
  if pid==0:
   os.close(r)
   try:
    import resource
    resource.setrlimit(resource.RLIMIT_CORE,(0,0));lib=ctypes.CDLL(None);require(lib.prctl(4,0,0,0,0)==0);require(lib.prctl(8,0,0,0,0)==0);require(lib.prctl(38,1,0,0,0)==0)
    os.setgroups([]);os.setresgid(1003,1003,1003);os.setresuid(1003,1003,1003);require(lib.prctl(4,0,0,0,0)==0 and lib.prctl(3,0,0,0,0)==0)
    def deadline(*_):raise TimeoutError()
    signal.signal(signal.SIGALRM,deadline);signal.alarm(4)
    status=Path('/proc/self/status').read_text()
    for field in('CapInh:','CapPrm:','CapEff:','CapAmb:'):require(int(next(v.split()[1]for v in status.splitlines()if v.startswith(field)),16)==0)
    with socket.socket(socket.AF_UNIX)as sock:
     sock.settimeout(3);sock.connect(str(self.fs.path(path)));body=b'{"operation":"get_percival_introspection_credential"}';sock.sendall(struct.pack('!I',len(body))+body)
     def read(n):
      out=b''
      while len(out)<n:chunk=sock.recv(n-len(out));require(bool(chunk));out+=chunk
      return out
     n=struct.unpack('!I',read(4))[0];require(0<n<=8192);value=json.loads(read(n));require(type(value)is dict and set(value)=={'credential'}and type(value['credential'])is str and re.fullmatch('[A-Za-z0-9_-]{43,128}',value['credential']))
     os.write(w,verifier(value['credential']).encode())
    os._exit(0)
   except BaseException:os._exit(1)
  os.close(w)
  try:
   require(select.select([r],[],[],6)[0]);raw=os.read(r,65);require(len(raw)==64 and re.fullmatch(b'[a-f0-9]{64}',raw));_,status=os.waitpid(pid,0);pid=None;require(status==0 and self.peer()==before);return raw.decode()
  finally:
   os.close(r)
   if pid is not None:
    try:os.kill(pid,signal.SIGKILL)
    except ProcessLookupError:pass
    os.waitpid(pid,0)

 def peer(self):
  self.identity();pid=self.query(['/usr/bin/systemctl','show','philotic-hotel.service','--property=MainPID','--value']).strip();require(re.fullmatch('[1-9][0-9]{0,9}',pid));proc=Path('/proc')/pid
  require(os.readlink(proc/'ns/net')==os.readlink('/proc/self/ns/net'))
  status=(proc/'status').read_bytes();require(len(status)<=65536 and[x.split()[1:]for x in status.splitlines()if x.startswith(b'Uid:')]==[[b'999']*4])
  path='/run/percival-personal-vault-broker.sock';info=self.fs.path(path).lstat();table=Path('/proc/net/unix').read_bytes();require(len(table)<=1048576)
  matches=listener_inodes(table,str(self.fs.path(path)))
  with os.scandir(proc/'fd')as entries:
   for count,entry in enumerate(entries):
    if count>=8192:break
    try:
     if os.readlink(entry.path)=='socket:['+matches[0].decode()+']':return(pid,info.st_dev,info.st_ino,matches[0])
    except FileNotFoundError:continue
  require(False)
