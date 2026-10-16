#!/usr/bin/env python3
"""Synthetic only: disposable Docker databases and actual supervised children.

No inherited service configuration, host ports, production paths or credentials.
Requires explicit compiled artifact and exact issuer source checkout arguments.
"""
import argparse, concurrent.futures, hashlib, json, os, pathlib, shutil, signal
import subprocess, tempfile, time, threading, urllib.request, urllib.error, uuid

MONGO = 'mongo@sha256:d0d926f94df099bff534b7ee5b5986458131a22489dfff8664509af0c1e2ca9c'
GRAPH = 'memgraph/memgraph@sha256:bd01a159023283b56b807943ed28225c8f662920b64317f26da7d9f0f5b19de4'

def run(*args, **kw):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kw).stdout.strip()

def digest(path):
    return hashlib.file_digest(open(path, 'rb'), 'sha256').hexdigest()

def http(url, payload=None, bearer=None):
    headers = {'Content-Type': 'application/json'}
    if bearer: headers['Authorization'] = 'Bearer ' + bearer
    request = urllib.request.Request(url, data=json.dumps(payload).encode() if payload is not None else None, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=12) as response: return response.status, response.read().decode()
    except urllib.error.HTTPError as error: return error.code, error.read().decode()


assert os.environ.get('LIFEGRAPH_DISPOSABLE_COMPOSED') == '1'
p = argparse.ArgumentParser()
p.add_argument('--work',type=pathlib.Path,required=True)
p.add_argument('--supervisor',type=pathlib.Path,required=True)
p.add_argument('--graph-ip',required=True)
p.add_argument('--mongo-ip',required=True)
p.add_argument('--issuer-head',required=True)
a=p.parse_args()
work=a.work;supervisor=a.supervisor;node=work/'node';bundle=work/'worker.mjs'
graph_ip=a.graph_ip;mongo_ip=a.mongo_ip
results=[]
def exercise():
    for case in ['privacy','handoff','before-final','issuer-first','creator-revoked','role-revoked','global','logout','token-revoke','unknown']:
        d=work/case;d.mkdir(mode=0o700)
        def put(name, value='synthetic'):
            p=d/name;p.write_text(value if isinstance(value,str) else json.dumps(value));p.chmod(0o600)
        put('journal','clean\n');put('connection.json',{'uri':graph_ip+':7687','user':'synthetic','password':''})
        worker=lambda role:{'guest':role,'agent':'synthetic-'+role,'roles':[],'role':role,'count':1,
            'executable':{'path':str(node),'sha256':digest(node)},'artifacts':[{'path':str(bundle),'sha256':digest(bundle)}],
            'directory':str(d),'arguments':[str(bundle)],'environment':{'SYNTHETIC_DIR':str(d),'SYNTHETIC_ROLE':role,
                'SYNTHETIC_MONGO_URI':'mongodb://'+mongo_ip+':27017','SYNTHETIC_DATABASE':'synthetic_'+case.replace('-','_')}}
        manifest={'version':1,'hotel':'synthetic-hotel','owner':'synthetic-owner','uid':os.geteuid(),'socket':str(d/'owner.sock'),
            'journal':str(d/'journal'),'graph_connection_file':str(d/'connection.json'),'workers':[worker('issuer'),worker('gateway')]}
        put('manifest.json',manifest)
        launch=lambda:subprocess.Popen([str(supervisor),'--config',str(d/'manifest.json')],env={},stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
        child=launch();restart=None
        contention=threading.Condition(); arrivals=[0]
        def observe():
            for line in child.stderr:
                if line.strip()==b'LifeGraph owner fence contended':
                    with contention:
                        arrivals[0]+=1;contention.notify_all()
        threading.Thread(target=observe,daemon=True).start()
        def queued(previous):
            with contention:
                assert contention.wait_for(lambda: arrivals[0]>previous,timeout=4), (case,'contender never reached held owner fence')
        def wait(name):
            deadline=time.monotonic()+60
            while not (d/name).exists():
                assert child.poll() is None and time.monotonic()<deadline,(case,name,[p.read_text() for p in d.glob('fixture-error-*')])
                time.sleep(.01)
        try:
            wait('gateway.json');data=json.loads((d/'issuer.json').read_text());gateway=json.loads((d/'gateway.json').read_text())
            base='http://127.0.0.1:'+str(data['port']);endpoint='http://127.0.0.1:'+str(gateway['port'])+'/life/mcp'
            control=lambda op: http(base+'/control?op='+op)
            def recall(client='dot', memory=False):
                return http(endpoint,{'jsonrpc':'2.0','id':1,'method':'tools/call','params':{'name':'life.recall','arguments':{'query_text':'synthetic','max_context_packets':12}}},data['minted'][client]['memory' if memory else 'cap'])
            def visible(result):
                assert result[0]==200 and 'goal:synthetic-root' in result[1],result
                packet=json.loads(json.loads(result[1])['result']['content'][0]['text'])
                assert {p['id'] for p in packet['packets']}=={'goal:synthetic-root','openloop:synthetic','asset:synthetic'},packet
                assert packet['edges']==[],packet
                assert all(x not in result[1] for x in ['person:synthetic','idea:synthetic','edge:synthetic','place:synthetic','synthetic-other']),result
                assert result[1].count('goal:synthetic-root')==1,result
            visible(recall());assert recall(memory=True)[0]==401
            claude=recall('claude');assert 'goal:synthetic-other' in claude[1] and 'synthetic-root' not in claude[1],claude
            with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
                if case=='handoff':
                    put('pause-handoff');read=pool.submit(recall);wait('handoff-held')
                    # Actual SQLite competing writer must fail during HTTP handoff.
                    script="const {DatabaseSync}=require('node:sqlite');const db=new DatabaseSync(process.argv[1],{timeout:50});try{db.exec('UPDATE privacy_revision SET revision=revision+1');process.exitCode=1}catch(e){if(!String(e).includes('locked'))throw e}finally{db.close()}"
                    run(str(node),'-e',script,str(d/'privacy.sqlite'))
                    previous=arrivals[0];revoke=pool.submit(control,'revoke-client');queued(previous)
                    concurrent.futures.wait([revoke],timeout=.25)
                    assert not revoke.done() and not (d/'first-mongo-write').exists()
                    put('finish-handoff');visible(read.result());assert revoke.result()[0]==200
                    assert (d/'first-mongo-write').exists();(d/'pause-handoff').unlink()
                    assert recall()[0]==401 and recall('claude')[0]==200
                    assert control('reenable')[0]==200 and recall()[0]==401
                elif case=='before-final':
                    put('pause-final');read=pool.submit(recall);wait('before-final');assert control('revoke-client')[0]==200
                    put('finish-final');result=read.result();assert result==(503,'{"error":"service_unavailable"}'),result
                    assert recall('claude')[0]==200
                elif case=='issuer-first':
                    revoke=pool.submit(control,'hold-revoke');wait('issuer-held');previous=arrivals[0];read=pool.submit(recall);queued(previous)
                    concurrent.futures.wait([read],timeout=.25)
                    assert not read.done();put('finish-issuer');assert revoke.result()[0]==200
                    result=read.result();assert result==(503,'{"error":"service_unavailable"}'),result
                    assert recall('claude')[0]==200
                elif case=='creator-revoked':
                    script="const {DatabaseSync}=require('node:sqlite');const db=new DatabaseSync(process.argv[1]);db.exec('BEGIN IMMEDIATE');const rows=db.prepare('SELECT resource,policy_json FROM privacy_policy').all();for(const row of rows){const p=JSON.parse(row.policy_json);p.creator_read_grant=false;db.prepare('UPDATE privacy_policy SET policy_json=? WHERE resource=?').run(JSON.stringify(p),row.resource)}db.exec('UPDATE privacy_revision SET revision=revision+1;COMMIT');db.close()"
                    run(str(node),'-e',script,str(d/'privacy.sqlite'));result=recall();assert result[0]==200,result
                    packet=json.loads(json.loads(result[1])['result']['content'][0]['text'])
                    assert [p['id'] for p in packet['packets']]==['asset:synthetic'] and packet['edges']==[],result
                elif case=='role-revoked':
                    assert control('role-revoke')[0]==200
                    assert recall()[0]==401 and recall('claude')[0]==200
                    minted=control('mint');assert minted[0]==200;data['minted']['dot']=json.loads(minted[1])
                    result=recall();assert result[0]==200,result
                    packet=json.loads(json.loads(result[1])['result']['content'][0]['text'])
                    assert {p['id'] for p in packet['packets']}=={'goal:synthetic-root','openloop:synthetic'} and packet['edges']==[],result
                elif case in ['global','logout','token-revoke']:
                    assert control(case)[0]==200 and recall()[0]==401
                    assert recall('claude')[0]==(200 if case=='token-revoke' else 401)
                elif case=='unknown':
                    assert control('unknown')[0]==503
                    assert (d/'journal').read_text()=='dirty\n'
                    assert recall()[0]==401
                    claims=json.loads(http(base+'/inspect?token='+data['minted']['claude']['cap'])[1])
                    assert claims['active'] is True
                    assert recall('claude')==(503,'{"error":"service_unavailable"}')
            child.send_signal(signal.SIGTERM);child.wait(timeout=15)
            if case=='unknown':
                assert (d/'journal').read_text()=='dirty\n';restart=launch();assert restart.wait(timeout=60)!=0
                assert (d/'journal').read_text()=='dirty\n'
            else: assert child.returncode==0 and (d/'journal').read_text()=='clean\n'
            results.append({'case':case,'passed':True});print(case+': passed',flush=True)
        finally:
            for process in [child,restart]:
                if process is not None and process.poll() is None: process.kill();process.wait(timeout=10)
    print(json.dumps({'issuer_head':a.issuer_head,'supervisor_sha256':digest(supervisor),'worker_sha256':digest(bundle),'cases':results,
        'scope_gate':'actual separate issuer-issued life:recall and memory:recall tokens','production_access':False}),flush=True)

exercise()
