"""Disposable offline container entrypoint. No host credentials or data mounted."""
import datetime
import http.cookiejar
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
import urllib.request
sys.path.insert(0, '/fixture')
from driver import Client, NoRedirect, run, verify_binary

def main():
    if os.getuid()!=994 or set(os.listdir('/data')):
        raise RuntimeError('fresh unprivileged fixture required')
    digest=verify_binary('/runtime/muninn')
    for forbidden in ['/home/runner','/run/docker.sock','/root/.ssh','/github','/proc/1/root/home/runner']:
        if Path(forbidden).exists(): raise RuntimeError('unexpected host mount')
    Path('/data/plugin_config.json').write_bytes(Path('/fixture/plugin_config.json').read_bytes())
    password='PUBLIC-SYNTHETIC-ONLY-exact-observe-v1-7381'
    env={'PATH':'/usr/bin:/bin','HOME':'/data','MUNINN_ADMIN_PASSWORD':password,
         'MUNINN_LOCAL_EMBED':'0','MUNINN_CLUSTER_ENABLED':'false','MUNINN_MEM_LIMIT_GB':'1',
         'MUNINN_GC_PERCENT':'50','MUNINN_ACCESS_LOG':'0'}
    argv=['/runtime/muninn','--daemon','--data=/data','--listen-host=127.0.0.1',
          '--mbp-addr=127.0.0.1:18474','--mcp-addr=127.0.0.1:18475',
          '--rest-addr=127.0.0.1:18476','--grpc-addr=127.0.0.1:18477',
          '--ui-addr=127.0.0.1:18478','--metrics-addr=','--backup-interval=','--log-level=error']
    backend=subprocess.Popen(argv,env=env,stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    receipt={'binary_sha256':digest,'synthetic_only':True}
    phase='readiness'
    try:
        signal.signal(signal.SIGALRM,lambda *_: (_ for _ in ()).throw(TimeoutError()))
        signal.alarm(140)
        reader=urllib.request.build_opener(urllib.request.ProxyHandler({}),NoRedirect())
        end=time.monotonic()+30
        while True:
            if backend.poll() is not None: raise RuntimeError('backend exited')
            try:
                with reader.open('http://127.0.0.1:18476/api/ready',timeout=1) as r:
                    if r.status==200 and json.loads(r.read(1024)).get('status')=='ready':break
            except Exception: pass
            if time.monotonic()>end: raise RuntimeError('readiness timeout')
            time.sleep(.1)
        jar=http.cookiejar.CookieJar()
        phase='synthetic_admin_login'
        admin=urllib.request.build_opener(urllib.request.ProxyHandler({}),NoRedirect(),urllib.request.HTTPCookieProcessor(jar))
        req=urllib.request.Request('http://127.0.0.1:18478/api/auth/login',
            data=json.dumps({'username':'root','password':password}).encode(),
            headers={'Content-Type':'application/json'},method='POST')
        with admin.open(req,timeout=5) as r:
            if r.status!=200 or len(r.read(4097))>4096: raise RuntimeError('login failed')
        deadline=time.monotonic()+100
        client=Client(admin,deadline)
        def gateway(token,vault,marker):
            p=subprocess.run(['/runtime/node','/fixture/gateway-check.mjs','/fixture/gateway.mjs'],
                input=json.dumps({'token':token,'vault':vault,'marker':marker}),
                capture_output=True,text=True,timeout=12,env={'PATH':'/usr/bin:/bin'})
            return 'pass' if p.returncode==0 and json.loads(p.stdout).get('actualGatewayAdapter')=='pass' else 'fail'
        def expiry_check(token,expiry,args):
            while datetime.datetime.now(datetime.timezone.utc)<=expiry+datetime.timedelta(seconds=1):
                if time.monotonic()>=deadline:return 'fail: deadline'
                time.sleep(.1)
            status,_=client.rpc(token,'muninn_recall',args)
            return 'pass' if status in (401,403) else 'fail'
        expiry=(datetime.datetime.now(datetime.timezone.utc)+datetime.timedelta(seconds=30)).isoformat()
        phase='observe_driver'
        receipt=run(client,digest,expiry,gateway_check=gateway,expiry_check=expiry_check)
        jar.clear()
    except Exception as error:
        # Exception type only; never backend logs, responses or authentication data.
        receipt['launcher_failure']=type(error).__name__
        receipt['launcher_phase']=phase
    finally:
        signal.alarm(0)
        backend.terminate()
        try: backend.wait(timeout=5)
        except subprocess.TimeoutExpired: backend.kill();backend.wait(timeout=2)
        receipt['backend_stopped']=backend.poll() is not None
    print(json.dumps(receipt,sort_keys=True))
    ignored={'all_cognitive_storage_writes_suppressed','observed_record_response_unchanged',
             'mcp_query_cross_vault_explicit_denial'}
    cases=receipt.get('cases',{})
    required={'mcp_observe_own_vault_recall','mcp_body_cross_vault_denied','mcp_query_no_foreign_content',
              'mcp_direct_foreign_id_denied','mcp_write_rejected','mcp_delete_rejected',
              'rest_query_cross_vault_denied','rest_body_cross_vault_denied','rest_direct_foreign_id_denied',
              'rest_write_rejected','denied_writes_did_not_add_records','actual_gateway_adapter',
              'test_key_expiry_metadata_bounded','expired_key_rejected'}
    return 0 if (required<=cases.keys() and all(v=='pass' for k,v in cases.items() if k not in ignored)
                 and receipt.get('test_keys_revoked') is True and receipt['backend_stopped']
                 and 'setup_or_transport' not in receipt and 'launcher_failure' not in receipt) else 1

if __name__=='__main__': sys.exit(main())
