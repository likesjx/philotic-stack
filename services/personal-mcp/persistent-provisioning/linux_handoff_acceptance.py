#!/usr/bin/env python3
"""Public sibling-process adversary. Only on a disposable CI Linux runner as root."""
import base64,hashlib,json,os,pathlib,pwd,select,shutil,signal,subprocess,sys,tempfile,time,array,fcntl,termios
HERE=pathlib.Path(__file__).resolve().parent
ROOT=pathlib.Path('/opt/percival-personal-mcp/provisioning')
ATTACK=r'''
import ctypes,json,os,sys
v=json.loads(sys.argv[1]);pid=v['pid'];result={}
for label,path in [('proc_root',f"/proc/{pid}/root{v['home']}"+("" if v.get("transition") else "/.philotic/vault-master-key.env")),('proc_fd',f"/proc/{pid}/fd/{v['key_fd']}"),('proc_mem',f'/proc/{pid}/mem')]:
 try:
  if label=='proc_fd':os.readlink(path)
  else:
   fd=os.open(path,os.O_RDONLY)
   if label!='proc_mem' and not v.get('transition'):os.read(fd,1)
   os.close(fd)
  result[label]={'allowed':True,'errno':0}
 except OSError as error:result[label]={'allowed':False,'errno':error.errno}
libc=ctypes.CDLL(None,use_errno=True)
success=libc.ptrace(16,pid,0,0)==0
result['ptrace']={'allowed':success,'errno':0 if success else ctypes.get_errno()}
if success:
 os.waitpid(pid,0);libc.ptrace(17,pid,0,0)
print(json.dumps(result))
'''
def identity():
 os.setgroups([]);os.setgid(pwd.getpwuid(999).pw_gid);os.setuid(999)
def attack(pid,home,fd,transition=False):
 out=subprocess.check_output(['/usr/bin/python3','-c',ATTACK,json.dumps({'pid':pid,'home':str(home),'key_fd':fd,'transition':transition})],preexec_fn=identity,timeout=5)
 return json.loads(out)
def ready(proc):
 assert select.select([proc.stdout],[],[],10)[0],'readiness timeout'
 return json.loads(proc.stdout.readline())
