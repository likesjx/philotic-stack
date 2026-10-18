"""Private operator transaction; no network, credentials, or effects on import.

Reuse the existing API and encrypted-vault owners through injected adapters.
The command line is deliberately plan-only: it never collects or transmits secrets.
An operator-controlled runner calls execute with a private handoff AFTER confirming
its manifest digest. No automated runner is authorized by a manifest boolean.
"""
import base64, datetime, fcntl, hashlib, json, os, re, secrets, stat, sys
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
    require(set(m) == {'version','resource','subject','vault','include_future_records','clients','expires_at'})
    require(m['version']==1 and m['resource']==RESOURCE and m['subject']==SUBJECT)
    require(isinstance(m['vault'],str) and re.fullmatch(r'[A-Za-z0-9_-]{1,128}',m['vault']))
    require(m['include_future_records'] is True)
    require(isinstance(m['clients'],list) and len(m['clients'])==2 and
            sorted(m['clients'])==sorted(CLIENTS))
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
        'requires_private_operator_handoff':True,'requires_creation_and_storage_confirmation':True}

def execute(m, journal, confirm, private_handoff):
    """API.request and broker.store_exact_acl must be existing trusted owner adapters.

    private_handoff is an operator-owned callable that authenticates privately and
    returns (api, broker). It is never invoked until confirm approves this digest.
    The broker adapter must persist with the exact scope/ACL and verify readback,
    returning ONLY {'reference': ...}. It must not adopt policy or restart services.
    No production CLI adapter is installed by this module.
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

def main():
    # No apply subcommand, credential argv/env parsing, SSH, or secret collection.
    m=json.loads(Path(sys.argv[1]).read_text())
    try:print(json.dumps(plan(m),sort_keys=True))
    except Exception:print(json.dumps({'outcome':'operator_scope_or_expiry_required','live_changes':False}));return 1
    return 0
if __name__=='__main__':
    try:sys.exit(main())
    except Exception:print('{"outcome":"invalid_manifest","live_changes":false}');sys.exit(1)
