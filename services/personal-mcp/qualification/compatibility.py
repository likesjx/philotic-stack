#!/usr/bin/env python3
"""Known-source Linux aiua builds and historical-client synthetic qualification.
Requires network-none disposable container, zero host mounts, synthetic HOME/state.
Does not inspect installed services, real credentials or memory.
"""
from contextlib import ExitStack, closing
import base64,ctypes,hashlib,json,os,pathlib,shutil,signal,socket,sqlite3,struct,subprocess,time,unittest
ROOT=pathlib.Path('/tmp/hotel-compat-fixture')
OLD='/binaries/historical'
NEW='/binaries/candidate'
GUEST='/guest-target/release/examples/qualification_guest'
HASHES={OLD:os.environ['QUALIFICATION_HISTORICAL_SHA256'],
        NEW:os.environ['QUALIFICATION_CANDIDATE_SHA256']}
HOTEL='qualification-hotel'
KEY=bytes([7]*32)
PROCS=[]
def sha(p):return hashlib.sha256(pathlib.Path(p).read_bytes()).hexdigest()
def drop():os.setgroups([]);os.setgid(987);os.setuid(999)
def env(work,key=None):
    result={'PATH':'/usr/bin:/bin','HOME':str(work/'home'),'PHILOTIC_HOTEL_SOCKET':str(work/'hotel.sock'),
        'PHILOTIC_LOG_DIR':str(work/'logs'),'PHILOTIC_SMOKE_MODE':'1','PHILOTIC_DISABLE_GUEST_SUPERVISOR':'1',
        'PHILOTIC_KEYCHAIN_ENABLED':'0','PHILOTIC_SCOPED_VAULT_ENABLED':'0'}
    if key is not None:result['PHILOTIC_VAULT_MASTER_KEY']=base64.b64encode(key).decode()
    return result
def db(work):return sqlite3.connect(work/'aiua_context.db',timeout=.1)
def config(c,key,value):
    c.execute("INSERT INTO graph_nodes(node_key,kind,label,data_json) VALUES(?,?,?,?) ON CONFLICT(node_key) DO UPDATE SET data_json=excluded.data_json",
        ('config:'+key,'config',key,json.dumps({'value':json.dumps(value)})))
def getconfig(c,key):
    row=c.execute('SELECT data_json FROM graph_nodes WHERE node_key=?',('config:'+key,)).fetchone()
    return json.loads(json.loads(row[0])['value']) if row else None
def credentials(work):
    with db(work) as c:
        return c.execute("SELECT node_key,data_json FROM graph_nodes WHERE kind='secret' OR node_key IN ('config:telegram_bot_token_qualification','config:openrouter_api_key','config:openrouter_api_key_ref','config:vault_registry') ORDER BY node_key").fetchall()
def encrypt(value):
    crypto=ctypes.CDLL('libcrypto.so.3');ptr=ctypes.c_void_p
    crypto.EVP_CIPHER_CTX_new.restype=ptr;crypto.EVP_aes_256_gcm.restype=ptr
    crypto.EVP_EncryptInit_ex.argtypes=[ptr,ptr,ptr,ptr,ptr];crypto.EVP_EncryptUpdate.argtypes=[ptr,ptr,ptr,ptr,ctypes.c_int]
    crypto.EVP_EncryptFinal_ex.argtypes=[ptr,ptr,ptr];crypto.EVP_CIPHER_CTX_ctrl.argtypes=[ptr,ctypes.c_int,ctypes.c_int,ptr]
    crypto.EVP_CIPHER_CTX_free.argtypes=[ptr]
    ctx=crypto.EVP_CIPHER_CTX_new();out=ctypes.create_string_buffer(4096);size=ctypes.c_int();final=ctypes.c_int();tag=ctypes.create_string_buffer(16);nonce=bytes([3]*12)
    try:
        assert crypto.EVP_EncryptInit_ex(ctx,crypto.EVP_aes_256_gcm(),None,KEY,nonce)==1
        assert crypto.EVP_EncryptUpdate(ctx,out,ctypes.byref(size),value,len(value))==1
        assert crypto.EVP_EncryptFinal_ex(ctx,ctypes.byref(out,size.value),ctypes.byref(final))==1
        assert crypto.EVP_CIPHER_CTX_ctrl(ctx,0x10,16,tag)==1
        return base64.b64encode(out.raw[:size.value+final.value]+tag.raw).decode(),base64.b64encode(nonce).decode()
    finally:crypto.EVP_CIPHER_CTX_free(ctx)
