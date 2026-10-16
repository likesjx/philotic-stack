#!/usr/bin/env python3
"""Prepared only. One exact-unit run on a separately approved ephemeral CI VM."""
import hashlib,json,os,pathlib,subprocess,sys,tarfile,time
HERE=pathlib.Path(__file__).resolve().parent
BASE=pathlib.Path('/var/lib/muninn-broker-dummy-e43bb385-r3')
UNIT='muninn-broker-dummy-e43bb385-r3.service'
SLICE='muninn-synthetic-run4.slice'
UNITPATH=pathlib.Path('/run/systemd/system')/UNIT
SLICEPATH=pathlib.Path('/run/systemd/system')/SLICE
ARCHIVE_SHA='15d427d6fff8c4b6ac615c98e4f2672eb455e5a0663d005a384a63096811f3a0'
MANIFEST_SHA='94d15a963307033789cf0386432b7943a5311434faee0cffd1d4bf1826fbe65e'
UNIT_SHA='95b7cf6b27cb0e8930f144f95d84c5fe10da5b095318a604bae14a2b8a735ac7'
SLICE_DATA=b'[Slice]\nMemoryMax=640M\nMemorySwapMax=0\nCPUQuota=50%\nTasksMax=128\n'
def run(*args,check=True):return subprocess.run(args,capture_output=True,text=True,check=check,timeout=20)
def show(name,*props):
    args=['systemctl','show',name]
    for p in props:args+=['-p',p]
    return dict(s.split('=',1) for s in run(*args).stdout.splitlines() if '=' in s)
