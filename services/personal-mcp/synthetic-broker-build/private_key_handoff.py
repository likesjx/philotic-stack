#!/usr/bin/env python3
"""Root-exec private handoff candidate; real use requires Linux acceptance. Never discovers keys."""
import argparse,base64,json,os,pathlib,pwd,resource,signal,stat,subprocess,sys,tempfile,socket,select,time
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
    if pathlib.Path("/proc/sys/fs/suid_dumpable").read_text().strip() not in ("0","2"):raise ValueError("unsafe kernel dump policy")
    subprocess.run(['/usr/bin/mount','-t','tmpfs','-o','mode=0700,nosuid,nodev,noexec,noswap,size=64k','tmpfs',directory],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    # Only this mount namespace sees the mounted content. The host sees an empty root0700 dir.
    gid=pwd.getpwuid(999).pw_gid
    home=pathlib.Path(directory);os.chown(home,999,gid)
    conf=home/'.philotic';conf.mkdir(mode=0o700);os.chown(conf,999,gid)
    # Empty socket only: the operator key FD is never inherited by this process.
    os.set_inheritable(fd,True)
    os.execve(HELPER,[HELPER,'--apply-scoped-synthetic','--root-private-start',str(fd)],{'HOME':directory,'PATH':'/usr/bin:/bin','PHILOTIC_VAULT_KEYCHAIN':'0'})
def deliver_after_ready(channel,key_fd):
    channel.settimeout(10)
    ready=b''
    while len(ready)<6:
        chunk=channel.recv(6-len(ready))
        if not chunk:raise ValueError('protection handshake absent')
        ready+=chunk
    if ready!=b'READY\n':raise ValueError('protection handshake invalid')
    # No read of the operator key until the protected helper acknowledges readiness.
    if not select.select([key_fd],[],[],2)[0]:raise ValueError('private input timeout')
    os.set_blocking(key_fd,False)
    channel.sendall(key_line(key_fd));channel.shutdown(socket.SHUT_WR)
def main():
    p=argparse.ArgumentParser();p.add_argument('--approved-private-handoff',action='store_true');p.add_argument('--key-fd',type=int);p.add_argument('--child');p.add_argument('--channel-fd',type=int);a=p.parse_args()
    if not a.approved_private_handoff:
        print(json.dumps({'mode':'plan','live_changes':False,'integrated':False}));return
    if os.geteuid()!=0 or sys.platform!='linux':raise ValueError('scope')
    if a.child:
        # Internal child mode is never a standalone operator entry point.
        root=pathlib.Path(a.child)
        if root.parent!=pathlib.Path('/run') or not root.name.startswith('synthetic-key-'):raise ValueError('path')
        st=root.lstat()
        if not stat.S_ISDIR(st.st_mode) or st.st_uid!=0 or stat.S_IMODE(st.st_mode)!=0o700:raise ValueError('ownership')
        if a.channel_fd is None or a.channel_fd<3 or a.key_fd is not None:raise ValueError('channel scope')
        child(str(root),a.channel_fd);return
    if a.key_fd is None or a.key_fd<3 or a.channel_fd is not None:raise ValueError('key scope')
    resource.setrlimit(resource.RLIMIT_CORE,(0,0))
    st=os.lstat(HELPER)
    if not stat.S_ISREG(st.st_mode) or st.st_uid!=0 or st.st_mode&0o022:raise ValueError('helper ownership')
    parent,peer=socket.socketpair()
    directory=tempfile.mkdtemp(prefix='synthetic-key-',dir='/run');proc=None
    try:
        proc=subprocess.Popen(['/usr/bin/unshare','--mount','--propagation','private','/usr/bin/python3',str(pathlib.Path(__file__).resolve()),'--approved-private-handoff','--child',directory,'--channel-fd',str(peer.fileno())],pass_fds=(peer.fileno(),),start_new_session=True,env={'PATH':'/usr/bin:/bin'},stderr=subprocess.DEVNULL)
        peer.close()
        deliver_after_ready(parent,a.key_fd)
        os.close(a.key_fd);a.key_fd=None
        if proc.wait(timeout=20)!=0:raise RuntimeError('private operation failed')
    finally:
        if proc is not None and proc.poll() is None:os.killpg(proc.pid,signal.SIGKILL);proc.wait()
        if a.key_fd is not None:os.close(a.key_fd)
        parent.close();peer.close()
        # Never recurse or delete unexpected files. Namespace exit destroys the tmpfs.
        os.rmdir(directory)
if __name__=='__main__':
    try:main()
    except Exception:
        print('private handoff stopped; reconcile journal',file=sys.stderr);sys.exit(1)