def secret(c,kind='openrouter_api_key',plain=b'public-provider-key',corrupt=False):
    ref='secret://hotel/default/'+kind+'/public-fixture';cipher,nonce=encrypt(plain)
    record={'secret_ref':ref,'secret_kind':kind,'scope':'hotel','allowed_roles':['model'],'allowed_guests':[],
        'ciphertext_b64':cipher if not corrupt else 'invalid','nonce_b64':nonce,'created_at':0,'updated_at':0}
    c.execute('INSERT INTO graph_nodes(node_key,kind,label,data_json) VALUES(?,?,?,?)',('secret:'+ref,'secret',ref,json.dumps(record)))
    return ref
def owned(work):
    for root,dirs,files in os.walk(work):
        os.chown(root,999,987)
        for name in files:os.chown(pathlib.Path(root)/name,999,987)
def seed(name,provider=True):
    work=ROOT/name;work.mkdir(mode=0o755);(work/'home/.philotic').mkdir(parents=True)
    (work/'home/.philotic/vault-master-key.env').write_text('PHILOTIC_VAULT_MASTER_KEY='+base64.b64encode(KEY).decode()+'\n')
    (work/'public-config.json').write_text('{}\n');owned(work)
    p=subprocess.run([OLD,'load','--file',str(work/'public-config.json'),'--hotel',HOTEL],cwd=work,env=env(work),preexec_fn=drop,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=20)
    assert p.returncode==0,p.stderr.decode()[-1000:]
    with db(work) as c:
        row=c.execute("SELECT node_key,data_json FROM graph_nodes WHERE kind='hotel' AND json_extract(data_json,'$.hotel_name')=?",(HOTEL,)).fetchone()
        assert row
        hotel=json.loads(row[1]);hotel['ipc_socket_path']=str(work/'hotel.sock')
        c.execute('DELETE FROM graph_edges');c.execute('DELETE FROM graph_nodes')
        c.execute('INSERT INTO graph_nodes(node_key,kind,label,data_json) VALUES(?,?,?,?)',(row[0],'hotel',HOTEL,json.dumps(hotel)))
        config(c,'qualification-marker',{'public':'preserve','version':1})
        config(c,'telegram_bot_token_qualification','public-telegram-token')
        if provider:config(c,'openrouter_api_key','public-provider-key')
        sentinels=[
          ('session:qualification:sentinel','session',{'session_id':'qualification:sentinel','session_kind':'qualification','primary_agent_id':None,'channel_kind':None,'channel_session_key':None,'status':'idle','lease_owner_component_id':None,'lease_expires_at':None,'summary_json':{'public':'preserve'},'created_at':123,'updated_at':123}),
          ('role_incarnation:qualification-agent:qualification-role','role_incarnation',{'agent_id':'qualification-agent','role_name':'qualification-role','guest_id':'qualification-role-guest','toolset_profile':'worker','role_manifest':'public preserve','is_admin':False,'home_node':'public-offline-home','placement_updated_unix':123}),
          ('autonomy_grant:qualification.fixture','autonomy_grant',{'lane':'qualification.fixture','posture':'confirm_first','budget':{'max_actions_per_day':2,'max_consecutive_failures':1},'earned':{'confirmed_good_outcomes':3,'required_for_promotion':5,'consecutive_failures':0},'frozen_until_operator_review':True,'actions_today':1,'action_day_utc':0,'updated_at':123})]
        for key,kind,data in sentinels:
            c.execute('INSERT INTO graph_nodes(node_key,kind,label,data_json) VALUES(?,?,?,?)',(key,kind,key,json.dumps(data)))
    owned(work);return work
