"""Full real aiua binary, ephemeral encrypted SQLite, numeric UIDs and fake supervisor."""
import base64, ctypes, json, os, pathlib, shutil, signal, socket, sqlite3, stat, subprocess, sys, time
assert os.geteuid() == 0 and sys.platform == 'linux'
assert pathlib.Path('/.dockerenv').exists()
assert os.environ.get('PERCIVAL_ISOLATED_LINUX_FIXTURE') == '1'
BIN='/hotel-target/debug/aiua'
FILES=pathlib.Path(os.environ.get('PERCIVAL_BOOTSTRAP_FIXTURES', '/integrated/hotel/services/personal-mcp/scoped-vault-tests')).resolve()
assert FILES == pathlib.Path(__file__).resolve().parent
BASE=pathlib.Path('/run/percival-full-hotel-fixture')
POLICY=pathlib.Path('/etc/percival-personal-mcp/scoped-vault-policy.json')
SOCKET=pathlib.Path('/run/percival-personal-vault-broker.sock')
HOTEL='percival-bootstrap-fixture'
REFERENCE='secret://hotel/default/percival-muninn-observe/synthetic-bootstrap'
INTROSPECTION_REFERENCE='secret://hotel/default/percival-issuer-introspection/synthetic-bootstrap'
KEY=bytes([7]*32) # Public synthetic fixture key only, never a production credential.
ENV={'PATH':'/usr/bin:/bin','PERCIVAL_ISOLATED_LINUX_FIXTURE':'1',
     'PHILOTIC_VAULT_MASTER_KEY':base64.b64encode(KEY).decode(),
     'PHILOTIC_DISABLE_GUEST_SUPERVISOR':'1','PHILOTIC_KEYCHAIN_ENABLED':'0',
     'PHILOTIC_SCOPED_VAULT_ENABLED':'0'}
servers=[]; parent_socket=None

def drop(uid):
    def apply(): os.setgroups([]); os.setgid(uid); os.setuid(uid)
    return apply

def encrypt_synthetic(plain=b'mk_synthetic', nonce=bytes([3]*12)):
    crypto=ctypes.CDLL('libcrypto.so.3'); ptr=ctypes.c_void_p
    crypto.EVP_CIPHER_CTX_new.restype=ptr; crypto.EVP_aes_256_gcm.restype=ptr
    crypto.EVP_EncryptInit_ex.argtypes=[ptr,ptr,ptr,ptr,ptr]
    crypto.EVP_EncryptUpdate.argtypes=[ptr,ptr,ptr,ptr,ctypes.c_int]
    crypto.EVP_EncryptFinal_ex.argtypes=[ptr,ptr,ptr]
    crypto.EVP_CIPHER_CTX_ctrl.argtypes=[ptr,ctypes.c_int,ctypes.c_int,ptr]
    crypto.EVP_CIPHER_CTX_free.argtypes=[ptr]
    ctx=crypto.EVP_CIPHER_CTX_new()
    out=ctypes.create_string_buffer(256); size=ctypes.c_int(); final=ctypes.c_int(); tag=ctypes.create_string_buffer(16)
    try:
        assert crypto.EVP_EncryptInit_ex(ctx,crypto.EVP_aes_256_gcm(),None,KEY,nonce)==1
        assert crypto.EVP_EncryptUpdate(ctx,out,ctypes.byref(size),plain,len(plain))==1
        assert crypto.EVP_EncryptFinal_ex(ctx,ctypes.byref(out,size.value),ctypes.byref(final))==1
        assert crypto.EVP_CIPHER_CTX_ctrl(ctx,0x10,16,tag)==1
        return base64.b64encode(out.raw[:size.value+final.value]+tag.raw).decode(),base64.b64encode(nonce).decode()
    finally: crypto.EVP_CIPHER_CTX_free(ctx)

