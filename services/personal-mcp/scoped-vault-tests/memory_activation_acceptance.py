#!/usr/bin/env python3
"""Real Ansible role against disposable files and a synthetic supervisor only.

No systemd socket, broker, database or native executable is contacted. The exact
staged Node/source bytes are copied and hashed; no application executes.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time


def main():
    parser=argparse.ArgumentParser();parser.add_argument('--node',required=True);args=parser.parse_args()
    assert sys.platform=='linux' and os.geteuid()==0 and Path('/.dockerenv').is_file()
    assert os.environ.get('PERCIVAL_ISOLATED_LINUX_FIXTURE')=='1'
    root=Path(__file__).resolve().parents[3]
    import ansible.module_utils
    ansible.module_utils.__path__.insert(0,str(root/'ansible/module_utils'))
    from ansible.module_utils import percival_memory_contract as c
    from ansible.module_utils import percival_memory_journal as j
    from ansible.module_utils import percival_memory_host as h
    # Pure fixture builder uses the same approval validator as unit tests.
    sys.path.insert(0,str(Path(__file__).parent))
    from memory_activation_contract_test import inputs
    node=Path(args.node);assert node.is_file() and not node.is_symlink()
    assert j.bytes_sha(node.read_bytes())==c.FILES['bin/node']
    ansible_playbook=shutil.which('ansible-playbook');assert ansible_playbook
    cases=0
    with tempfile.TemporaryDirectory(prefix='memory-activation-input-',dir='/tmp') as input_name:
        inputs_dir=Path(input_name)
        play=inputs_dir/'play.json';play.write_text(json.dumps([{'hosts':'localhost','connection':'local','gather_facts':False,'roles':[{'role':str(root/'ansible/roles/percival_personal_mcp')}]}]))
        cfg=inputs_dir/'ansible.cfg';cfg.write_text('[defaults]\nretry_files_enabled=False\ninterpreter_python=auto_silent\n')
        env={'PATH':os.environ['PATH'],'HOME':str(inputs_dir),'LANG':'C.UTF-8','ANSIBLE_CONFIG':str(cfg),
             'ANSIBLE_LIBRARY':str(root/'ansible/library'),'ANSIBLE_MODULE_UTILS':str(root/'ansible/module_utils'),
             'ANSIBLE_LOCAL_TEMP':str(inputs_dir/'local'),'ANSIBLE_REMOTE_TEMP':str(inputs_dir/'remote'),
             'PERCIVAL_ISOLATED_LINUX_FIXTURE':'1'}
        for fail_restart in (False,True):
            fixture=Path('/run/muninn-memory-activation-fixture-'+os.urandom(8).hex());fixture.mkdir(mode=0o755)
            sock=None
            try:
                def put(relative,data,mode=0o644):
                    p=fixture/relative.lstrip('/');p.parent.mkdir(parents=True,exist_ok=True)
                    p.write_bytes(data if isinstance(data,bytes) else data.encode());p.chmod(mode);return p
                m,q,before=inputs();m['approved_at']=int(time.time());m['expires_at']=m['approved_at']+1800;m['native_key_attestation'].update(verified_at=m['approved_at'],expires_at=m['approved_at']+7200)
                # Build exact fixed memory config, with no credentials or mutable native data.
                subject='15393f50-045b-4227-90b0-0cf4264f0b3d';clients=['claude_muninn_recall_v1','chatgpt_muninn_recall_v1'];broker='/run/percival-personal-vault-broker.sock'
                config={'resource':'https://mcp.jaredlikes.com/personal/mcp','port':8913,'muninnVault':'default','enabledTools':['muninn_recall'],
                    'allowedSubjects':[subject],'allowedClients':clients,'wholeVaultAccess':{'version':1,'subject':subject,'vault':'default','includeFutureRecords':True},
                    'clientPolicies':[{'clientId':x,'subjects':[subject],'vault':'default','scopes':['memory:recall']} for x in clients],
                    'issuer':{'issuer':'https://www.jaredlikes.com','discovery':'https://www.jaredlikes.com/.well-known/oauth-authorization-server','introspection':'https://www.jaredlikes.com/oauth/introspect','clientId':'percival_gateway_persistent_recall_v1','credentialBrokerSocketPath':broker},
                    'upstreams':{'muninn_recall':{'url':'http://127.0.0.1:8750/mcp','backendProtocol':'muninn-unary-json','credentialBrokerSocketPath':broker}}}
                c.memory_configuration(config);raw=c.canonical(config)+b'\n';m['config_sha256']=j.bytes_sha(raw)
                stage=c.BASE+'/staged/memory-v1-'+m['config_sha256']
                for name,sha in c.FILES.items():
                    data=node.read_bytes() if name=='bin/node' else (root/'services/personal-mcp'/name[8:]).read_bytes()
                    assert j.bytes_sha(data)==sha;put(stage+'/'+name,data,0o755 if name=='bin/node' else 0o644)
                put(stage+'/gateway.json',raw)
                for name in ('server.mjs','gateway.mjs','vault.mjs','lifegraph.mjs'):
                    put(c.BASE+'/releases/'+c.GATEWAY+'/'+name,(root/'services/personal-mcp'/name).read_bytes())
                put(h.APPLICATION,'PUBLIC EXISTING APPLICATION OVERRIDE UNCHANGED\n')
                put(h.UNIT,'PUBLIC BASE UNIT UNCHANGED\n');put(c.DISCOVERY,'{"mode":"discovery-only"}\n');put(c.POLICY,before)
                (fixture/'var/lib').mkdir(parents=True)
                put('/fixture-identities.json',json.dumps({'gateway':[1003,1003,[]],'hotel':[999,987,[]]}))
                put('/fixture-provenance.json',json.dumps({'sha256':c.HOTEL,'running':True}))
                gateway={k:'yes' for k in h.PROPS};gateway.update(User='percival-gateway',Group='percival-gateway',SupplementaryGroups='',ProtectSystem='strict',ProtectHome='yes',CapabilityBoundingSet='',AmbientCapabilities='')
                gateway.update(ActiveState='active',ExecStart='{ path=/usr/bin/node ; argv[]=/usr/bin/node '+h.BASE_ENTRY+' '+c.DISCOVERY+' ; ignore_errors=no ; }',WorkingDirectory=c.BASE+'/releases/'+c.GATEWAY,DropInPaths=h.APPLICATION)
                hotel={k:'' for k in h.PROPS};hotel.update(User='philotic',Group='philotic',SupplementaryGroups='',Requires='percival-personal-vault.socket',After='percival-personal-vault.socket',TriggeredBy='percival-personal-vault.socket')
                values={'percival-personal-mcp.service':gateway,'philotic-hotel.service':hotel,'percival-personal-vault.socket':{'ActiveState':'active','Triggers':'philotic-hotel.service'}}
                put('/fixture-supervisor.json',json.dumps(values))
                put('/fixture-running-gateway.json',json.dumps({'executable':'/usr/bin/node'}))
                put('/events.json','[]')
                if fail_restart:put('/fail-restart','once')
                sockpath=fixture/h.SOCKET.lstrip('/');sockpath.parent.mkdir(parents=True,exist_ok=True)
                sock=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);sock.bind(str(sockpath));os.chown(sockpath,1003,1003);os.chmod(sockpath,0o600)
                stub=r'''import json,sys
from pathlib import Path
root=Path(__file__).parent
step=sys.argv[1]
assert step in ('stop_gateway','reload','start_socket','restart_hotel','start_gateway','start_discovery')
def get(name):return json.loads((root/name).read_text())
def put(name,value):(root/name).write_text(json.dumps(value))
events=get('events.json');events.append(step);put('events.json',events)
v=get('fixture-supervisor.json');g=v['percival-personal-mcp.service']
if step=='stop_gateway':g['ActiveState']='inactive'
if step=='reload':
 p=root/'etc/systemd/system/percival-personal-mcp.service.d/99-muninn-memory.conf'
 if p.exists():
  text=p.read_text().splitlines();argv=[x[10:] for x in text if x.startswith('ExecStart=') and len(x)>10][0]
  node=argv.split()[0];g['ExecStart']='{ path='+node+' ; argv[]='+argv+' ; ignore_errors=no ; }'
  g['WorkingDirectory']=[x[17:] for x in text if x.startswith('WorkingDirectory=')][0]
  g['DropInPaths']='/etc/systemd/system/percival-personal-mcp.service.d/95-application-709c0d7e.conf /etc/systemd/system/percival-personal-mcp.service.d/99-muninn-memory.conf'
 else:
  g.update(get('fixture-original-gateway.json'))
  g['ActiveState']='inactive'
if step=='restart_hotel':
 marker=root/'fail-restart'
 if marker.exists():
  marker.unlink();p=get('fixture-provenance.json');p['running']=False;put('fixture-provenance.json',p);sys.exit(1)
 p=get('fixture-provenance.json');p['running']=True;put('fixture-provenance.json',p)
if step in ('start_gateway','start_discovery'):
 g['ActiveState']='active'
 node=g['ExecStart'].split('path=')[1].split(' ;')[0]
 put('fixture-running-gateway.json',{'executable':node})
put('fixture-supervisor.json',v)
'''
                put('/systemctl_fixture.py',stub);put('/fixture-original-gateway.json',json.dumps(gateway))
                host=h.Host(str(fixture))
                m['baseline'].update(gateway_unit_sha256=j.bytes_sha(host.read(h.UNIT)),discovery_sha256=j.bytes_sha(host.read(c.DISCOVERY)),supervisor_sha256=host.supervisor(),application_override_sha256=j.bytes_sha(host.read(h.APPLICATION)))
                approval=c.digest(m)
                def run(label,success,manifest=m,receipt=q,profile='memory-activate-v1',extra=None):
                    nonlocal cases
                    variables={'percival_enabled':True,'percival_profile':profile,'percival_memory_manifest':manifest,'percival_memory_qualification':receipt,'percival_memory_approval_sha256':c.digest(manifest),'percival_memory_fixture_root':str(fixture)}
                    variables.update(extra or {});varsfile=inputs_dir/'vars.json';varsfile.write_text(json.dumps(variables))
                    result=subprocess.run([ansible_playbook,'-i','localhost,',str(play),'--extra-vars','@'+str(varsfile)],env=env,cwd=inputs_dir,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True,timeout=240)
                    if (result.returncode==0)!=success:
                        print(result.stdout[-18000:]);raise AssertionError(label)
                    cases+=1;print('PASS '+label,flush=True)
                    assert j.bytes_sha(host.read(h.UNIT))==m['baseline']['gateway_unit_sha256']
                    assert j.bytes_sha(host.read(c.DISCOVERY))==m['baseline']['discovery_sha256']
                if not fail_restart:
                    run('default off does not prepare or change services',True,extra={'percival_enabled':False})
                    run('missing same-binary qualification refuses before mutation',False,receipt={})
                    bad=json.loads(json.dumps(m));bad['owner_attestations']['admissions_disabled']=False
                    run('missing live-owner gate refuses',False,manifest=bad)
                    bad=json.loads(json.dumps(m));bad['expires_at']=bad['approved_at']
                    run('expired approval refuses',False,manifest=bad)
                    assert json.loads(host.read('/events.json'))==[]
                run('uncertain hotel failure closes gateway' if fail_restart else 'exact owned activation',not fail_restart)
                if fail_restart:
                    assert json.loads(host.read('/events.json'))[-1]=='stop_gateway'
                    assert host.properties('percival-personal-mcp.service',('ActiveState',))['ActiveState']=='inactive'
                else:
                    assert host.read(c.POLICY)==c.canonical(m['policy'])+b'\n'
                    assert host.read(c.OVERRIDE)==c.validate(m,q,approval)['override'].encode()
                    before_events=host.read('/events.json')
                    run('activation replay refuses without service retry',False)
                    assert host.read('/events.json')==before_events
                recovery=json.loads(json.dumps(m));recovery['approved_at']=int(time.time());recovery['expires_at']=recovery['approved_at']+1800
                transaction_dir,latest,_=host.load(m['transaction'])
                incomplete=transaction_dir/('.muninn-publish-'+'f'*32+'.tmp')
                incomplete.write_bytes(b'{incomplete publication');incomplete.chmod(0o600)
                assert host.load(m['transaction'])[1]==latest
                recovery['recovery']={'activation_sha256':approval,'commands_quiescent':True,'previous_record_sha256':c.digest(latest),'previous_approval_sha256':None}
                if not fail_restart:
                    original_override=host.read(c.OVERRIDE)
                    put(c.OVERRIDE,'FOREIGN OVERRIDE MUST NOT BE REMOVED\n')
                    events_before=host.read('/events.json')
                    run('foreign override recovery refuses without removal',False,manifest=recovery,profile='memory-recover-v1')
                    assert host.read(c.OVERRIDE)==b'FOREIGN OVERRIDE MUST NOT BE REMOVED\n' and host.read('/events.json')==events_before
                    put(c.OVERRIDE,original_override)
                    baseline_server=c.BASE+'/releases/'+c.GATEWAY+'/server.mjs'
                    original_server=host.read(baseline_server);put(baseline_server,b'UNAPPROVED DISCOVERY SOURCE')
                    run('discovery source drift blocks restart',False,manifest=recovery,profile='memory-recover-v1')
                    assert host.read('/events.json')==events_before
                    put(baseline_server,original_server)
                    # Damaged new candidate must not block restoring approved discovery.
                    (fixture/(stage+'/gateway/whole-vault.mjs').lstrip('/')).unlink()
                    bad=json.loads(json.dumps(recovery));bad['recovery']['commands_quiescent']=False
                    run('recovery requires fresh explicit quiescence',False,manifest=bad,profile='memory-recover-v1')
                if fail_restart:
                    put('/fail-restart','once')
                    run('interrupted recovery preserves pending checkpoint and closes gateway',False,manifest=recovery,profile='memory-recover-v1')
                    previous_approval=c.digest(recovery)
                    _,latest,_=host.load(m['transaction'])
                    recovery['approved_at']=int(time.time());recovery['expires_at']=recovery['approved_at']+1800
                    recovery['recovery'].update(previous_record_sha256=c.digest(latest),previous_approval_sha256=previous_approval)
                run('fresh authorized recovery resumes interrupted recovery' if fail_restart else 'explicit recovery restores discovery only',True,manifest=recovery,profile='memory-recover-v1')
                assert host.read(c.POLICY)==before and not (fixture/c.OVERRIDE.lstrip('/')).exists()
                assert host.properties('percival-personal-mcp.service',('ActiveState',))['ActiveState']=='active'
                assert json.loads(host.read('/fixture-running-gateway.json'))=={'executable':'/usr/bin/node'}
            finally:
                if sock:sock.close()
                shutil.rmtree(fixture)
    print(str(cases)+' real Ansible activation/recovery fixture cases passed; no live or native execution.')

if __name__=='__main__':main()
