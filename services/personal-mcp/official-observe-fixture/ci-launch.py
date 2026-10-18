"""Host launcher for a disposable GitHub Linux runner, never a VPS launcher."""
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile

EXPECTED='6a862d2fa39d48fe20b849b6ced66f2673822d1677e8bfe52300f8c16dc032d4'
def call(args,**kwargs):
    return subprocess.run(args,check=True,capture_output=True,timeout=20,**kwargs)

def present(kind,name):
    args=(['docker','container','ls','--all','--filter','name=^/'+name+'$','--format','{{.ID}}']
          if kind=='container' else ['docker','image','ls','--filter','reference='+name,'--format','{{.ID}}'])
    # A failed daemon query raises; it is never treated as absence.
    return bool(call(args).stdout.strip())

def remove_owned(kind,name,owner):
    if present(kind,name):
        label=call(['docker',kind,'inspect','--format','{{index .Config.Labels "fixture.run"}}',name]).stdout.decode().strip()
        if label!=owner:raise RuntimeError('cleanup ownership mismatch')
        call(['docker',kind,'rm',*(['--force'] if kind=='container' else []),name])
    return not present(kind,name)

def main():
    if os.environ.get('GITHUB_ACTIONS')!='true' or sys.platform!='linux':raise RuntimeError('disposable CI only')
    binary=Path(sys.argv[1]).resolve();node=Path(sys.argv[2]).resolve()
    with binary.open('rb') as f:
        if binary.stat().st_size!=101319128 or hashlib.file_digest(f,'sha256').hexdigest()!=EXPECTED:
            raise RuntimeError('official artifact mismatch')
    source=Path(__file__).resolve().parent
    name='muninn-observe-'+os.environ['GITHUB_RUN_ID']+'-'+os.environ['GITHUB_RUN_ATTEMPT']
    image=name+':fixture'
    cleanup=False;container_attempted=False;image_attempted=False;receipt={}
    if present('container',name) or present('image',image):raise RuntimeError('fixture name collision')
    with tempfile.TemporaryDirectory() as directory:
        stage=Path(directory)/'fixture';stage.mkdir(mode=0o755)
        for filename in ('driver.py','ci-inner.py','gateway-check.mjs','plugin_config.json'):
            shutil.copyfile(source/filename,stage/filename)
        for filename in ('gateway.mjs','lifegraph.mjs'):
            shutil.copyfile(source.parent/filename,stage/filename)
        # Empty rootfs image assembled locally: no pull, install, registry login,
        # build secret, checkout/.git mount or host environment inside container.
        archive=io.BytesIO()
        with tarfile.open(fileobj=archive,mode='w') as tar:
            for path in ('usr','usr/bin','usr/lib','lib','lib64','runtime','fixture','data','tmp','etc'):
                member=tarfile.TarInfo(path);member.type=tarfile.DIRTYPE;member.mode=0o755;tar.addfile(member)
        try:
            image_attempted=True
            call(['docker','import','--change','LABEL fixture.run='+name,'-',image],input=archive.getvalue())
            args=['docker','create','--name',name,'--label','fixture.run='+name,'--network','none','--read-only','--user','994:994',
                  '--cap-drop','ALL','--security-opt','no-new-privileges','--memory','640m','--memory-swap','640m',
                  '--cpus','0.5','--pids-limit','128','--ulimit','nofile=1024:1024','--workdir','/data',
                  '--tmpfs','/data:rw,nosuid,nodev,size=256m,uid=994,gid=994,mode=700',
                  '--tmpfs','/tmp:rw,nosuid,nodev,size=16m,uid=994,gid=994,mode=700']
            mounts=[('/usr/bin','/usr/bin'),('/usr/lib','/usr/lib'),('/lib','/lib'),
                    (str(stage),'/fixture'),(str(binary),'/runtime/muninn'),(str(node),'/runtime/node')]
            if Path('/lib64').exists():mounts.append(('/lib64','/lib64'))
            for host,target in mounts:args+=['--mount',f'type=bind,src={host},dst={target},readonly']
            args += [image,'/usr/bin/python3','-I','-B','/fixture/ci-inner.py']
            container_attempted=True
            call(args)
            raw=call(['docker','inspect','--format','{{json .HostConfig}}',name]).stdout
            config=json.loads(raw)
            assert config['NetworkMode']=='none' and config['ReadonlyRootfs']
            assert config['Memory']==640*1024**2 and config['MemorySwap']==config['Memory']
            assert config['NanoCpus']==500000000 and config['PidsLimit']==128
            completed=subprocess.run(['docker','start','--attach',name],capture_output=True,timeout=170)
            if len(completed.stdout)>65536:raise RuntimeError('receipt limit')
            receipt=json.loads(completed.stdout)
            state=json.loads(call(['docker','inspect','--format','{{json .State}}',name]).stdout)
            receipt['container_exit']=state['ExitCode'];receipt['oom_killed']=state['OOMKilled']
            receipt['container_isolation']={'network':'none','readonly_root':True,'uid':994,'memory_bytes':config['Memory'],
                'swap_bytes':0,'cpus':0.5,'pids':128,'data_tmpfs_bytes':256*1024**2}
        except Exception as error:
            receipt['host_launcher_failure']=type(error).__name__
            receipt['operation_uncertain']=isinstance(error,subprocess.TimeoutExpired)
        finally:
            # Reconcile both attempts independently, including unknown outcomes.
            if container_attempted:
                try:cleanup=remove_owned('container',name,name)
                except Exception:receipt['container_cleanup']='unconfirmed'
            if image_attempted:
                try:receipt['image_removed']=remove_owned('image',image,name)
                except Exception:receipt['image_cleanup']='unconfirmed'
        receipt['container_removed']=cleanup
        print(json.dumps(receipt,indent=2,sort_keys=True))
        Path('official-observe-result.json').write_text(json.dumps(receipt,indent=2)+'\n')
        return 0 if (cleanup and receipt.get('image_removed') is True and receipt.get('container_exit')==0
                     and receipt.get('oom_killed') is False and not receipt.get('host_launcher_failure')) else 1

if __name__=='__main__':sys.exit(main())
