"""Concrete localhost Muninn admin and protected hotel-vault subprocess adapters."""
import select,shlex,time
import base64, ctypes, hashlib, http.cookiejar, json, os, re, resource, signal, stat, subprocess, sys, urllib.error, urllib.request
from pathlib import Path
from provision import require,Refused,ROLE,GUEST,INTROSPECTION
BASE=Path('/opt/percival-personal-mcp/provisioning')
HELPER=BASE/'persistent-broker-helper'
HANDOFF=BASE/'private_key_handoff.py'
EXISTING_KEY_SOURCE='/opt/philotic/etc/secrets.env'
EXISTING_KEY_FIELD='PHILOTIC_VAULT_MASTER_KEY'

def selected_existing_key(raw):
    """Deliberately narrow, single-line EnvironmentFile subset; never shell eval.

    Refuse escapes/multiline syntax throughout the file so a key-looking line
    inside another variable's continued/quoted value cannot become an assignment.
    Duplicate target assignments refuse rather than adopting systemd's last-wins.
    """
    try:
        require(type(raw)is bytes and 0<len(raw)<=65536)
        text=raw.decode('utf-8');require(not any(c in text for c in ('\x00','\ufeff')))
        selected=[]
        for line in text.split('\n'):
            line=line.strip(' \t\r')
            require('\\'not in line and not any(ord(c)<32 and c!='\t'for c in line))
            if not line or line.startswith(('#',';')):continue
            name,sep,value=line.partition('=')
            if not sep:
                require(not line.startswith(EXISTING_KEY_FIELD));continue
            name=name.strip(' \t');value=value.strip(' \t\r')
            require(re.fullmatch('[A-Za-z_][A-Za-z0-9_]*',name)is not None)
            if value.startswith(('"',"'")):
                require(len(value)>=2 and value[-1]==value[0]and value[0]not in value[1:-1]);value=value[1:-1]
            if name==EXISTING_KEY_FIELD:
                require(re.fullmatch('[A-Za-z0-9+/]{43}=',value)is not None)
                key=value.encode('ascii');require(base64.b64encode(base64.b64decode(key,validate=True))==key);selected.append(key)
        require(len(selected)==1);return selected[0]
    except Exception:pass
    # Outside the exception handler: no raw decoder/parse exception is chained.
    raise Refused('existing_key_source_refused')

def check_existing_key_registration(host):
    """Only selected non-value systemd metadata, no Environment contents.
    v255 systemctl-show prints each EnvironmentFiles item as path(ignore_errors).
    Require the one recorded mandatory source and no final key removal.
    This binds next-start file selection, not a historical in-memory key value.
    """
    text=host.query(['/usr/bin/systemctl','show','philotic-hotel.service','--property=EnvironmentFiles,UnsetEnvironment,NeedDaemonReload','--all'])
    values={}
    for line in text.splitlines():
        key,sep,value=line.partition('=');require(sep and key in('EnvironmentFiles','UnsetEnvironment','NeedDaemonReload')and key not in values);values[key]=value
    require(set(values)=={'EnvironmentFiles','UnsetEnvironment','NeedDaemonReload'})
    require(values['EnvironmentFiles']==EXISTING_KEY_SOURCE+' (ignore_errors=no)'and values['NeedDaemonReload']=='no')
    require(all(value!=EXISTING_KEY_FIELD and not value.startswith(EXISTING_KEY_FIELD+'=')for value in shlex.split(values['UnsetEnvironment'])))

