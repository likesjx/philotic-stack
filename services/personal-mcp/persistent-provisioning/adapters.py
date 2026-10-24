"""Concrete localhost Muninn admin and protected hotel-vault subprocess adapters."""
import select,time
import base64, ctypes, hashlib, http.cookiejar, json, os, re, resource, signal, stat, subprocess, sys, urllib.error, urllib.request
from pathlib import Path
from provision import require,ROLE,GUEST,INTROSPECTION
BASE=Path('/opt/percival-personal-mcp/provisioning')
HELPER=BASE/'persistent-broker-helper'
HANDOFF=BASE/'private_key_handoff.py'
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