def markers(work):
    with db(work) as c:return c.execute("SELECT node_key,data_json FROM graph_nodes WHERE node_key IN ('session:qualification:sentinel','role_incarnation:qualification-agent:qualification-role','autonomy_grant:qualification.fixture','config:qualification-marker') ORDER BY node_key").fetchall()
def launch(work,binary=NEW,key=None):
    log=(work/('process-'+str(time.time_ns())+'.log')).open('wb')
    p=subprocess.Popen([binary,'--hotel',HOTEL],cwd=work,env=env(work,key),preexec_fn=drop,stdout=log,stderr=log)
    PROCS.append(p);p.fixture_log=log
    return p
def ready(work,p):
    until=time.monotonic()+12
    while time.monotonic()<until:
        if p.poll() is not None:raise AssertionError('hotel exited before readiness')
        try:
            with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as s:s.connect(str(work/'hotel.sock'));return
        except OSError:time.sleep(.03)
    raise AssertionError('IPC readiness deadline')
def stop(p,hard=False):
    if p.poll() is None:p.send_signal(signal.SIGKILL if hard else signal.SIGINT)
    p.wait(timeout=10);p.fixture_log.close()
    if not hard:assert p.returncode==0,p.returncode
def failstart(work,key=None):
    p=launch(work,key=key)
    try:p.wait(timeout=12)
    except subprocess.TimeoutExpired:
        stop(p);raise AssertionError('invalid credentials unexpectedly admitted startup')
    p.fixture_log.close();assert p.returncode!=0
def guest(work,label):
    with db(work) as c:
        hotel=json.loads(c.execute("SELECT data_json FROM graph_nodes WHERE kind='hotel'").fetchone()[0]);node=hotel['capabilities']['node_id']
    out=subprocess.check_output([GUEST,str(work/'hotel.sock'),node,label],env={'PATH':'/usr/bin:/bin','HOME':str(work/'home')},preexec_fn=drop,timeout=30,text=True)
    print(out,flush=True)
    return node
def integrity(work):
    with db(work) as c:assert c.execute('PRAGMA integrity_check').fetchone()==('ok',)
def snapshot(work,target):
    assert all(p.poll() is not None for p in PROCS), 'all fixture hotel writers must be stopped'
    target.mkdir();(target/'home').mkdir()
    names=('aiua_context.db','whisper_training.db')
    present={name:(work/name).exists() for name in names}
    assert present['aiua_context.db']
    with ExitStack() as stack:
        sources={}
        versions={}
        # Establish both observations before either backup. Rechecking each DB
        # only around its own backup would accept an old/new mixed pair.
        for name in names:
            if not present[name]:continue
            assert (work/name).is_file()
            source=stack.enter_context(closing(sqlite3.connect(work/name)))
            sources[name]=source
            versions[name]=source.execute('PRAGMA data_version').fetchone()
        for name,source in sources.items():
            with closing(sqlite3.connect(target/name)) as dest:
                source.backup(dest)
                assert dest.execute('PRAGMA integrity_check').fetchone()==('ok',)
            # Synthetic snapshot metadata is part of executable+state recovery.
            # SQLite backup creates a new inode; it does not preserve mode/owner.
            original=(work/name).stat()
            shutil.copystat(work/name,target/name)
            os.chown(target/name,original.st_uid,original.st_gid)
        shutil.copytree(work/'home',target/'home',dirs_exist_ok=True)
        shutil.copy2(work/'public-config.json',target/'public-config.json')
        shutil.copy2(work/'selection.json',target/'selection.json')
        assert {name:(work/name).exists() for name in names}==present, 'fixture database presence changed during backup'
        for name,source in sources.items():
            assert source.execute('PRAGMA data_version').fetchone()==versions[name], 'unexpected fixture writer during backup'