def existing_hotel_master_key():
    """Explicit fixed source only. No fallback, discovery, env injection or output.
    All file contents stay in the protected process; only the selected key leaves
    this function for the existing private handoff. No zeroization claim.
    """
    raw=None;key=None
    try:
        import pwd
        from introspection_owner import Files,Owner
        require(sys.flags.isolated and sys.platform=='linux'and os.geteuid()==0)
        protect_operator()
        require((pwd.getpwnam('philotic').pw_uid,pwd.getpwnam('philotic').pw_gid)==(999,987))
        # Reuse exact host/B7/process identity, not a guessed executable owner.
        host=Owner.__new__(Owner);host.fs=Files();host.identity();check_existing_key_registration(host)
        files=Files(owners={0,999,pwd.getpwnam('deploy').pw_uid})
        raw,identity=files.read(EXISTING_KEY_SOURCE,65536,private_identity=(999,987,0o600))
        require(identity['uid']==999 and identity['gid']==987 and identity['mode']==0o600)
        key=selected_existing_key(raw)
        require(files.read(EXISTING_KEY_SOURCE,65536,private_identity=(999,987,0o600))==(raw,identity));host.identity();check_existing_key_registration(host)
        return key
    except BaseException:pass
    finally:raw=None;key=None
    raise Refused('existing_key_source_refused')
class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self,*args,**kwargs):raise RuntimeError('redirect refused')
class API:
    def __init__(self,rest_port,ui_port):
        require(all(type(p) is int and 1024<=p<=65535 for p in (rest_port,ui_port)) and rest_port!=ui_port)
        self.rest_port=rest_port;self.ui_port=ui_port
        self.cookies=http.cookiejar.CookieJar()
        self.opener=urllib.request.build_opener(urllib.request.ProxyHandler({}),NoRedirect(),urllib.request.HTTPCookieProcessor(self.cookies))
    def login(self,username,password):
        try:
            status,_=self._wire('POST','/api/auth/login',{'username':username,'password':password},self.ui_port)
            require(status==200 and bool(list(self.cookies)))
        except BaseException:self.cookies.clear();raise
    def request(self,method,path,payload=None,admin=False):
        require(admin is True)
        require((method,path) in [('GET','/api/vaults'),('POST','/api/admin/keys')])
        if method=='POST':
            require(isinstance(payload,dict) and set(payload)=={'vault','mode','label','expires'} and payload['mode']=='observe')
        return self._wire(method,path,payload,self.rest_port)
    def _wire(self,method,path,payload,port):
        request=urllib.request.Request(f'http://127.0.0.1:{port}'+path,method=method,
            data=None if payload is None else json.dumps(payload).encode(),headers={'Content-Type':'application/json'})
        try:response=self.opener.open(request,timeout=10)
        except urllib.error.HTTPError as error:
            error.close();return error.code,None
        with response:
            raw=response.read(65537);require(len(raw)<=65536)
            return response.status,json.loads(raw) if raw else None
    def close_private_session(self):
        try:self._wire('POST','/api/auth/logout',{},self.ui_port)
        finally:self.cookies.clear()
def trusted(path):
    path=Path(path)
    for part in [path,*path.parents]:
        st=part.lstat();require(not stat.S_ISLNK(st.st_mode) and st.st_uid==0 and not st.st_mode & 0o022)
    require(path.is_file())
def protect_operator():
    require(sys.platform=='linux' and os.geteuid()==0)
    resource.setrlimit(resource.RLIMIT_CORE,(0,0))
    libc=ctypes.CDLL(None,use_errno=True)
    require(libc.prctl(4,0,0,0,0)==0 and libc.prctl(3,0,0,0,0)==0) # PR_SET/GET_DUMPABLE