def seed(name):
    work=BASE/name; work.mkdir(mode=0o700); os.chown(work,999,999)
    config=work/'empty.json'; config.write_text('{}')
    env=dict(ENV,PHILOTIC_LOG_DIR=str(work/'logs'))
    loaded=subprocess.run([BIN,'load','--file',str(config),'--hotel',HOTEL],cwd=work,env=env,
                          preexec_fn=drop(999),stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=15)
    assert loaded.returncode==0, 'synthetic load failed: '+loaded.stderr.decode()[:400]
    ciphertext,nonce=encrypt_synthetic(); marker=work/'guest-probe.json'
    with sqlite3.connect(work/'aiua_context.db') as db:
        row=db.execute("SELECT node_key,data_json FROM graph_nodes WHERE kind='hotel'").fetchone()
        assert row
        hotel=json.loads(row[1]); hotel['ipc_socket_path']=str(work/'general.sock')
        db.execute('UPDATE graph_nodes SET data_json=? WHERE node_key=?',(json.dumps(hotel),row[0]))
        record={'secret_ref':REFERENCE,'secret_kind':'percival-muninn-observe','scope':'percival_connection_test',
                'allowed_roles':['percival-personal-recall'],'allowed_guests':['percival-personal-gateway'],
                'ciphertext_b64':ciphertext,'nonce_b64':nonce,'created_at':0,'updated_at':0}
        db.execute('INSERT INTO graph_nodes(node_key,kind,label,data_json) VALUES(?,?,?,?)',
                   ('secret:'+REFERENCE,'secret',REFERENCE,json.dumps(record)))
        second_ciphertext,second_nonce=encrypt_synthetic(b'i'*43,bytes([4]*12))
        second={**record,'secret_ref':INTROSPECTION_REFERENCE,'secret_kind':'percival-issuer-introspection',
                'ciphertext_b64':second_ciphertext,'nonce_b64':second_nonce}
        db.execute('INSERT INTO graph_nodes(node_key,kind,label,data_json) VALUES(?,?,?,?)',
                   ('secret:'+INTROSPECTION_REFERENCE,'secret',INTROSPECTION_REFERENCE,json.dumps(second)))
        guest={'hotel_name':HOTEL,'guest_id':'synthetic-fd-probe','role':'synthetic-probe',
               'config_json':json.dumps({'command':'/usr/bin/python3','args':[str(FILES/'bootstrap_guest_probe.py'),str(marker)]}),
               'is_active':True,'active_pid':None,'last_active_at':None}
        db.execute('INSERT INTO graph_nodes(node_key,kind,label,data_json) VALUES(?,?,?,?)',
                   ('guest:'+HOTEL+':synthetic-fd-probe','guest','synthetic-fd-probe',json.dumps(guest)))
    return work

def policy():
    POLICY.parent.mkdir(mode=0o755,exist_ok=True)
    POLICY.write_text(json.dumps({'caller_uid':1234,'hotel_uid':999,'secret_ref':REFERENCE,'introspection_secret_ref':INTROSPECTION_REFERENCE}))
    os.chmod(POLICY,0o644); os.chown(POLICY,0,0)