def digest(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def cgroup_path(value):
    assert value.startswith('/') and '..' not in pathlib.PurePosixPath(value).parts
    return pathlib.Path('/sys/fs/cgroup')/value.lstrip('/')
def cgroup_empty(path):
    if path is None or not path.exists():return True
    events=dict(line.split() for line in (path/'cgroup.events').read_text().splitlines())
    return events['populated']=='0' # Includes descendants, unlike cgroup.procs.
def stage():
    archive=HERE/'vps-broker-dummy-e43bb385-r3.tar.gz'
    assert digest(archive)==ARCHIVE_SHA
    BASE.mkdir(mode=0o700)
    with tarfile.open(archive) as t:
        ms=t.getmembers();assert len(ms)==24 and len(set(t.getnames()))==24
        raw=t.extractfile('manifest.json').read();assert hashlib.sha256(raw).hexdigest()==MANIFEST_SHA
        manifest=json.loads(raw);assert set(t.getnames())==set(manifest)|{'manifest.json'}
        for m in ms:
            assert not pathlib.PurePosixPath(m.name).is_absolute() and '..' not in pathlib.PurePosixPath(m.name).parts
            assert m.uid==m.gid==0 and (m.isfile() or m.isdir() or m.issym())
            if m.name=='manifest.json':continue
            e=manifest[m.name];assert m.mode==int(e['mode'],8)
            if m.isfile():assert hashlib.sha256(t.extractfile(m).read()).hexdigest()==e['sha256']
            if m.issym():assert m.linkname==e['target'] and m.name in ['root/bin','root/sbin','root/lib','root/lib64']
        for m in ms:
            p=BASE/m.name
            if m.isdir():p.mkdir(mode=m.mode)
            elif m.issym():p.symlink_to(m.linkname)
            else:
                with p.open('xb') as f:f.write(t.extractfile(m).read())
                p.chmod(m.mode)
        for name,e in manifest.items():
            p=BASE/name;assert p.lstat().st_uid==0
            if e['type']=='file':assert digest(p)==e['sha256']
def cleanup(owned_unit,owned_slice,cg,report):
    errors=[]
    def attempt(label,fn):
        try:return True,fn()
        except Exception as e:
            errors.append({'operation':label,'type':type(e).__name__,'detail':str(e)[:2000]})
            return False,None
    started=report.get('run_count',0)>0
    if owned_unit and started and cg is None:
        def resolve_cgroup():
            value=show(UNIT,'ControlGroup')['ControlGroup']
            if value:return cgroup_path(value)
            value=show(SLICE,'ControlGroup')['ControlGroup']
            assert value,'attempted run cgroup path unavailable; preserve files'
            return cgroup_path(value)/UNIT
        _,cg=attempt('resolve-attempted-run-cgroup',resolve_cgroup)
    report['cgroup_path_verified']=not started or cg is not None
    if cg is not None:report['actual_cgroup_path']=str(cg)
    # Stop attempts always run, independently of diagnostics/evidence failures.
    if owned_unit:attempt('stop-unit',lambda:run('systemctl','stop',UNIT))
    # CI-created slice only. Never stop the VPS's pre-existing shared slice.
    if owned_slice:attempt('stop-owned-slice',lambda:run('systemctl','stop',SLICE))
    def unit_evidence():
        s=show(UNIT,'MainPID','ControlGroup','ActiveState')
        assert s['MainPID']=='0' and s['ControlGroup']=='' and s['ActiveState'] in ['inactive','failed']
        assert not started or cg is not None,'attempted run cgroup evidence incomplete'
        assert cgroup_empty(cg)
        mounts=[l for l in pathlib.Path('/proc/self/mountinfo').read_text().splitlines() if str(BASE) in l or 'synthetic-key-' in l]
        assert not mounts;report['residual_owned_mounts']=mounts
        runtime=BASE/'root/run';report['retained_runtime_entries']=sorted(p.name for p in runtime.iterdir())
        assert not list(runtime.glob('synthetic-key-*'))
        assert set(report['retained_runtime_entries'])<={'host','mount','systemd'}
    def remove_unit():
        assert digest(UNITPATH)==UNIT_SHA;UNITPATH.unlink()
    def remove_slice():
        s=show(SLICE,'ActiveState','ControlGroup')
        assert s['ActiveState'] in ['inactive','failed'] and s['ControlGroup']==''
        assert not started or cg is not None,'attempted run cgroup evidence incomplete'
        assert cgroup_empty(cg)
        assert SLICEPATH.read_bytes()==SLICE_DATA;SLICEPATH.unlink()
    if owned_unit:
        safe,_=attempt('verify-unit-cleanup',unit_evidence)
        if safe:attempt('remove-owned-unit',remove_unit)
    if owned_slice:attempt('remove-owned-slice',remove_slice)
    attempt('daemon-reload',lambda:run('systemctl','daemon-reload'))
    if owned_unit:
        attempt('reset-failed',lambda:run('systemctl','reset-failed',UNIT,check=False))
        ok,journal=attempt('capture-journal',lambda:run('journalctl','-u',UNIT,'-n','80','--no-pager','-o','cat').stdout)
        if ok:report['journal']=journal
    def check_policy():
        assert pathlib.Path('/proc/sys/fs/suid_dumpable').read_text().strip()=='2'
        assert pathlib.Path('/proc/sys/kernel/yama/ptrace_scope').read_text().strip()=='1'
    attempt('verify-unchanged-policy',check_policy)
    report['cleanup_errors']=errors
    report['cleanup_passed']=not errors
def main():
    if sys.argv[1:]!=['--approved-disposable-linux']:
        print('{"plan":true,"live_changes":false}');return
    # No production/VPS execution entry point. No credential or broad env reads.
    assert os.geteuid()==0 and sys.platform=='linux'
    assert os.environ.get('GITHUB_ACTIONS')=='true' and os.environ.get('RUNNER_ENVIRONMENT')=='github-hosted'
    assert pathlib.Path('/proc/1/comm').read_text().strip()=='systemd'
    assert os.uname().machine=='x86_64'
    assert run('systemctl','--version').stdout.splitlines()[0].split()[1]=='255'
    assert pathlib.Path('/proc/sys/fs/suid_dumpable').read_text().strip()=='2'
    assert pathlib.Path('/proc/sys/kernel/yama/ptrace_scope').read_text().strip()=='1'
    assert not any(os.path.lexists(p) for p in [BASE,UNITPATH,SLICEPATH])
    for name in [UNIT,SLICE]:assert show(name,'LoadState')['LoadState']=='not-found'
    report={'archive_sha256':ARCHIVE_SHA,'run_count':0,'runtime_passed':False,'cleanup_passed':False}
    owned_unit=False;owned_slice=False;cg=None
    try:
        stage()
        with SLICEPATH.open('xb') as f:f.write(SLICE_DATA)
        owned_slice=True;SLICEPATH.chmod(0o644)
        assert digest(BASE/UNIT)==UNIT_SHA
        with UNITPATH.open('xb') as f:f.write((BASE/UNIT).read_bytes())
        owned_unit=True;UNITPATH.chmod(0o644)
        run('systemctl','daemon-reload')
        effective=show(UNIT,'MemoryMax','MemorySwapMax','CPUQuotaPerSecUSec','TasksMax','RuntimeMaxUSec','TimeoutStopUSec','PrivateNetwork','Restart','RootDirectory','DropInPaths')
        assert effective=={'MemoryMax':'134217728','MemorySwapMax':'0','CPUQuotaPerSecUSec':'250ms','TasksMax':'32','RuntimeMaxUSec':'2min','TimeoutStopUSec':'15s','PrivateNetwork':'yes','Restart':'no','RootDirectory':str(BASE/'root'),'DropInPaths':''}
        report['effective_limits']=effective
        slice_limits=show(SLICE,'MemoryMax','MemorySwapMax','CPUQuotaPerSecUSec','TasksMax','DropInPaths')
        assert slice_limits=={'MemoryMax':'671088640','MemorySwapMax':'0','CPUQuotaPerSecUSec':'500ms','TasksMax':'128','DropInPaths':''}
        report['effective_slice_limits']=slice_limits
        report['run_count']=1
        run('systemctl','start',UNIT) # Exactly one start, no retry.
        slice_cg=show(SLICE,'ControlGroup')['ControlGroup']
        unit_cg=show(UNIT,'ControlGroup')['ControlGroup']
        cg=cgroup_path(unit_cg) if unit_cg else cgroup_path(slice_cg)/UNIT
        report['actual_cgroup_path']=str(cg)
        deadline=time.monotonic()+140
        while True:
            status=show(UNIT,'ActiveState','Result','MainPID','ExecMainStatus','ControlGroup')
            if status['ActiveState'] in ('inactive','failed'):break
            assert time.monotonic()<deadline,'supervisor deadline'
            time.sleep(1)
        report['unit_result']=status
        assert status['Result']=='success' and status['ExecMainStatus']=='0' and status['MainPID']=='0'
        receipt=json.loads((BASE/'root/results/receipt.json').read_text())
        report['receipt']=receipt
        for key in ['uid999_sqlite_regression','dummy_create_readback_delete','private_mount_cleanup','sysctl_unchanged']:assert receipt[key] is True
        assert all(v['allowed'] for v in receipt['positive_controls'].values())
        assert all(not v['allowed'] and v['errno'] in [1,13] for v in receipt['protected_denials'].values())
        assert receipt['production_activation'] is False and receipt['transition_window_retested'] is False
        report['runtime_passed']=True
    except Exception as e:
        report['error']={'type':type(e).__name__,'detail':str(e)[:2000]}
    finally:
        cleanup(owned_unit,owned_slice,cg,report)
        # Export only nonsecret dummy results; preserve the root tree until VM disposal.
        out=HERE/'rehearsal-result.json'
        with out.open('x') as f:json.dump(report,f,indent=2);f.write('\n')
        out.chmod(0o644)
    print(json.dumps({'runtime_passed':report['runtime_passed'],'cleanup_passed':report['cleanup_passed']}))
    if not (report['runtime_passed'] and report['cleanup_passed']):raise SystemExit(1)
if __name__=='__main__':main()
