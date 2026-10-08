"""Disposable-container acceptance for the actual Rust listener; synthetic only."""
import ctypes,json,os,pathlib,selectors,signal,socket,stat,struct,subprocess,sys,time
ROOT=pathlib.Path('/run/percival-personal-vault-broker.sock')
POLICY=pathlib.Path('/etc/percival-personal-mcp/scoped-vault-policy.json')
BINARY='/test-target/debug/linux-listener-fixture'
REFERENCE='secret://hotel/default/percival-muninn-observe/synthetic'
GENERAL=pathlib.Path('/run/percival-fixture-general.sock')
DB=pathlib.Path('/run/percival-fixture-vault.db')
KEY=pathlib.Path('/run/percival-fixture-root.key')
ENV={**os.environ,'PERCIVAL_ISOLATED_LINUX_FIXTURE':'1'}

def caps(effective):
    class Header(ctypes.Structure):_fields_=[('version',ctypes.c_uint32),('pid',ctypes.c_int)]
    class Data(ctypes.Structure):_fields_=[('effective',ctypes.c_uint32),('permitted',ctypes.c_uint32),('inheritable',ctypes.c_uint32)]
    libc=ctypes.CDLL(None,use_errno=True);header=Header(0x20080522,0);data=(Data*2)()
    data[0].effective=effective;data[0].permitted=effective
    assert libc.capset(ctypes.byref(header),ctypes.byref(data))==0,ctypes.get_errno()

def become(uid,bypass=False):
    if bypass:
        libc=ctypes.CDLL(None,use_errno=True)
        assert libc.prctl(8,1,0,0,0)==0,ctypes.get_errno() # PR_SET_KEEPCAPS, fixture only.
    os.setgroups([]);os.setgid(uid);os.setuid(uid)
    if bypass:caps(1<<1) # Only CAP_DAC_OVERRIDE; no ability to change UID again.
    elif uid==0:caps(0)

def receive(connection):
    def exact(size):
        result=b''
        while len(result)<size:
            part=connection.recv(size-len(result))
            if not part:raise EOFError()
            result+=part
        return result
    size=struct.unpack('>I',exact(4))[0];assert 0<size<=8192
    return json.loads(exact(size))

def client(uid,bypass,kind,payload):
    become(uid,bypass);connection=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);connection.settimeout(6)
    start=time.monotonic();result={'uid':os.geteuid()}
    try:
        connection.connect(str(ROOT))
        if kind=='oversized':connection.sendall(struct.pack('>I',4097))
        elif kind=='truncated':connection.sendall(struct.pack('>I',100)+b'{');connection.shutdown(socket.SHUT_WR)
        elif kind=='trickle':
            raw=struct.pack('>I',128)+b'{'*128
            for value in raw:
                try:connection.sendall(bytes([value]))
                except (BrokenPipeError,ConnectionResetError):break
                time.sleep(.1)
        else:
            raw=json.dumps(payload).encode();connection.sendall(struct.pack('>I',len(raw))+raw)
        try:
            reply=receive(connection)
            result.update(accepted=reply=={'credential':('i'*43 if kind=='introspection' else 'mk_synthetic')},denied=reply=={'error':'credential_unavailable'})
        except (EOFError,BrokenPipeError,ConnectionResetError):result.update(accepted=False,denied=True,closed=True)
    except PermissionError:result.update(accepted=False,denied=True,permission_denied=True)
    finally:connection.close()
    result['elapsed']=time.monotonic()-start
    if kind=='filesystem':
        for path in (GENERAL,DB,KEY):
            try:
                if path==GENERAL:
                    probe=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);probe.connect(str(path));probe.close()
                else:fd=os.open(path,os.O_RDONLY);os.close(fd)
                result[path.name]='accessible'
            except PermissionError:result[path.name]='permission_denied'
    print(json.dumps(result));return

if len(sys.argv)>1 and sys.argv[1]=='client':
    client(int(sys.argv[2]),sys.argv[3]=='1',sys.argv[4],json.loads(sys.argv[5]));sys.exit(0)
assert sys.platform=='linux' and os.geteuid()==0
assert os.environ.get('PERCIVAL_ISOLATED_LINUX_FIXTURE')=='1'
assert pathlib.Path('/.dockerenv').is_file(),'disposable Docker fixture required'
VALID={'caller_uid':1234,'hotel_uid':999,'secret_ref':REFERENCE}
REQUEST={'operation':'get_percival_credential'}
servers=[];log={};listener=None
POLICY.parent.mkdir(mode=0o755,exist_ok=True)