def start(work,enabled='1',metadata=True,extra=False,wrong_pid=False,test=False,smoke=False,uid=999):
    (work/'guest-probe.json').unlink(missing_ok=True)
    env=dict(ENV,PHILOTIC_LOG_DIR=str(work/'logs'))
    if enabled is None: env.pop('PHILOTIC_SCOPED_VAULT_ENABLED')
    else: env['PHILOTIC_SCOPED_VAULT_ENABLED']=enabled
    if smoke: env['PHILOTIC_SMOKE_MODE']='1'
    fd=parent_socket.fileno(); alias=os.dup(fd) if extra else None
    if metadata:
        env.update(LISTEN_PID='1' if wrong_pid else 'self',LISTEN_FDS=str(fd-2),
                   LISTEN_FDNAMES=':'.join(['unrelated']*(fd-3)+['percival-scoped-vault']))
    args=[BIN,'--hotel',HOTEL]
    if test: args+=['--test','scoped-vault-bootstrap']
    try:
        child=subprocess.Popen([sys.executable,str(FILES/'bootstrap_supervisor_exec.py'),*args],cwd=work,env=env,
              preexec_fn=drop(uid),pass_fds=(fd,)+((alias,) if alias is not None else ()),
              stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    finally:
        if alias is not None: os.close(alias)
    servers.append(child); return child

def client(kind='valid'):
    result=subprocess.run([sys.executable,str(FILES/'linux_acceptance.py'),'client','1234','0',kind,
                           json.dumps({'operation':'get_percival_introspection_credential' if kind=='introspection' else 'get_percival_credential'})],
                          env={'PATH':'/usr/bin:/bin'},stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=7)
    assert result.returncode==0, result.stderr.decode()[:300]
    return json.loads(result.stdout)

def ready(child,work,startup_test=False):
    deadline=time.monotonic()+12
    while time.monotonic()<deadline:
        if child.poll() is not None: raise AssertionError('hotel exited before ready: '+child.stderr.read().decode()[:400])
        marker=work/'guest-probe.json'
        if marker.exists():
            assert json.loads(marker.read_text())=={'scoped_fd_inherited':False,'uid':999}
            shutdown_ready=startup_test or any('Hotel bootstrap complete; shutdown handlers ready.' in log.read_text() for log in (work/'logs').glob('*') if log.is_file())
            if shutdown_ready and client().get('accepted') and client('introspection').get('accepted'): return
        time.sleep(.05)
    raise AssertionError('actual hotel/guest/credential readiness timed out')

def denied_start(name,**kwargs):
    work=seed(name); child=start(work,**kwargs)
    child.wait(timeout=10)
    assert child.returncode!=0, 'invalid activation unexpectedly admitted'
    assert not (work/'guest-probe.json').exists(),'invalid startup materialized guest'

try:
    BASE.mkdir(mode=0o755,exist_ok=True); policy(); SOCKET.unlink(missing_ok=True)
    parent_socket=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); parent_socket.bind(str(SOCKET)); parent_socket.listen(16)
    os.chown(SOCKET,1234,1234); os.chmod(SOCKET,0o600)
    work=seed('normal'); child=start(work); ready(child,work)
    began=time.monotonic(); child.send_signal(signal.SIGTERM); child.wait(timeout=36)
    assert child.returncode==0 and time.monotonic()-began<3
    # A fresh actual process recovers using the same retained supervisor descriptor.
    child=start(work); ready(child,work); child.send_signal(signal.SIGTERM); child.wait(timeout=36); assert child.returncode==0
    print('PASS full Linux hotel bootstrap: two real encrypted SQLite credentials, distinct fixed operations, numeric-UID requests, actual materialized guest exec has no scoped FD; SIGTERM and fresh-process recovery with retained supervisor FD',flush=True)
    work=seed('startup-test'); child=start(work,test=True); ready(child,work,startup_test=True); child.wait(timeout=10); assert child.returncode==0
    print('PASS full Linux hotel scoped startup-test completion and process teardown',flush=True)
    denied_start('wrong-pid',wrong_pid=True); denied_start('extra-alias',extra=True)
    denied_start('disabled',enabled='0'); denied_start('unset',enabled=None)
    denied_start('hidden-disabled',enabled='0',metadata=False); denied_start('hidden-unset',enabled=None,metadata=False)
    denied_start('smoke-rejection',smoke=True)
    denied_start('root-process',uid=0)
    denied_start('wrong-process',uid=1235)
    print('PASS full Linux hotel invalid PID/extra alias/disabled/unset/hidden FD/smoke-mode activation rejected before guest materialization',flush=True)
finally:
    for child in servers:
        if child.poll() is None: child.kill(); child.wait(timeout=3)
    if parent_socket is not None: parent_socket.close()
    SOCKET.unlink(missing_ok=True)
    shutil.rmtree(BASE,ignore_errors=True)