def restore(work,snap):
    assert all(p.poll() is not None for p in PROCS), 'all fixture hotel writers must be stopped'
    assert (snap/'aiua_context.db').is_file()
    for name in ('aiua_context.db','whisper_training.db'):
        for suffix in ('','-wal','-shm','-journal'):(work/(name+suffix)).unlink(missing_ok=True)
        if (snap/name).exists():shutil.copy2(snap/name,work/name)
    shutil.rmtree(work/'home');shutil.copytree(snap/'home',work/'home')
    shutil.copy2(snap/'public-config.json',work/'public-config.json');shutil.copy2(snap/'selection.json',work/'selection.json')
    owned(work)
def logical(work):
    with db(work) as c:return [c.execute('SELECT * FROM '+table+' ORDER BY 1').fetchall() for table in ('graph_nodes','graph_edges','node_config','vault_secrets','hotels','materialized_guests','agent_identities','memory_apartments')]
class Qualification(unittest.TestCase):
    def test_01_historical_client_roundtrips_both_hotels(self):
        for label,binary in [('historical',OLD),('candidate',NEW)]:
            with self.subTest(label=label):
                w=seed('protocol-'+label);before=markers(w);p=launch(w,binary);ready(w,p)
                guest(w,label);stop(p);integrity(w);self.assertEqual(markers(w),before)
                with db(w) as c:
                    session=c.execute("SELECT data_json FROM graph_nodes WHERE kind='session' AND json_extract(data_json,'$.session_id')=?",('qualification:'+label,)).fetchall()
                    self.assertEqual(len(session),1)
                    turns=c.execute("SELECT data_json FROM graph_nodes WHERE kind='session_turn' AND json_extract(data_json,'$.session_id')=? AND json_extract(data_json,'$.turn_id')=?",('qualification:'+label,'turn:'+label)).fetchall()
                    self.assertEqual(len(turns),1)
                    self.assertEqual(json.loads(turns[0][0])['user_message_json']['content'],'public-input')
    def test_02_legacy_columns_preserved_and_added(self):
        w=seed('legacy-columns')
        columns={'materialized_guests':['active_pid','hotel_name','last_active_at'],'hotels':['execution_port'],'agent_identities':['authority_hotel']}
        with db(w) as c:
            for table,names in columns.items():
                for name in names:c.execute('ALTER TABLE '+table+' DROP COLUMN '+name)
            c.execute("INSERT INTO materialized_guests(guest_id,role,config_json,is_active) VALUES('public-dormant','tool','{}',0)")
            c.execute("INSERT INTO agent_identities(agent_id,persona_name,bundle_json) VALUES('public-agent','Public','{}')")
        owned(w);p=launch(w);ready(w,p);stop(p)
        with db(w) as c:
            for table,names in columns.items():self.assertTrue(set(names)<={row[1] for row in c.execute('PRAGMA table_info('+table+')')})
            self.assertEqual(c.execute("SELECT role,config_json,is_active,hotel_name,active_pid FROM materialized_guests WHERE guest_id='public-dormant'").fetchone(),('tool','{}',0,'default',None))
            self.assertEqual(c.execute("SELECT persona_name,bundle_json,authority_hotel FROM agent_identities WHERE agent_id='public-agent'").fetchone(),('Public','{}',''))
        integrity(w)
    def test_03_plaintext_migration_repeat_and_existing_reference(self):
        w=seed('repeat');before=markers(w);p=launch(w);ready(w,p);guest(w,'repeat-first');stop(p);first=credentials(w)
        p=launch(w);ready(w,p);guest(w,'repeat-second');stop(p)
        self.assertEqual(credentials(w),first);self.assertEqual(markers(w),before)
        with db(w) as c:self.assertIsNone(getconfig(c,'openrouter_api_key'));self.assertEqual(c.execute("SELECT count(*) FROM graph_nodes WHERE kind='secret'").fetchone()[0],2)
        # A valid existing ref wins over duplicate legacy plaintext without rotation.
        with db(w) as c:config(c,'openrouter_api_key','public-obsolete-duplicate')
        p=launch(w);ready(w,p);guest(w,'existing-valid');stop(p);self.assertEqual(credentials(w),first);integrity(w)
    def test_04_invalid_existing_refs_preserve_plaintext(self):
        for variant in ('missing','corrupt','wrong-kind','empty','wrong-key','wrong-acl','guest-restricted'):
            with self.subTest(variant=variant):
                w=seed('invalid-'+variant)
                with db(w) as c:
                    ref='secret://hotel/default/openrouter_api_key/missing'
                    if variant!='missing':ref=secret(c,kind='telegram_bot_token' if variant=='wrong-kind' else 'openrouter_api_key',plain=b'' if variant=='empty' else b'public-provider-key',corrupt=variant=='corrupt')
                    if variant in ('wrong-acl','guest-restricted'):
                        row=c.execute('SELECT data_json FROM graph_nodes WHERE node_key=?',('secret:'+ref,)).fetchone()
                        value=json.loads(row[0])
                        if variant=='wrong-acl':value['allowed_roles']=['membrane']
                        else:value['allowed_guests']=['unrelated-public-guest']
                        c.execute('UPDATE graph_nodes SET data_json=? WHERE node_key=?',(json.dumps(value),'secret:'+ref))
                    config(c,'openrouter_api_key_ref',ref)
                before=credentials(w);owned(w);failstart(w,bytes([8]*32) if variant=='wrong-key' else None)
                self.assertEqual(credentials(w),before);integrity(w)
    def test_05_failed_config_write_is_atomic_and_retry_unique(self):
        w=seed('write-interruption');before=credentials(w)
        with db(w) as c:c.execute("CREATE TRIGGER fixture_abort_ref BEFORE INSERT ON graph_nodes WHEN NEW.node_key='config:openrouter_api_key_ref' BEGIN SELECT RAISE(ABORT,'synthetic migration boundary'); END")
        owned(w);failstart(w);self.assertEqual(credentials(w),before,'failed config write left orphan secret/registry/ref')
        with db(w) as c:c.execute('DROP TRIGGER fixture_abort_ref')
        p=launch(w);ready(w,p);guest(w,'write-retry');stop(p)
        with db(w) as c:self.assertEqual(c.execute("SELECT count(*) FROM graph_nodes WHERE kind='secret'").fetchone()[0],2)
        integrity(w)
    def test_06_kill_pending_writer_then_repeat_start(self):
        w=seed('kill-writer');before=credentials(w)
        reader=db(w);reader.execute('BEGIN');reader.execute('SELECT * FROM graph_nodes').fetchall()
        p=launch(w);until=time.monotonic()+8
        try:
            while time.monotonic()<until:
                self.assertIsNone(p.poll(),'writer exited instead of blocking')
                if (w/'aiua_context.db-journal').exists():break
                time.sleep(.02)
            else:self.fail('never reached real SQLite write boundary')
            self.assertFalse((w/'hotel.sock').exists());stop(p,hard=True)
        finally:reader.rollback();reader.close()
        self.assertEqual(credentials(w),before);integrity(w)
        p=launch(w);ready(w,p);guest(w,'kill-retry');stop(p);first=credentials(w)
        p=launch(w);ready(w,p);stop(p);self.assertEqual(credentials(w),first)
    def test_07_commit_busy_rolls_back_credential_writes(self):
        w=seed('commit-busy');before=credentials(w)
        reader=db(w);reader.execute('BEGIN');reader.execute('SELECT * FROM graph_nodes').fetchall()
        try:
            failstart(w)
            logs=b''.join(path.read_bytes() for path in w.glob('process-*.log'))
            self.assertIn(b'commit credential migration',logs,'startup failed before the COMMIT boundary')
        finally:reader.rollback();reader.close()
        self.assertEqual(credentials(w),before);integrity(w)
        p=launch(w);ready(w,p);guest(w,'commit-busy-retry');stop(p)
    def test_08_restore_state_and_executable_then_old_roundtrip(self):
        w=seed('rollback');selection=w/'selected-aiua';selection.symlink_to(OLD)
        (w/'selection.json').write_text(json.dumps({'path':OLD,'sha256':HASHES[OLD]}))
        p=launch(w,str(selection));ready(w,p);guest(w,'before-rollback');stop(p)
        # Hotel is stopped. Keep committed WAL frames present in both databases:
        # a main-file-only copy must not satisfy this restoration test.
        held=[]
        try:
            for name in ('aiua_context.db','whisper_training.db'):
                c=sqlite3.connect(w/name);held.append(c)
                self.assertEqual(c.execute('PRAGMA journal_mode=WAL').fetchone(),('wal',))
                c.execute('PRAGMA wal_autocheckpoint=0')
                c.execute('CREATE TABLE qualification_backup_marker(value TEXT PRIMARY KEY)')
                c.execute("INSERT INTO qualification_backup_marker VALUES('public-committed-wal')")
                c.commit()
                self.assertGreater((w/(name+'-wal')).stat().st_size,32)
            before=logical(w);before_credentials=credentials(w);before_markers=markers(w)
            with sqlite3.connect(w/'whisper_training.db') as c:
                before_training=c.execute('SELECT * FROM whisper_training_samples ORDER BY sample_id').fetchall()
            snap=ROOT/'rollback-snapshot';snapshot(w,snap)
            for name in ('aiua_context.db','whisper_training.db'):
                with sqlite3.connect(snap/name) as c:
                    self.assertEqual(c.execute('SELECT value FROM qualification_backup_marker').fetchall(),[('public-committed-wal',)])
        finally:
            for c in held:c.close()
        selection.unlink();selection.symlink_to(NEW);(w/'selection.json').write_text(json.dumps({'path':NEW,'sha256':HASHES[NEW]}))
        p=launch(w,str(selection));ready(w,p);guest(w,'candidate-before-restore');stop(p)
        with db(w) as c:config(c,'qualification-candidate-only',True)
        with sqlite3.connect(w/'whisper_training.db') as c:
            c.execute("INSERT INTO qualification_backup_marker VALUES('candidate-only')")
            c.execute("INSERT INTO whisper_training_samples(sample_id,agent_id,raw_transcript,timestamp) VALUES('public-candidate-only','fixture','synthetic transcript',1)")
        restore(w,snap);selection.unlink();selection.symlink_to(OLD)
        for name in ('aiua_context.db','whisper_training.db'):
            with sqlite3.connect(w/name) as c:
                self.assertEqual(c.execute('PRAGMA integrity_check').fetchone(),('ok',))
                self.assertEqual(c.execute('SELECT value FROM qualification_backup_marker').fetchall(),[('public-committed-wal',)])
        with sqlite3.connect(w/'whisper_training.db') as c:
            self.assertEqual(c.execute('SELECT * FROM whisper_training_samples ORDER BY sample_id').fetchall(),before_training)
        self.assertEqual(sha(selection),HASHES[OLD]);self.assertEqual(logical(w),before)
        self.assertEqual((w/'home/.philotic/vault-master-key.env').read_bytes(),(snap/'home/.philotic/vault-master-key.env').read_bytes())
        self.assertEqual(json.loads((w/'selection.json').read_text())['sha256'],HASHES[OLD])
        p=launch(w,str(selection));ready(w,p);guest(w,'after-rollback');stop(p)
        self.assertEqual(credentials(w),before_credentials);self.assertEqual(markers(w),before_markers);integrity(w)
    def test_09_restore_training_absence_before_old_start(self):
        w=seed('rollback-training-absent')
        self.assertFalse((w/'whisper_training.db').exists())
        (w/'selection.json').write_text(json.dumps({'path':OLD,'sha256':HASHES[OLD]}))
        before=logical(w);snap=ROOT/'absent-training-snapshot';snapshot(w,snap)
        self.assertFalse((snap/'whisper_training.db').exists())
        p=launch(w,NEW);ready(w,p);stop(p)
        self.assertTrue((w/'whisper_training.db').is_file())
        with sqlite3.connect(w/'whisper_training.db') as c:
            c.execute("INSERT INTO whisper_training_samples(sample_id,agent_id,raw_transcript,timestamp) VALUES('public-candidate-only','fixture','synthetic transcript',1)")
        restore(w,snap)
        for suffix in ('','-wal','-shm','-journal'):
            self.assertFalse((w/('whisper_training.db'+suffix)).exists())
        self.assertEqual(logical(w),before);integrity(w)
        p=launch(w,OLD);ready(w,p);guest(w,'restored-training-absence');stop(p)
        with sqlite3.connect(w/'whisper_training.db') as c:
            self.assertEqual(c.execute("SELECT COUNT(*) FROM whisper_training_samples WHERE sample_id='public-candidate-only'").fetchone(),(0,))

    def test_10_reject_between_database_backup_writer(self):
        from unittest.mock import patch
        w=ROOT/'between-backups';w.mkdir();(w/'home').mkdir()
        for name in ('public-config.json','selection.json'):(w/name).write_text('{}')
        names=('aiua_context.db','whisper_training.db')
        original=sqlite3.connect
        for name in names:
            with closing(original(w/name)) as c:
                c.execute('CREATE TABLE qualification_backup_marker(value TEXT)')
                c.execute("INSERT INTO qualification_backup_marker VALUES('before')")
                c.commit()
        target=ROOT/'between-backups-snapshot'
        injected=False
        def connect(path,*args,**kwargs):
            nonlocal injected
            if pathlib.Path(path)==target/'whisper_training.db':
                assert not injected;injected=True
                for name in names:
                    with closing(original(w/name)) as c:
                        c.execute("UPDATE qualification_backup_marker SET value='after'")
                        c.commit()
            return original(path,*args,**kwargs)
        with patch.object(sqlite3,'connect',connect):
            with self.assertRaisesRegex(AssertionError,'unexpected fixture writer'):
                snapshot(w,target)
        self.assertTrue(injected)
        # Prove the negative control actually produced the mixed backup pair.
        for name,expected in zip(names,('before','after')):
            with closing(original(target/name)) as c:
                self.assertEqual(c.execute('SELECT value FROM qualification_backup_marker').fetchone(),(expected,))


    def test_11_multiple_holders_and_mode_preserving_recovery(self):
        import select
        w=ROOT/'multiple-holders';w.mkdir(mode=0o755);(w/'home').mkdir()
        for name in ('public-config.json','selection.json'):(w/name).write_text('{}')
        for name,mode in (('aiua_context.db',0o600),('whisper_training.db',0o644)):
            with closing(sqlite3.connect(w/name)) as c:
                c.execute('CREATE TABLE public_marker(value TEXT)')
                c.execute("INSERT INTO public_marker VALUES('before')");c.commit()
            os.chmod(w/name,mode)
        owned(w)
        # Seven real disposable processes, not assumed main-PID-only fencing.
        code="import sqlite3,sys,time; a=sqlite3.connect(sys.argv[1]); b=sqlite3.connect(sys.argv[2]); print('ready',flush=True); time.sleep(60)"
        holders=[]
        try:
            for _ in range(7):
                process=subprocess.Popen(['/usr/bin/python3','-I','-B','-c',code,str(w/'aiua_context.db'),str(w/'whisper_training.db')],preexec_fn=drop,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
                holders.append(process);PROCS.append(process)
                self.assertTrue(select.select([process.stdout],[],[],3)[0],'synthetic holder readiness timeout')
                self.assertEqual(process.stdout.readline(),b'ready\n')
            holders[0].terminate();holders[0].wait(timeout=3)
            with self.assertRaisesRegex(AssertionError,'all fixture hotel writers'):
                snapshot(w,ROOT/'must-not-snapshot')
            self.assertFalse((ROOT/'must-not-snapshot').exists())
            for process in holders[1:-1]:process.terminate();process.wait(timeout=3)
            with self.assertRaisesRegex(AssertionError,'all fixture hotel writers'):
                restore(w,ROOT/'must-not-restore')
            holders[-1].terminate();holders[-1].wait(timeout=3)
            before={name:((w/name).stat().st_uid,(w/name).stat().st_gid,(w/name).stat().st_mode&0o777) for name in ('aiua_context.db','whisper_training.db')}
            self.assertEqual(before,{'aiua_context.db':(999,987,0o600),'whisper_training.db':(999,987,0o644)})
            snap=ROOT/'multiple-holders-snapshot';snapshot(w,snap)
            for name in before:
                self.assertEqual(((snap/name).stat().st_uid,(snap/name).stat().st_gid,(snap/name).stat().st_mode&0o777),before[name])
                with closing(sqlite3.connect(w/name)) as c:c.execute("UPDATE public_marker SET value='candidate-only'");c.commit()
                os.chmod(w/name,0o666)
                # Stale sidecars must be removed, never copied over a restored main.
                for suffix,mode in (('-wal',0o664 if name=='aiua_context.db' else 0o644),('-shm',0o664 if name=='aiua_context.db' else 0o644),('-journal',0o600)):
                    side=w/(name+suffix);side.write_bytes(b'public-stale-sidecar');os.chmod(side,mode)
            restore(w,snap)
            for name in before:
                self.assertEqual(((w/name).stat().st_uid,(w/name).stat().st_gid,(w/name).stat().st_mode&0o777),before[name])
                for suffix in ('-wal','-shm','-journal'):self.assertFalse((w/(name+suffix)).exists())
                with closing(sqlite3.connect(w/name)) as c:self.assertEqual(c.execute('SELECT value FROM public_marker').fetchall(),[('before',)])
        finally:
            for process in holders:
                if process.poll() is None:process.kill();process.wait(timeout=3)
                process.stdout.close()

if __name__=='__main__':
    assert os.geteuid()==0 and pathlib.Path('/.dockerenv').is_file()
    assert set(os.listdir('/sys/class/net'))=={'lo'},'network isolation missing'
    assert os.environ.get('PERCIVAL_ISOLATED_LINUX_FIXTURE')=='1'
    assert not ROOT.exists()
    for binary,expected in HASHES.items():assert sha(binary)==expected,(binary,sha(binary))
    ROOT.mkdir(mode=0o755)
    try:
        suite=unittest.defaultTestLoader.loadTestsFromTestCase(Qualification)
        result=unittest.TextTestRunner(verbosity=2).run(suite)
        report={'passed':result.wasSuccessful(),'tests_run':result.testsRun,'failures':len(result.failures),'errors':len(result.errors),
            'binaries':HASHES,'guest_sha256':sha(GUEST),'fixture_sha256':sha(__file__),'installed_equivalence':False}
        pathlib.Path('/evidence/compatibility-result.json').write_text(json.dumps(report,sort_keys=True,indent=2)+'\n')
        raise SystemExit(0 if result.wasSuccessful() else 1)
    finally:
        for p in PROCS:
            if p.poll() is None:p.kill();p.wait(timeout=5)
        shutil.rmtree(ROOT)