def main():
 if sys.argv[1:]!=['--disposable-public-fixture']:
  print('{"mode":"plan","live_changes":false}');return
 assert sys.platform=='linux' and os.geteuid()==0 and os.environ.get('GITHUB_ACTIONS')=='true'
 observed_policy=pathlib.Path('/proc/sys/fs/suid_dumpable').read_text().strip()
 print(json.dumps({'observed_suid_dumpable':observed_policy,'kernel':os.uname().release}),file=sys.stderr)
 assert observed_policy=='2','fixture requires existing policy2; no sysctl mutation permitted'
 assert not ROOT.exists() and not list(pathlib.Path('/run').glob('persistent-key-*'))
 binary=HERE.parent/'synthetic-broker-build/broker-helper/target/x86_64-unknown-linux-gnu/release/examples/private_start_fixture'
 assert binary.read_bytes()[:6]==b'\x7fELF\x02\x01'
 created_user=False;control=None;work=None;proc=None
 try:
  try:pwd.getpwuid(999)
  except KeyError:
   subprocess.run(['useradd','--system','--uid','999','--no-create-home','--shell','/usr/sbin/nologin','synthetic-broker-fixture'],check=True);created_user=True
  work=pathlib.Path(tempfile.mkdtemp(prefix='public-proc-control-',dir='/run'));os.chown(work,999,pwd.getpwuid(999).pw_gid)
  control_code="import os,pathlib,json,sys,time;h=pathlib.Path(sys.argv[1]);(h/'.philotic').mkdir();p=h/'.philotic/vault-master-key.env';p.write_text('public fixture');f=p.open();print(json.dumps({'pid':os.getpid(),'key_fd':f.fileno()}),flush=True);time.sleep(60)"
  control=subprocess.Popen(['/usr/bin/python3','-c',control_code,str(work)],preexec_fn=identity,stdout=subprocess.PIPE)
  c=ready(control);positive=attack(c['pid'],work,c['key_fd'])
  assert positive['proc_root']['allowed'] and positive['proc_fd']['allowed'],'missing same-UID positive controls'
  control.terminate();control.wait();control=None
  ROOT.mkdir(mode=0o755,parents=True);helper=ROOT/'persistent-broker-helper';shutil.copyfile(binary,helper);helper.chmod(0o755)
  for timeout in [False,True]:
   r,w=os.pipe();os.write(w,base64.b64encode(bytes([7])*32)+b'\n');os.close(w);observer=os.dup(r)
   proc=subprocess.Popen(['/usr/bin/python3',str(HERE/'private_key_handoff.py'),'--approved-private-handoff','--key-fd',str(r)],pass_fds=(r,),stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
   os.close(r);proc.stdin.write(json.dumps({'timeout':timeout}).encode());proc.stdin.close();proc.stdin=None
   transition=ready(proc);assert transition['transition_ready'] and transition['uid']==999 and transition['dumpability']==2
   paths=list(pathlib.Path('/run').glob('persistent-key-*'));assert len(paths)==1
   def stopped():
    return any(line.startswith('State:') and line.split()[1]=='T' for line in pathlib.Path(f"/proc/{transition['pid']}/status").read_text().splitlines())
   deadline=time.monotonic()+2
   while not stopped() and time.monotonic()<deadline:time.sleep(0.01)
   assert stopped(),'transition must be stopped before probing'
   denied_transition=attack(transition['pid'],paths[0],transition['key_fd'],True)
   assert stopped(),'transition unexpectedly resumed'
   assert all(not v['allowed'] and v['errno'] in (1,13) for v in denied_transition.values()),denied_transition
   unread=array.array('i',[0]);fcntl.ioctl(observer,termios.FIONREAD,unread,True);os.close(observer)
   assert unread[0]==45,'operator key consumed before protected readiness'
   assert stopped()
   os.kill(transition['pid'],signal.SIGCONT)
   state=ready(proc)
   home=paths[0];assert home.stat().st_uid==0 and home.stat().st_mode&0o777==0o700 and list(home.iterdir())==[]
   os.kill(state['pid'],0)
   denied=attack(state['pid'],home,state['key_fd'])
   os.kill(state['pid'],0)
   assert all(not v['allowed'] and v['errno'] in (1,13) for v in denied.values()),denied
   out,err=proc.communicate(timeout=27)
   assert b'BwcHBwc' not in out+err and not list(pathlib.Path('/run').glob('persistent-key-*'))
   assert proc.returncode==(1 if timeout else 0)
   proc=None
  assert pathlib.Path('/proc/sys/fs/suid_dumpable').read_text().strip()==observed_policy,'global policy changed'
  print(json.dumps({'private_key_handoff_accepted':True,'kernel_suid_dumpable':2,'transition_same_uid_denied':True,'key_withheld_until_protected':True,'same_uid_proc_denied':True,'public_fixture_only':True,'host_namespace_hidden':True,'noswap_verified':True,'uid_verified':True,'timeout_cleanup':True,'positive_controls':positive,'adapter_sha256':hashlib.sha256((HERE/'private_key_handoff.py').read_bytes()).hexdigest(),'bootstrap_sha256':hashlib.sha256((HERE.parent/'synthetic-broker-build/broker-helper/src/private_start.rs').read_bytes()).hexdigest(),'helper_sha256':hashlib.sha256((binary).read_bytes()).hexdigest(),'main_sha256':hashlib.sha256((HERE.parent/'synthetic-broker-build/broker-helper/src/persistent_main.rs').read_bytes()).hexdigest()}))
 finally:
  if control is not None:control.kill();control.wait()
  if proc is not None and proc.poll() is None:
   # Adapter owns a 20s child deadline; preserve it long enough to complete cleanup.
   try:proc.wait(timeout=25)
   except subprocess.TimeoutExpired:proc.kill();proc.wait()
  if (ROOT/'persistent-broker-helper').exists():(ROOT/'persistent-broker-helper').unlink()
  if ROOT.exists():ROOT.rmdir()
  if work is not None:shutil.rmtree(work)
  if created_user:subprocess.run(['userdel','synthetic-broker-fixture'],check=True)
if __name__=='__main__':main()
