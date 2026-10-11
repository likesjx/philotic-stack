#!/usr/bin/env python3
"""Synthetic only: disposable Docker databases and actual supervised children.

No inherited service configuration, host ports, production paths or credentials.
Requires explicit compiled artifact and exact issuer source checkout arguments.
"""
import argparse, concurrent.futures, hashlib, json, os, pathlib, shutil, signal
import subprocess, tempfile, time, urllib.request, urllib.error, uuid

ISSUER_HEAD = 'bc97105557c30a17b9f966c1e1d0453c66c6f3ef'
MONGO = 'mongo@sha256:d0d926f94df099bff534b7ee5b5986458131a22489dfff8664509af0c1e2ca9c'
GRAPH = 'memgraph/memgraph@sha256:bd01a159023283b56b807943ed28225c8f662920b64317f26da7d9f0f5b19de4'
ROOT = pathlib.Path(__file__).resolve().parents[3]
HERE = pathlib.Path(__file__).resolve().parent

def run(*args, **kw):
    result = subprocess.run(args, capture_output=True, text=True, **kw)
    if result.returncode:
        raise RuntimeError('Synthetic command failed: '+result.stdout[-4000:]+result.stderr[-4000:])
    return result.stdout.strip()

def digest(path):
    return hashlib.file_digest(open(path, 'rb'), 'sha256').hexdigest()

def http(url, payload=None, bearer=None):
    headers = {'Content-Type': 'application/json'}
    if bearer: headers['Authorization'] = 'Bearer ' + bearer
    request = urllib.request.Request(url, data=json.dumps(payload).encode() if payload is not None else None, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=12) as response: return response.status, response.read().decode()
    except urllib.error.HTTPError as error: return error.code, error.read().decode()

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--approved-fixture-run', action='store_true', required=True)
parser.add_argument('--supervisor', type=pathlib.Path, required=True)
parser.add_argument('--issuer-source', type=pathlib.Path, required=True)
args = parser.parse_args()
assert os.uname().sysname == 'Linux'
supervisor = args.supervisor.resolve(strict=True)
issuer = args.issuer_source.resolve(strict=True)
assert run('git', '-C', str(issuer), 'rev-parse', 'HEAD') == ISSUER_HEAD
assert not run('git', '-C', str(issuer), 'status', '--porcelain')
node_source = pathlib.Path(shutil.which('node')).resolve(strict=True)
assert int(run(str(node_source), '-p', 'process.versions.node.split(".")[0]')) >= 24
containers, results = [], []
network = 'synthetic-lifegraph-' + uuid.uuid4().hex[:12]
with tempfile.TemporaryDirectory(prefix='synthetic-lifegraph-composed-') as temporary:
    work = pathlib.Path(temporary); work.chmod(0o700)
    node = work/'node'; shutil.copyfile(node_source, node); node.chmod(0o555)
    bundle = work/'worker.mjs'
    run(str(issuer/'node_modules/.bin/esbuild'), str(HERE/'composed-worker.mts'), '--bundle', '--platform=node', '--format=esm',
        '--alias:__ISSUER__='+str(issuer),
        '--banner:js=import {createRequire} from "node:module"; const require=createRequire(import.meta.url);', '--outfile='+str(bundle),
        env={**os.environ,'NODE_PATH':str(issuer/'node_modules')})
    bundle.chmod(0o444)
    try:
        run('docker', 'network', 'create', '--internal', network)
        for label, image in [('mongo', MONGO), ('graph', GRAPH)]:
            name = network+'-'+label
            if label == 'mongo':
                command=['mongod','--bind_ip_all','--nounixsocket','--setParameter','enableLocalhostAuthBypass=false']
                tmpfs=['--user','999:999','--entrypoint','mongod','--tmpfs','/data/db:rw,uid=999,gid=999','--tmpfs','/data/configdb:rw,uid=999,gid=999']
                command=command[1:]
            else:
                command=['--log-level=WARNING','--storage-wal-enabled=false','--storage-snapshot-interval-sec=0','--telemetry-enabled=false']
                tmpfs=['--tmpfs','/var/lib/memgraph:rw,uid=101,gid=103','--tmpfs','/var/log/memgraph:rw,uid=101,gid=103']
            containers.append(name)
            run('docker','run','-d','--name',name,'--network',network,'--cap-drop','ALL','--security-opt','no-new-privileges',
                '--memory','1g','--cpus','1','--pids-limit','128',*tmpfs,image,*command)
            inspected=json.loads(run('docker','inspect',name))[0]
            assert inspected['Config']['Image']==image and not inspected['HostConfig']['PortBindings']
            assert inspected['HostConfig']['NetworkMode']==network
        graph=containers[1]
        ready=False
        for _ in range(60):
            if subprocess.run(['docker','exec','-i',graph,'mgconsole','--host','127.0.0.1','--port','7687'],input='RETURN 1;',text=True,capture_output=True).returncode==0:
                ready=True;break
            time.sleep(.5)
        assert ready, 'synthetic graph startup failed'
        seed=(HERE.parent/'lifegraph-memgraph-tests/fixture.cypher').read_text()+"\nMATCH (n {id:'idea:synthetic'}) SET n.source_policy_manifest='[\"person:synthetic\"]';\n"
        run('docker','exec','-i',graph,'mgconsole','--host','127.0.0.1','--port','7687',input=seed)
        address=lambda name: json.loads(run('docker','inspect',name))[0]['NetworkSettings']['Networks'][network]['IPAddress']
        graph_ip, mongo_ip=address(graph),address(containers[0])
        # Only dynamically inspected addresses of these newly-created containers.
        runner_name=network+'-runner';containers.append(runner_name)
        output=run('docker','run','--rm','--name',runner_name,'--network',network,'--user',f'{os.geteuid()}:{os.getegid()}',
            '--cap-drop','ALL','--security-opt','no-new-privileges','--pids-limit','128','--memory','1g',
            '-e','LIFEGRAPH_DISPOSABLE_COMPOSED=1','-v',str(work)+':'+str(work),
            '-v',str(supervisor)+':/fixture-supervisor:ro','-v',str(HERE/'composed-exercise.py')+':/fixture-test.py:ro',
            'python@sha256:5560e9ab8709f459489e5b8aa696eda8a07ef821e14bb122be62d91234bfa98b',
            'python3','/fixture-test.py','--work',str(work),'--supervisor','/fixture-supervisor',
            '--graph-ip',graph_ip,'--mongo-ip',mongo_ip)
        print(output,flush=True)
    finally:
        for name in reversed(containers): subprocess.run(['docker','rm','-f',name],capture_output=True)
        subprocess.run(['docker','network','rm',network],capture_output=True)
