#!/usr/bin/env python3
"""Root-exec private handoff candidate; real use requires Linux acceptance. Never discovers keys."""
import argparse,base64,json,os,pathlib,pwd,resource,signal,stat,subprocess,sys,tempfile
HELPER='/opt/muninn-claude-synthetic/broker-helper-linux-amd64'
def key_line(fd):
    if not stat.S_ISFIFO(os.fstat(fd).st_mode):raise ValueError('pipe required')
    raw=os.read(fd,129)
    # The operator writes one canonical base64 value plus optional final newline, then closes.
    raw=raw.removesuffix(b'\n')
    if len(raw)!=44 or base64.b64encode(base64.b64decode(raw,validate=True))!=raw or len(base64.b64decode(raw))!=32:
        raise ValueError('invalid private input')
    if os.read(fd,1):raise ValueError('excess private input')
    return b'PHILOTIC_VAULT_MASTER_KEY='+raw+b'\n'
def child(directory,fd):
    resource.setrlimit(resource.RLIMIT_CORE,(0,0))
    if pathlib.Path("/proc/sys/fs/suid_dumpable").read_text().strip()!="0":raise ValueError("unsafe kernel dump policy")
    subprocess.run(['/usr/bin/mount','-t','tmpfs','-o','mode=0700,nosuid,nodev,noexec,noswap,size=64k','tmpfs',directory],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    # Only this mount namespace sees the mounted content. The host sees an empty root0700 dir.
    gid=pwd.getpwuid(999).pw_gid
    home=pathlib.Path(directory);os.chown(home,999,gid)
    conf=home/'.philotic';conf.mkdir(mode=0o700);os.chown(conf,999,gid)
    line=key_line(fd);os.close(fd)
    out=os.open(conf/'vault-master-key.env',os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o400)
    try:
        os.fchown(out,999,gid)
        if os.write(out,line)!=len(line):raise OSError('short write')
    finally:os.close(out)
    del line
    # Remain root through exec; Rust drops privilege without any later exec.
    os.execve(HELPER,[HELPER,'--apply-scoped-synthetic','--root-private-start'],{'HOME':directory,'PATH':'/usr/bin:/bin','PHILOTIC_VAULT_KEYCHAIN':'0'})
def main():
    p=argparse.ArgumentParser();p.add_argument('--approved-private-handoff',action='store_true');p.add_argument('--key-fd',type=int);p.add_argument('--child');a=p.parse_args()
    if not a.approved_private_handoff:
        print(json.dumps({'mode':'plan','live_changes':False,'integrated':False}));return
    if os.geteuid()!=0 or sys.platform!='linux' or a.key_fd is None or a.key_fd<3:raise ValueError('scope')
    if a.child:
        # Internal child mode is never a standalone operator entry point.
        root=pathlib.Path(a.child)
        if root.parent!=pathlib.Path('/run') or not root.name.startswith('synthetic-key-'):raise ValueError('path')
        st=root.lstat()
        if not stat.S_ISDIR(st.st_mode) or st.st_uid!=0 or stat.S_IMODE(st.st_mode)!=0o700:raise ValueError('ownership')
        child(str(root),a.key_fd);return
    st=os.lstat(HELPER)
    if not stat.S_ISREG(st.st_mode) or st.st_uid!=0 or st.st_mode&0o022:raise ValueError('helper ownership')
    directory=tempfile.mkdtemp(prefix='synthetic-key-',dir='/run');proc=None
    try:
        proc=subprocess.Popen(['/usr/bin/unshare','--mount','--propagation','private','/usr/bin/python3',str(pathlib.Path(__file__).resolve()),'--approved-private-handoff','--child',directory,'--key-fd',str(a.key_fd)],pass_fds=(a.key_fd,),start_new_session=True,env={'PATH':'/usr/bin:/bin'},stderr=subprocess.DEVNULL)
        os.close(a.key_fd)
        if proc.wait(timeout=20)!=0:raise RuntimeError('private operation failed')
    finally:
        if proc is not None and proc.poll() is None:os.killpg(proc.pid,signal.SIGKILL);proc.wait()
        # Never recurse or delete unexpected files. Namespace exit destroys the tmpfs.
        os.rmdir(directory)
if __name__=='__main__':
    try:main()
    except Exception:
        print('private handoff stopped; reconcile journal',file=sys.stderr);sys.exit(1)
