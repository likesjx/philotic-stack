#!/usr/bin/env python3
"""Synthetic only: disposable Docker databases and actual supervised children.

No inherited service configuration, host ports, production paths or credentials.
Builds the supervisor from an exact clean source commit; never accepts a cached binary.
"""
import argparse, concurrent.futures, hashlib, json, os, pathlib, shutil, signal
import subprocess, tempfile, time, urllib.request, urllib.error, uuid

ISSUER_HEAD = 'bc97105557c30a17b9f966c1e1d0453c66c6f3ef'
MONGO = 'mongo@sha256:d0d926f94df099bff534b7ee5b5986458131a22489dfff8664509af0c1e2ca9c'
GRAPH = 'memgraph/memgraph@sha256:bd01a159023283b56b807943ed28225c8f662920b64317f26da7d9f0f5b19de4'
RUST = 'rust@sha256:8e0f5926ce10ec99e12703b315c0043e4709d6bdc740dc1d28753e5fb87c766f'
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
parser.add_argument('--source-head', required=True)
parser.add_argument('--cargo-registry', type=pathlib.Path, required=True, help='Existing public dependency registry cache; no Cargo credentials/config loaded')
parser.add_argument('--issuer-source', type=pathlib.Path, required=True)
args = parser.parse_args()
assert os.uname().sysname == 'Linux'
source_head = run('git','-C',str(ROOT),'rev-parse','HEAD')
assert source_head == args.source_head and len(source_head)==40
assert not run('git','-C',str(ROOT),'status','--porcelain')
issuer = args.issuer_source.resolve(strict=True)
assert run('git', '-C', str(issuer), 'rev-parse', 'HEAD') == ISSUER_HEAD
assert not run('git', '-C', str(issuer), 'status', '--porcelain')
node_source = pathlib.Path(shutil.which('node')).resolve(strict=True)
assert int(run(str(node_source), '-p', 'process.versions.node.split(".")[0]')) >= 24
containers, results = [], []
network = 'synthetic-lifegraph-' + uuid.uuid4().hex[:12]
with tempfile.TemporaryDirectory(prefix='synthetic-lifegraph-composed-') as temporary:
    work = pathlib.Path(temporary); work.chmod(0o700)
    source=work/'source';source.mkdir()
    # Export the exact Git object rather than mutable working-tree bytes.
    archive=work/'source.tar'
    with archive.open('wb') as output:
        subprocess.run(['git','-C',str(ROOT),'archive',source_head],stdout=output,check=True)
    run('tar','-xf',str(archive),'-C',str(source));archive.unlink()
    fixture=source/'services/personal-mcp/lifegraph-owner-tests'
    run('docker','run','--rm','--network','none','--cpus','2','--memory','3g',
        '-v',str(source)+':/src:ro','-v',str(work)+':/build',
        '-v',str(args.cargo_registry.resolve(strict=True))+':/cargo/registry:ro',
        '-e','CARGO_HOME=/cargo','-e','CARGO_TARGET_DIR=/build/target','-e','CARGO_BUILD_JOBS=2','-e','CARGO_INCREMENTAL=0',
        '-e','CARGO_PROFILE_DEV_DEBUG=0','-e','RUSTC=/usr/local/rustup/toolchains/1.94.0-x86_64-unknown-linux-gnu/bin/rustc',
        '-w','/src',RUST,'/usr/local/rustup/toolchains/1.94.0-x86_64-unknown-linux-gnu/bin/cargo',
        'build','--offline','--locked','-p','data-memorygraphrag','--bin','lifegraph-read-supervisor')
    supervisor=work/'target/debug/lifegraph-read-supervisor'
    assert supervisor.is_file()
    node = work/'node'; shutil.copyfile(node_source, node); node.chmod(0o555)
    bundle = work/'worker.mjs'
    run(str(issuer/'node_modules/.bin/esbuild'), str(fixture/'composed-worker.mts'), '--bundle', '--platform=node', '--format=esm',
        '--alias:__ISSUER__='+str(issuer),
        '--banner:js=import {createRequire} from "node:module"; const require=createRequire(import.meta.url);', '--outfile='+str(bundle),
        env={**os.environ,'NODE_PATH':str(issuer/'node_modules')})
    bundle.chmod(0o444)
    assert run('git','-C',str(issuer),'rev-parse','HEAD') == ISSUER_HEAD
    assert not run('git','-C',str(issuer),'status','--porcelain')
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
        seed=(fixture.parent/'lifegraph-memgraph-tests/fixture.cypher').read_text()+"\nMATCH (n {id:'idea:synthetic'}) SET n.source_policy_manifest='[\"person:synthetic\"]';\n"
        run('docker','exec','-i',graph,'mgconsole','--host','127.0.0.1','--port','7687',input=seed)
        address=lambda name: json.loads(run('docker','inspect',name))[0]['NetworkSettings']['Networks'][network]['IPAddress']
        graph_ip, mongo_ip=address(graph),address(containers[0])
        # Only dynamically inspected addresses of these newly-created containers.
        runner_name=network+'-runner';containers.append(runner_name)
        output=run('docker','run','--rm','--name',runner_name,'--network',network,'--user',f'{os.geteuid()}:{os.getegid()}',
            '--cap-drop','ALL','--security-opt','no-new-privileges','--pids-limit','128','--memory','1g',
            '-e','LIFEGRAPH_DISPOSABLE_COMPOSED=1','-v',str(work)+':'+str(work),
            '-v',str(supervisor)+':/fixture-supervisor:ro','-v',str(fixture/'composed-exercise.py')+':/fixture-test.py:ro',
            'python@sha256:5560e9ab8709f459489e5b8aa696eda8a07ef821e14bb122be62d91234bfa98b',
            'python3','/fixture-test.py','--work',str(work),'--supervisor','/fixture-supervisor',
            '--graph-ip',graph_ip,'--mongo-ip',mongo_ip)
        receipt=json.loads(output.splitlines()[-1])
        receipt.update(source_head=source_head,supervisor_built_from_exact_commit=True,rust_image=RUST)
        print(output,flush=True);print(json.dumps(receipt),flush=True)
    finally:
        for name in reversed(containers): subprocess.run(['docker','rm','-f',name],capture_output=True)
        subprocess.run(['docker','network','rm',network],capture_output=True)