def run_helper(request,key,expected_hash,*,timeout=35,allocation=None):
    """Exactly one helper process per store; no resident key-bearing broker.

    The launcher/helper share a process group. Any timeout kills and waits for
    that group before returning an uncertain effect; never repeat a create.
    """
    trusted(HELPER);trusted(HANDOFF)
    require(hashlib.sha256(HELPER.read_bytes()).hexdigest()==expected_hash)
    r,w=os.pipe();proc=None
    try:
        require(os.write(w,key+b'\n')==len(key)+1);os.close(w);w=None
        proc=subprocess.Popen(['/usr/bin/python3','-I',str(HANDOFF),'--approved-private-handoff','--key-fd',str(r)],
            stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,pass_fds=(r,),
            start_new_session=True,env={'PATH':'/usr/bin:/bin'})
        os.close(r);r=None
        try:
            if allocation is None:output,_=proc.communicate(json.dumps(request).encode(),timeout=timeout)
            else:
                deadline=time.monotonic()+timeout
                proc.stdin.write(json.dumps(request).encode()+b'\n');proc.stdin.flush();line=b''
                while not line.endswith(b'\n'):
                    require(len(line)<4096)
                    require(select.select([proc.stdout],[],[],max(0,deadline-time.monotonic()))[0]);part=os.read(proc.stdout.fileno(),1);require(bool(part));line+=part
                result=json.loads(line);require(set(result)=={'allocated_reference'});ref=result['allocated_reference']
                require(isinstance(ref,str)and re.fullmatch('secret://hotel/default/percival-issuer-introspection/[a-f0-9]{32}',ref))
                allocation(ref) # MUST persist durably before authorizing any DB write.
                proc.stdin.write(json.dumps({'commit':ref}).encode()+b'\n');proc.stdin.flush();proc.stdin.close();proc.stdin=None
                output,_=proc.communicate(timeout=max(.001,deadline-time.monotonic()))
        except BaseException:
            os.killpg(proc.pid,signal.SIGKILL);proc.wait();raise
        require(proc.returncode==0 and 0<len(output)<=4096)
        result=json.loads(output);require(isinstance(result,dict) and(set(result)=={'reference'}if request['operation']!='reconcile_introspection'else set(result)=={'status'}and result['status']in('stored','absent','mismatch')))
        return result
    finally:
        if r is not None:os.close(r)
        if w is not None:os.close(w)
        if proc is not None and proc.poll() is None:os.killpg(proc.pid,signal.SIGKILL);proc.wait()
        if proc is not None:
            for stream in [proc.stdin,proc.stdout]:
                if stream is not None:stream.close()
class Broker:
    def __init__(self,key,expected_hash):
        require(isinstance(key,bytes) and len(key)==44 and base64.b64encode(base64.b64decode(key,validate=True))==key and len(base64.b64decode(key))==32)
        require(re.fullmatch('[0-9a-f]{64}',expected_hash) is not None)
        self.key=key;self.expected_hash=expected_hash
    def store_exact_acl(self,*,kind,scope,secret,allowed_roles,allowed_guests,allocation=None):
        require(allowed_roles==[ROLE] and allowed_guests==[GUEST])
        require((kind=='percival-muninn-observe' and isinstance(scope,str) and re.fullmatch('[a-z0-9_-]{1,64}',scope)) or
                (kind=='percival-issuer-introspection' and scope==INTROSPECTION))
        require(isinstance(secret,str) and 32<=len(secret)<4096 and not re.search(r'[\x00-\x20\x7f]',secret))
        require(allocation is None or kind=='percival-issuer-introspection')
        return run_helper({'operation':'create_introspection'if allocation is not None else'create','kind':kind,'scope':scope,'secret':secret,
            'allowed_roles':allowed_roles,'allowed_guests':allowed_guests},self.key,self.expected_hash,**({}if allocation is None else{'allocation':allocation}))
    def reconcile_introspection(self,reference,verifier):
        require(isinstance(reference,str)and re.fullmatch('secret://hotel/default/percival-issuer-introspection/[a-f0-9]{32}',reference))
        require(isinstance(verifier,str)and re.fullmatch('[a-f0-9]{64}',verifier))
        return run_helper({'operation':'reconcile_introspection','kind':'percival-issuer-introspection','scope':INTROSPECTION,'reference':reference,'verifier':verifier,'allowed_roles':[ROLE],'allowed_guests':[GUEST]},self.key,self.expected_hash)
    def close(self):self.key=b''