def policy(value=VALID,mode=0o644,owner=0):
    if POLICY.is_symlink():POLICY.unlink()
    POLICY.write_text(json.dumps(value));os.chmod(POLICY,mode);os.chown(POLICY,owner,owner)

def bound(path=ROOT,owner=1234,mode=0o600,listen=True):
    try:path.unlink()
    except FileNotFoundError:pass
    result=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);result.bind(str(path))
    os.chmod(path,mode);os.chown(path,owner,owner)
    if listen:result.listen(8)
    return result

def drain(process):
    while True:
        try:part=os.read(process.stdout.fileno(),65536)
        except BlockingIOError:break
        if not part:break
        log[process.pid]=log.get(process.pid,b'')+part

def wait_marker(process,text,count=1,timeout=3):
    deadline=time.monotonic()+timeout
    while time.monotonic()<deadline:
        drain(process)
        if log.get(process.pid,b'').count(text.encode())>=count:return
        if process.poll() is not None:raise AssertionError('fixture exited before marker '+text)
        time.sleep(.01)
    raise AssertionError('fixture marker timed out '+text)

def start(sock,uid=999,stuck=False):
    def drop():os.setgroups([]);os.setgid(uid);os.setuid(uid)
    process=subprocess.Popen([BINARY,str(sock.fileno()),'stuck' if stuck else 'healthy'],pass_fds=(sock.fileno(),),preexec_fn=drop,
        env=ENV,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    os.set_blocking(process.stdout.fileno(),False);servers.append(process);return process

def stop(process,expect=0):
    start_time=time.monotonic();process.send_signal(signal.SIGTERM)
    process.wait(timeout=2);drain(process)
    assert process.returncode==expect,(process.returncode,process.stderr.read().decode())
    assert time.monotonic()-start_time<1.5
    assert b'synthetic-bounded-runtime-shutdown-returned' in log.get(process.pid,b'')
    assert b'mk_synthetic' not in log.get(process.pid,b'') # Credentials never logged.

def pending(uid=1234,bypass=False,kind='valid',payload=REQUEST):
    return subprocess.Popen([sys.executable,__file__,'client',str(uid),'1' if bypass else '0',kind,json.dumps(payload)],
        env=ENV,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)

def finish(process):
    out,err=process.communicate(timeout=7);assert process.returncode==0,err
    return json.loads(out)

def request(**kwargs):return finish(pending(**kwargs))

def rejection(sock,uid=999):
    process=start(sock,uid=uid);process.wait(timeout=3);drain(process)
    assert process.returncode==3,(process.returncode,process.stderr.read().decode())
    assert b'synthetic-resolver-entered' not in log.get(process.pid,b'')

try:
    policy();listener=bound();general=bound(GENERAL,999)
    for path in (DB,KEY):path.write_bytes(b'synthetic fixture only');os.chmod(path,0o600);os.chown(path,999,999)
    metadata=ROOT.lstat();assert stat.S_ISSOCK(metadata.st_mode) and metadata.st_uid==1234 and stat.S_IMODE(metadata.st_mode)==0o600
    server=start(listener);wait_marker(server,'synthetic-listener-generation-started')
    accepted=request(kind='filesystem');assert accepted['accepted'],accepted
    for name in (GENERAL.name,DB.name,KEY.name):assert accepted[name]=='permission_denied',accepted
    drain(server);initial_resolutions=log[server.pid].count(b'synthetic-resolver-entered');assert initial_resolutions==1
    for uid in (1235,999,0):
        # Forced DAC bypass is fixture-only: proves actual Rust peer denial even
        # when a root/hotel/wrong UID reaches the gateway-owned socket.
        denied=request(uid=uid,bypass=True);assert denied['denied'] and not denied['accepted'],denied
    for uid in (1235,0):
        denied=request(uid=uid,kind='filesystem');assert denied.get('permission_denied'),denied
        for name in (GENERAL.name,DB.name,KEY.name):assert denied[name]=='permission_denied',denied
    hotel=request(uid=999,kind='filesystem');assert hotel.get('permission_denied'),hotel
    for name in (GENERAL.name,DB.name,KEY.name):assert hotel[name]=='accessible',hotel
    drain(server);assert log[server.pid].count(b'synthetic-resolver-entered')==initial_resolutions
    print('PASS actual Rust serve: gateway1234 accepted encrypted fixture; wrong1235/root0/hotel999 kernel peers denied even with fixture DAC bypass; gateway denied general IPC/DB/root-key; socket UID/mode verified',flush=True)
    drain(server);before_invalid=log[server.pid].count(b'synthetic-resolver-entered')
    for payload in ({**REQUEST,'secret_ref':'secret://other'},{**REQUEST,'role':'hotel.internal'},
        {**REQUEST,'guest_id':'other'},{**REQUEST,'uid':1234},{'operation':'Register'}):
        denied=request(payload=payload);assert denied['denied'] and not denied['accepted'],denied
    for kind in ('oversized','truncated'):
        denied=request(kind=kind);assert denied['denied'],denied
    drain(server);assert log[server.pid].count(b'synthetic-resolver-entered')==before_invalid
    slow=pending(kind='trickle');time.sleep(.15)
    overlap=request();assert overlap['accepted'] and overlap['elapsed']<1.5,overlap
    denied=finish(slow);assert denied['denied'] and 2.5<denied['elapsed']<4.5,denied
    print('PASS actual Rust framing/policy override denials, total slow-input deadline and overlapping healthy request',flush=True)
    # Four slow reads consume four slots, and excess work is rejected.
    pressure=[pending(kind='trickle') for _ in range(4)];time.sleep(.2)
    denied=request();assert denied['denied'] and denied['elapsed']<1,denied
    for child in pressure:assert finish(child)['denied']
    assert request()['accepted'];stop(server)
    # Policy and real process UID are checked before any resolver invocation.
    for value in ({**VALID,'caller_uid':0},{**VALID,'hotel_uid':0},{**VALID,'caller_uid':999},
        {**VALID,'role':'hotel.internal'},{**VALID,'secret_ref':'secret://other'}):
        policy(value);rejection(listener)
    policy(mode=0o666);rejection(listener)
    policy(owner=1234);rejection(listener)
    policy();os.chmod(POLICY.parent,0o777);rejection(listener);os.chmod(POLICY.parent,0o755)
    original=POLICY.parent/'synthetic-policy-real.json';original.write_text(json.dumps(VALID));original.chmod(0o644)
    POLICY.unlink();POLICY.symlink_to(original);rejection(listener);POLICY.unlink();policy()
    for uid in (0,1234,1235):rejection(listener,uid)
    # Descriptor pathname/ownership/listening checks run in actual serve.
    listener.close()
    for kwargs in ({'owner':0},{'mode':0o666},{'listen':False}):
        candidate=bound(**kwargs);rejection(candidate);candidate.close()
    wrong=bound(path=pathlib.Path('/run/percival-fixture-wrong.sock'));rejection(wrong);wrong.close()
    listener=bound();print('PASS actual Rust startup: unsafe/root/shared UID policy, file/ancestor ownership-mode-symlink, wrong process UID and invalid prebound listener rejected',flush=True)
    # Permanently stuck resolver work, same-process replacement, shutdown and
    # fresh-process recovery exercise real production slots/JoinSet/serve.
    server=start(listener,stuck=True);wait_marker(server,'synthetic-listener-generation-started')
    stuck_clients=[pending() for _ in range(4)];wait_marker(server,'synthetic-resolver-entered',4)
    denied=request();assert denied['denied'] and denied['elapsed']<1,denied
    server.send_signal(signal.SIGHUP);wait_marker(server,'synthetic-listener-generation-started',2)
    for child in stuck_clients:
        denied=finish(child);assert denied['denied'] and not denied['accepted'],denied
    denied=request();assert denied['denied'] and denied['elapsed']<1,denied
    drain(server);assert log[server.pid].count(b'synthetic-resolver-entered')==4
    stop(server)
    # Same inherited listener, new process: fresh slots restore acceptance.
    recovered=start(listener);wait_marker(recovered,'synthetic-listener-generation-started')
    assert request()['accepted'];stop(recovered)
    print('PASS actual Rust four-stuck-job bound persists across listener replacement; old sockets close/no late credentials; bounded process exit and fresh-process recovery succeeded',flush=True)
    print('ALL actual Linux Rust listener synthetic acceptance cases passed',flush=True)
finally:
    for process in servers:
        if process.poll() is None:process.kill();process.wait(timeout=2)
    if listener is not None:listener.close()
