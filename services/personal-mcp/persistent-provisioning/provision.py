"""Private operator transaction; no network, credentials, or effects on import.

Reuse the existing API and encrypted-vault owners through injected adapters.
The apply command requires an interactive protected root terminal and exact confirmation.
An operator-controlled runner calls execute with a private handoff AFTER confirming
its manifest digest. No automated runner is authorized by a manifest boolean.
"""
import sys
if __name__=='__main__' and not sys.flags.isolated:
    sys.stderr.write('isolated Python required; no private input accepted\n');sys.exit(1)
import base64, datetime, fcntl, hashlib, json, os, re, secrets, stat
from pathlib import Path
CLIENTS = ['claude_muninn_recall_v1', 'chatgpt_muninn_recall_v1']
RESOURCE = 'https://mcp.jaredlikes.com/personal/mcp'
SUBJECT = '15393f50-045b-4227-90b0-0cf4264f0b3d'
INTROSPECTION = 'percival_gateway_persistent_recall_v1'
ROLE = 'percival-personal-recall'
GUEST = 'percival-personal-gateway'
class Refused(Exception): pass
def require(ok):
    if not ok: raise Refused('precondition refused')
class Journal:
    def __init__(self,path):
        self.path=Path(path)
        # Pin a private owner-controlled directory; all later operations are
        # relative to this fd even if a pathname ancestor is renamed/replaced.
        self.directory=os.open(self.path.parent,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
        st=os.fstat(self.directory)
        if not (st.st_uid==os.geteuid() and stat.S_IMODE(st.st_mode)==0o700):
            os.close(self.directory);raise Refused('private journal directory required')
        self.temporary=self.path.name+'.pending'
        self.fd=None
        try:
            self.fd=os.open(self.path.name+'.lock',os.O_RDWR|os.O_CREAT|os.O_NOFOLLOW|os.O_NONBLOCK,0o600,dir_fd=self.directory)
            st=os.fstat(self.fd)
            require(stat.S_ISREG(st.st_mode) and st.st_uid==os.geteuid() and st.st_mode & 0o077 == 0)
            fcntl.flock(self.fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
            try:
                readfd=os.open(self.path.name,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK,dir_fd=self.directory)
            except FileNotFoundError: raw=b''
            else:
                try:
                    st=os.fstat(readfd)
                    require(stat.S_ISREG(st.st_mode) and st.st_uid==os.geteuid() and st.st_mode & 0o077 == 0)
                    raw=os.read(readfd,65537);require(0<len(raw)<=65536)
                finally:os.close(readfd)
            self.data=json.loads(raw) if raw else {'phase':'new','key_ids':{},'steps':{}}
        except BaseException:
            if self.fd is not None:os.close(self.fd)
            os.close(self.directory)
            raise
    def save(self):
        # Stable .lock remains locked across replacement. A leftover .pending is
        # evidence of interrupted persistence; refuse rather than deleting it.
        blob=json.dumps(self.data).encode();require(len(blob)<=65536)
        fd=os.open(self.temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600,dir_fd=self.directory)
        try:
            offset=0
            while offset<len(blob):
                count=os.write(fd,blob[offset:]);require(count>0);offset+=count
            os.fsync(fd)
        finally:os.close(fd)
        os.replace(self.temporary,self.path.name,src_dir_fd=self.directory,dst_dir_fd=self.directory)
        os.fsync(self.directory)
    def close(self):
        os.close(self.fd);os.close(self.directory)
    def effect(self,name,call,record=None):
        require(name not in self.data['steps'])
        self.data['steps'][name]='pending'; self.save()
        value=call()
        if record is not None: record(value)
        self.data['steps'][name]='done'; self.save()
        return value

def validate(m, now=None):
    require(set(m) == {'version','resource','subject','vault','include_future_records','clients','expires_at','transport'})
    require(m['version']==1 and m['resource']==RESOURCE and m['subject']==SUBJECT)
    require(isinstance(m['vault'],str) and re.fullmatch(r'[a-z0-9_-]{1,64}',m['vault']))
    require(m['include_future_records'] is True)
    require(isinstance(m['clients'],list) and len(m['clients'])==2 and
            sorted(m['clients'])==sorted(CLIENTS))
    t=m['transport']
    require(isinstance(t,dict) and set(t)=={'rest_port','ui_port','helper_sha256'})
    require(all(type(t[k]) is int and 1024<=t[k]<=65535 for k in ['rest_port','ui_port']) and t['rest_port']!=t['ui_port'])
    require(isinstance(t['helper_sha256'],str) and re.fullmatch('[0-9a-f]{64}',t['helper_sha256']))
    require(isinstance(m['expires_at'],str) and re.fullmatch(r'\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,6})?(?:Z|[+-]\d{2}:\d{2})',m['expires_at']))
    end=datetime.datetime.fromisoformat(m['expires_at'].replace('Z','+00:00'))
    require(end.tzinfo is not None)
    now=now or datetime.datetime.now(datetime.timezone.utc)
    require(0 < (end-now).total_seconds() <= 31*86400)
    return end

def digest(m):
    return hashlib.sha256(json.dumps(m,sort_keys=True,separators=(',',':')).encode()).hexdigest()

def plan(m):
    validate(m)
    return {'manifest_sha256':digest(m),'live_changes':False,'key_request':{
        'vault':m['vault'],'mode':'observe','label':'persistent-two-client-recall-v1','expires':m['expires_at']},
        'introspection_client':INTROSPECTION,'role':ROLE,'guest':GUEST,
        'transport':m['transport'],'requires_private_operator_handoff':True,'requires_creation_and_storage_confirmation':True}

def execute(m, journal, confirm, private_handoff):
    """API.request and broker.store_exact_acl must be existing trusted owner adapters.

    private_handoff is an operator-owned callable that authenticates privately and
    returns (api, broker). It is never invoked until confirm approves this digest.
    The broker adapter must persist with the exact scope/ACL and verify readback,
    returning ONLY {'reference': ...}. It must not adopt policy or restart services.
    CLI apply supplies the concrete localhost API and protected hotel helper.
    """
    m=json.loads(json.dumps(m))
    end=validate(m); bound=digest(m)
    require(journal.data['phase']=='new' and not journal.data['steps'])
    require(confirm(bound) == 'CREATE_AND_STORE '+bound)
    # Bind recovery metadata durably before any private authentication/transmission.
    journal.data.update(phase='private_handoff_pending',manifest_sha256=bound,vault=m['vault'],expires_at=m['expires_at'])
    journal.save()
    api=broker=None
    try:
        api,broker=private_handoff()
        # Only vault metadata: do not read real records, change visibility, or seed.
        status,names=api.request('GET','/api/vaults',admin=True)
        require(status==200 and isinstance(names,list) and m['vault'] in names)
        def record_key(reply):
            status,body=reply;require(status==201 and isinstance(body,dict))
            k=body.get('key',{})
            require(isinstance(k.get('id'),str) and re.fullmatch(r'[A-Za-z0-9_-]+',k['id']))
            # Record a returned ID before subsequent contract checks for recovery.
            journal.data['key_ids']['observe']=k['id'];journal.save()
            require(k.get('vault')==m['vault'] and k.get('mode')=='observe')
            require(datetime.datetime.fromisoformat(k['expires_at'].replace('Z','+00:00'))==end)
            require(isinstance(body.get('token'),str) and body['token'].startswith('mk_') and 32<=len(body['token'])<=4096)
        _,body=journal.effect('create_observe',lambda:api.request('POST','/api/admin/keys',plan(m)['key_request'],admin=True),record_key)
        def store(kind,scope,secret,field):
            def record_ref(result):
                ref=result.get('reference')
                require(isinstance(ref,str) and re.fullmatch('secret://hotel/default/'+kind+'/[a-f0-9]{32}',ref))
                journal.data[field]=ref
            journal.effect('store_'+kind,lambda:broker.store_exact_acl(kind=kind,scope=scope,secret=secret,
                allowed_roles=[ROLE],allowed_guests=[GUEST]),record_ref)
        store('percival-muninn-observe',m['vault'],body.pop('token'),'secret_ref')
        secret=secrets.token_urlsafe(32)
        store('percival-issuer-introspection',INTROSPECTION,secret,'introspection_secret_ref')
        verifier=hashlib.sha256(('Basic '+base64.b64encode((INTROSPECTION+':'+secret).encode()).decode()).encode()).hexdigest()
        # Verifier remains in the private journal, never stdout. Applying it is a
        # separate issuer configuration action; this transaction does not activate.
        journal.data.update(phase='stored_not_activated',introspection_client=INTROSPECTION,introspection_verifier=verifier)
        journal.save()
        return {'outcome':'stored_not_activated','manifest_sha256':bound}
    except Exception:
        journal.data['phase']='recovery_required';journal.save()
        return {'outcome':'recovery_required','manifest_sha256':bound}
    finally:
        if api is not None:
            try:api.close_private_session()
            except Exception:pass
        if broker is not None and hasattr(broker,'close'):broker.close()

def main():
    import argparse,getpass,warnings
    p=argparse.ArgumentParser();p.add_argument('action',choices=['plan','apply','introspection-plan','introspection-apply','introspection-reconcile']);p.add_argument('--manifest',required=True);p.add_argument('--journal')
    a=p.parse_args()
    if a.action.startswith('introspection-'):
        if a.action!='introspection-plan':
            base=Path('/opt/percival-personal-mcp/provisioning')
            require(sys.flags.isolated and sys.platform=='linux'and os.geteuid()==0 and Path(__file__).absolute()==base/'provision.py')
            for file in ('provision.py','introspection.py','adapters.py','private_key_handoff.py','persistent-broker-helper'):
                path=base/file
                for part in [path,*path.parents]:
                    st=part.lstat();require(not stat.S_ISLNK(st.st_mode)and st.st_uid==0 and not st.st_mode&0o022)
                require(path.is_file())
        sys.path.insert(0,str(Path(__file__).resolve().parent))
        from introspection import cli
        return cli(a)
    with open(a.manifest,'rb') as f:
        raw=f.read(65537);require(len(raw)<=65536);m=json.loads(raw)
    prepared=plan(m)
    if a.action=='plan':print(json.dumps(prepared,sort_keys=True));return 0
    # Only isolated stdlib is loaded until fixed installation provenance and
    # process protection have been checked. No local module import before this.
    import ctypes,resource
    BASE=Path('/opt/percival-personal-mcp/provisioning');HELPER=BASE/'persistent-broker-helper';HANDOFF=BASE/'private_key_handoff.py'
    require(a.journal is not None and sys.stdin.isatty() and sys.stdout.isatty())
    require(sys.platform=='linux' and os.geteuid()==0 and sys.flags.isolated)
    require(Path(__file__).absolute()==BASE/'provision.py')
    for path in [BASE/'provision.py',BASE/'adapters.py',HELPER,HANDOFF]:
        for part in [path,*path.parents]:
            st=part.lstat();require(not stat.S_ISLNK(st.st_mode) and st.st_uid==0 and not st.st_mode&0o022)
        require(path.is_file())
    require(hashlib.sha256(HELPER.read_bytes()).hexdigest()==m['transport']['helper_sha256'])
    resource.setrlimit(resource.RLIMIT_CORE,(0,0))
    libc=ctypes.CDLL(None,use_errno=True)
    require(libc.prctl(4,0,0,0,0)==0 and libc.prctl(3,0,0,0,0)==0)
    sys.path.insert(0,str(BASE))
    from adapters import API,Broker
    warnings.simplefilter('error',getpass.GetPassWarning)
    print(json.dumps(prepared,indent=2,sort_keys=True))
    def confirm(bound):
        return input('Create and store these persistent credentials? Type CREATE_AND_STORE '+bound+': ')
    def handoff():
        # Operator enters existing secrets privately, only after digest confirmation.
        username=getpass.getpass('Existing Muninn admin username (hidden): ')
        password=getpass.getpass('Existing Muninn admin password (hidden): ')
        key=getpass.getpass('Existing hotel master key (base64, hidden): ').encode('ascii')
        broker=Broker(key,m['transport']['helper_sha256'])
        api=API(m['transport']['rest_port'],m['transport']['ui_port'])
        try:api.login(username,password);return api,broker
        except BaseException:broker.close();api.cookies.clear();raise
    journal=Journal(a.journal)
    try:
        outcome=execute(m,journal,confirm,handoff)
        print(json.dumps(outcome,sort_keys=True));return 0 if outcome['outcome']=='stored_not_activated' else 1
    finally:journal.close()
if __name__=='__main__':
    try:sys.exit(main())
    except (Exception,KeyboardInterrupt):print('{"outcome":"stopped_reconcile_private_journal","live_activation":false}',file=sys.stderr);sys.exit(1)
