"""Fixed host checks and public journal persistence; never controls a service."""
import json
import fcntl
import inspect
import tempfile
import contextlib
import os
from pathlib import Path
import pwd
import re
import stat
import subprocess
from ansible.module_utils import percival_memory_contract as c
from ansible.module_utils import percival_memory_journal as j
from ansible.module_utils.percival_provenance import systemd_properties, verify, artifact

JOURNALS='/var/lib/percival-memory-transactions'
UNIT='/etc/systemd/system/percival-personal-mcp.service'
APPLICATION='/etc/systemd/system/percival-personal-mcp.service.d/95-application-709c0d7e.conf'
HOTEL='/opt/philotic/bin/aiua'
SOCKET='/run/percival-personal-vault-broker.sock'
BASE_ENTRY=c.BASE+'/releases/'+c.GATEWAY+'/server.mjs'
PROPS=('User','Group','SupplementaryGroups','NoNewPrivileges','ProtectSystem','ProtectHome','PrivateTmp','PrivateDevices',
       'ProtectKernelTunables','ProtectKernelModules','ProtectControlGroups','RestrictSUIDSGID','CapabilityBoundingSet','AmbientCapabilities')


class Host:
    def __init__(self, fixture=''):
        self.fixture=fixture
        if fixture:
            c.require(os.geteuid()==0 and Path('/.dockerenv').is_file() and os.environ.get('PERCIVAL_ISOLATED_LINUX_FIXTURE')=='1' and
                      re.fullmatch('/run/muninn-memory-activation-fixture-[a-f0-9]{16}',fixture))
        else:
            import socket
            c.require(os.geteuid()==0 and socket.gethostname()=='srv875272')

    def path(self,path): return self.fixture+path
    def read(self,path,limit=1048576,private=False): return j.read_regular(self.path(path),limit,private)

    def properties(self,unit,properties):
        if self.fixture:
            all_values=json.loads(self.read('/fixture-supervisor.json'))[unit]
            return {k:all_values.get(k,'') for k in properties}
        p=subprocess.run(['/usr/bin/systemctl','show',unit,'--no-pager','--property='+','.join(properties)],stdin=subprocess.DEVNULL,
                         stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,env={'PATH':'/usr/bin:/bin','LANG':'C'},timeout=10)
        c.require(p.returncode==0 and len(p.stdout)<65536)
        result={}
        for line in p.stdout.decode().splitlines():
            key,sep,value=line.partition('=');c.require(sep and key in properties and key not in result);result[key]=value
        c.require(set(result)==set(properties));return result

    def provenance(self,running=True):
        if self.fixture:
            p=json.loads(self.read('/fixture-provenance.json'))
            c.exact(p,'sha256 running');c.require(p['sha256']==c.HOTEL and (p['running'] is True or not running));return
        before=systemd_properties();verify(before,HOTEL,c.HOTEL,running)
        c.require(systemd_properties()==before)

    def identities(self, allow_missing_socket=False):
        if self.fixture:
            values=json.loads(self.read('/fixture-identities.json'))
            c.require(values=={'gateway':[1003,1003,[]],'hotel':[999,987,[]]})
        else:
            for name,uid,gid in [('percival-gateway',1003,1003),('philotic',999,987)]:
                p=pwd.getpwnam(name);c.require(p.pw_uid==uid and p.pw_gid==gid and p.pw_shell in ('/usr/sbin/nologin','/sbin/nologin','/bin/false'))
                c.require(set(os.getgrouplist(name,gid))=={gid})
        relation=self.properties('philotic-hotel.service',('Requires','After','TriggeredBy'))
        c.require(all('percival-personal-vault.socket' in relation[k].split() for k in relation))
        c.require('philotic-hotel.service' in self.properties('percival-personal-vault.socket',('Triggers',))['Triggers'].split())
        try: info=os.lstat(self.path(SOCKET))
        except FileNotFoundError:
            c.require(allow_missing_socket);return None
        c.require(stat.S_ISSOCK(info.st_mode) and info.st_uid==1003 and info.st_gid==1003 and stat.S_IMODE(info.st_mode)==0o600)
        return (info.st_dev,info.st_ino,info.st_uid,info.st_gid,info.st_mode)

    def supervisor(self):
        values={u:self.properties(u,PROPS) for u in ('percival-personal-mcp.service','philotic-hotel.service')}
        g=values['percival-personal-mcp.service']
        c.require(g['User']=='percival-gateway' and g['Group']=='percival-gateway' and g['SupplementaryGroups']=='')
        c.require(g['NoNewPrivileges']=='yes' and g['ProtectSystem']=='strict' and g['ProtectHome']=='yes')
        c.require(all(g[k]=='yes' for k in ('PrivateTmp','PrivateDevices','ProtectKernelTunables','ProtectKernelModules','ProtectControlGroups','RestrictSUIDSGID')))
        c.require(g['CapabilityBoundingSet']==g['AmbientCapabilities']=='')
        h=values['philotic-hotel.service'];c.require(h['User']=='philotic' and h['Group']=='philotic' and h['SupplementaryGroups']=='')
        return c.digest(values)

    def stage(self,manifest):
        stage=c.BASE+'/staged/memory-v1-'+manifest['config_sha256']
        for name,digest in c.FILES.items():
            data=self.read(stage+'/'+name,256*1024*1024 if name=='bin/node' else 2*1024*1024)
            c.require(j.bytes_sha(data)==digest)
            info=os.lstat(self.path(stage+'/'+name));c.require(info.st_gid==0 and stat.S_IMODE(info.st_mode)==(0o755 if name=='bin/node' else 0o644))
        raw=self.read(stage+'/gateway.json');c.require(j.bytes_sha(raw)==manifest['config_sha256']);c.memory_configuration(json.loads(raw))
        for directory in (stage,stage+'/gateway',stage+'/bin'):
            info=os.lstat(self.path(directory));c.require(stat.S_ISDIR(info.st_mode) and info.st_uid==info.st_gid==0 and stat.S_IMODE(info.st_mode)==0o755)
        config_info=os.lstat(self.path(stage+'/gateway.json'));c.require(config_info.st_gid==0 and stat.S_IMODE(config_info.st_mode)==0o644)
        expected={'gateway','bin','gateway.json',*c.FILES}
        root=Path(self.path(stage))
        c.require({str(p.relative_to(root)) for p in root.rglob('*')}==expected)

    def unchanged(self,manifest,record=None,running=True,recovery=False,allow_missing_socket=False):
        if not recovery: self.stage(manifest)
        self.provenance(running);before=self.identities(allow_missing_socket)
        c.require(j.bytes_sha(self.read(APPLICATION))==manifest['baseline']['application_override_sha256'])
        for name in ('server.mjs','gateway.mjs','vault.mjs','lifegraph.mjs'):
            c.require(j.bytes_sha(self.read(c.BASE+'/releases/'+c.GATEWAY+'/'+name))==c.FILES['gateway/'+name])
        self.gateway_command(manifest,record)
        c.require(j.bytes_sha(self.read(UNIT))==manifest['baseline']['gateway_unit_sha256'])
        raw=self.read(c.DISCOVERY);c.require(j.bytes_sha(raw)==manifest['baseline']['discovery_sha256'])
        discovery=json.loads(raw);c.require(type(discovery) is dict and discovery.get('mode')=='discovery-only')
        c.require(self.supervisor()==manifest['baseline']['supervisor_sha256'])
        raw=self.read(c.POLICY);c.policy(json.loads(raw))
        allowed={manifest['baseline']['policy_sha256']}
        if record: allowed.add(j.bytes_sha(c.canonical(manifest['policy'])+b'\n'))
        c.require(j.bytes_sha(raw) in allowed)
        override=Path(self.path(c.OVERRIDE))
        for parent in override.parents:
            if not os.path.lexists(parent):
                c.require(parent==override.parent);continue
            info=parent.lstat();c.require(stat.S_ISDIR(info.st_mode) and info.st_uid==0 and not info.st_mode&0o022)
        if os.path.lexists(override):
            c.require(record is not None)
            expected=c.validate(record['activation'],record['qualification'],record['activation_sha256'],record['activation']['approved_at'])['override'].encode()
            c.require(self.read(c.OVERRIDE)==expected)
        c.require(self.identities(allow_missing_socket)==before)

    def gateway_command(self,manifest,record):
        props=self.properties('percival-personal-mcp.service',('ExecStart','WorkingDirectory','DropInPaths'))
        stage=c.BASE+'/staged/memory-v1-'+manifest['config_sha256']
        baseline=('/usr/bin/node',BASE_ENTRY,c.DISCOVERY,c.BASE+'/releases/'+c.GATEWAY)
        target=(stage+'/bin/node',stage+'/gateway/server.mjs',stage+'/gateway.json',stage+'/gateway')
        match=re.fullmatch(r'\{ path=([^ ;{}]+) ; argv\[\]=([^ ;{}]+) ([^ ;{}]+) ([^ ;{}]+) ; [^{}]*\}',props['ExecStart'])
        c.require(match is not None and match[1]==match[2])
        actual=(match[1],match[3],match[4],props['WorkingDirectory'])
        # Only the dedicated override is allowed; no unreviewed drop-in can
        # override argv, isolation, environment or write paths.
        drops=props['DropInPaths'].split()
        c.require(drops in ([APPLICATION],[APPLICATION,c.OVERRIDE]))
        c.require(actual==baseline or (record is not None and actual==target))
        if actual==target: c.require(drops==[APPLICATION,c.OVERRIDE])

    def running_gateway(self,manifest,memory):
        expected=c.BASE+'/staged/memory-v1-'+manifest['config_sha256']+'/bin/node' if memory else '/usr/bin/node'
        if self.fixture:
            c.require(json.loads(self.read('/fixture-running-gateway.json'))=={'executable':expected})
            return
        pid=self.properties('percival-personal-mcp.service',('MainPID',))['MainPID']
        c.require(re.fullmatch('[1-9][0-9]{0,8}',pid))
        actual=artifact('/proc/'+pid+'/exe');wanted=artifact(expected)
        c.require(actual==wanted)
        c.require(wanted[1]==(c.FILES['bin/node'] if memory else manifest['baseline']['discovery_node_sha256']))

    def transition_files(self,record,mode,step,status):
        original=record['activation']
        target=c.canonical(original['policy'])+b'\n'
        current=self.read(c.POLICY)
        override=Path(self.path(c.OVERRIDE))
        expected=c.validate(original,record['qualification'],record['activation_sha256'],original['approved_at'])['override'].encode()
        if step in ('write_policy','restore_policy'):
            if status=='done': c.require(current==(target if step=='write_policy' else record['baseline_policy'].encode()))
            elif mode=='activate': c.require(current==record['baseline_policy'].encode())
        if step=='write_override':
            if status=='pending': c.require(not os.path.lexists(override))
            else: c.require(self.read(c.OVERRIDE)==expected)
        if step=='remove_override' and status=='done': c.require(not os.path.lexists(override))
        if mode=='activate' and step in ('reload','start_socket','restart_hotel','start_gateway'):
            c.require(current==target and self.read(c.OVERRIDE)==expected)
        if mode=='recover' and step in ('reload','start_socket','restart_hotel','start_discovery'):
            c.require(current==record['baseline_policy'].encode() and not os.path.lexists(override))

    def quiescent(self):
        for unit in ('percival-personal-mcp.service','philotic-hotel.service','percival-personal-vault.socket'):
            c.require(self.properties(unit,('Job',))['Job'] in ('','0'))

    @contextlib.contextmanager
    def locked(self):
        path=self.path(JOURNALS+'/owner.json')
        fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW)
        try:
            fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
            yield
        finally:
            os.close(fd)

    def mutation(self,manifest,qualification,approved_sha,mode,step):
        c.require(step in ('write_policy','write_override','restore_policy','remove_override'))
        with self.locked():
            directory,record,index=self.load(manifest['transaction'])
            c.validate(manifest,qualification,approved_sha,recovery=mode=='recover')
            c.require(approved_sha==(record['activation_sha256'] if mode=='activate' else record['recovery']['sha256']))
            c.require(j.inspect(record)['pending']==(mode,step))
            owner=json.loads(self.read(JOURNALS+'/owner.json',private=True))
            c.require(owner=={'transaction':manifest['transaction'],'activation_sha256':record['activation_sha256']})
            self.unchanged(record['activation'],record,running=mode!='recover',recovery=mode=='recover',allow_missing_socket=mode=='recover')
            self.quiescent();self.transition_files(record,mode,step,'pending')
            if step in ('write_policy','restore_policy'):
                path=Path(self.path(c.POLICY));before=self.read(c.POLICY);identity=path.lstat()
                after=(c.canonical(manifest['policy'])+b'\n') if step=='write_policy' else record['baseline_policy'].encode()
                fd,tmp=tempfile.mkstemp(prefix='.muninn-owned-',dir=path.parent)
                try:
                    os.fchmod(fd,0o644);c.require(os.write(fd,after)==len(after));os.fsync(fd);os.close(fd);fd=None
                    current=path.lstat();c.require((current.st_dev,current.st_ino)==(identity.st_dev,identity.st_ino) and self.read(c.POLICY)==before)
                    os.replace(tmp,path)
                finally:
                    if fd is not None:os.close(fd)
                    if os.path.exists(tmp):os.unlink(tmp)
            else:
                path=Path(self.path(c.OVERRIDE))
                if step=='write_override':
                    if not path.parent.exists():path.parent.mkdir(mode=0o755)
                    content=c.validate(record['activation'],record['qualification'],record['activation_sha256'],record['activation']['approved_at'])['override'].encode()
                    j.atomic_publish(path,content,0o644)
                elif os.path.lexists(path):
                    identity=path.lstat();before=self.read(c.OVERRIDE)
                    expected=c.validate(record['activation'],record['qualification'],record['activation_sha256'],record['activation']['approved_at'])['override'].encode()
                    c.require(before==expected)
                    current=path.lstat();c.require((current.st_dev,current.st_ino)==(identity.st_dev,identity.st_ino) and self.read(c.OVERRIDE)==before)
                    os.unlink(path)
            parent=Path(self.path(c.POLICY if 'policy' in step else c.OVERRIDE)).parent
            fd=os.open(parent,os.O_RDONLY|os.O_DIRECTORY)
            try:os.fsync(fd)
            finally:os.close(fd)
            self.transition_files(record,mode,step,'done')
            return {'mutated_owned_file':True}

    def directory(self,path):
        target=Path(self.path(path))
        for parent in target.parents:
            p=parent.lstat();c.require(stat.S_ISDIR(p.st_mode) and p.st_uid==0 and not p.st_mode&0o022)
        if not target.exists(): target.mkdir(mode=0o700)
        info=target.lstat();c.require(stat.S_ISDIR(info.st_mode) and info.st_uid==0 and stat.S_IMODE(info.st_mode)==0o700)
        # Traversal-safe read of ancestors using a newly written marker is not needed:
        # directory ancestry is independently checked before creating any record.
        for parent in target.parents:
            p=parent.lstat();c.require(stat.S_ISDIR(p.st_mode) and p.st_uid==0 and not p.st_mode&0o022)
        return target

    def load(self,transaction):
        root=self.directory(JOURNALS);directory=root/transaction
        info=directory.lstat();c.require(stat.S_ISDIR(info.st_mode) and info.st_uid==0 and stat.S_IMODE(info.st_mode)==0o700)
        names=sorted(p.name for p in directory.iterdir() if not j.is_publication_temporary(p))
        c.require(names and names==[f'{i:03d}.json' for i in range(len(names))] and len(names)<=170)
        previous=None
        for name in names:
            current=json.loads(j.read_regular(directory/name,private=True));j.inspect(current)
            if previous is not None:
                c.require(current['activation']==previous['activation'] and current['qualification']==previous['qualification'] and current['baseline_policy']==previous['baseline_policy'])
                if current['recovery']!=previous['recovery']:
                    c.require(current['recovery']['manifest']['recovery']['previous_record_sha256']==c.digest(previous))
                    c.require(current['recovery']['manifest']['recovery']['previous_approval_sha256']==(previous['recovery']['sha256'] if previous['recovery'] else None))
                    c.require(current['history']==previous['history']+[{'mode':'authorization','step':'begin_recovery','status':'authorized'}])
                else:
                    c.require(current['history'][:-1]==previous['history'] and len(current['history'])==len(previous['history'])+1)
            previous=current
        return directory,previous,len(names)

    def prepare(self,manifest,qualification,approved_sha):
        output=c.validate(manifest,qualification,approved_sha)
        self.unchanged(manifest)
        self.quiescent()
        root=self.directory(JOURNALS)
        directory=root/manifest['transaction'];directory.mkdir(mode=0o700)
        # Exclusive ownership: another activation may not prepare while one exists.
        lock=root/'owner.json'
        j.write_new(lock,{'transaction':manifest['transaction'],'activation_sha256':approved_sha})
        self.unchanged(manifest)
        record=j.initial(manifest,qualification,approved_sha,self.read(c.POLICY))
        j.write_new(directory/'000.json',record)
        return output

    def checkpoint(self,manifest,qualification,approved_sha,mode,step,status):
        c.validate(manifest,qualification,approved_sha,recovery=mode=='recover')
        directory,record,index=self.load(manifest['transaction'])
        owner=json.loads(self.read(JOURNALS+'/owner.json',private=True))
        c.require(owner=={'transaction':manifest['transaction'],'activation_sha256':record['activation_sha256']})
        if mode=='activate': c.require(record['activation_sha256']==approved_sha and record['recovery'] is None)
        else: c.require(record['recovery'] and record['recovery']['sha256']==approved_sha)
        self.unchanged(record['activation'],record,running=not (mode=='recover' and (step in ('stop_gateway','restore_policy','remove_override','reload','start_socket') or step=='restart_hotel' and status=='pending')), recovery=mode=='recover',allow_missing_socket=mode=='recover' and (step in ('stop_gateway','restore_policy','remove_override','reload') or step=='start_socket' and status=='pending'))
        self.quiescent()
        self.transition_files(record,mode,step,status)
        if status=='done' and step in ('start_gateway','start_discovery'):
            state=self.properties('percival-personal-mcp.service',('ActiveState',))
            c.require(state['ActiveState']=='active')
        if status=='done' and step=='start_socket':
            c.require(self.properties('percival-personal-vault.socket',('ActiveState',))['ActiveState']=='active')
        if step=='start_discovery':
            if not self.fixture: c.require(j.bytes_sha(self.read('/usr/bin/node',256*1024*1024))==manifest['baseline']['discovery_node_sha256'])
        if status=='done' and step=='stop_gateway':
            c.require(self.properties('percival-personal-mcp.service',('ActiveState',))['ActiveState']=='inactive')
        if status=='done' and step in ('start_gateway','start_discovery'):
            self.running_gateway(manifest,step=='start_gateway')
        next_record=j.advance(record,mode,step,status)
        j.write_new(directory/f'{index:03d}.json',next_record)
        return {'checked':True}

    def recover(self,manifest,qualification,approved_sha):
        c.validate(manifest,qualification,approved_sha,recovery=True)
        directory,record,index=self.load(manifest['transaction'])
        c.require(record['qualification']==qualification)
        owner=json.loads(self.read(JOURNALS+'/owner.json',private=True))
        c.require(owner=={'transaction':manifest['transaction'],'activation_sha256':record['activation_sha256']})
        self.unchanged(record['activation'],record,running=False,recovery=True,allow_missing_socket=True)
        self.quiescent()
        next_record=j.begin_recovery(record,manifest,approved_sha)
        j.write_new(directory/f'{index:03d}.json',next_record)
        return {'baseline_policy':record['baseline_policy']}
