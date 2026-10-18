"""Real local HTTP/cookie wire tests; enabled only on disposable CI."""
import datetime,http.server,json,os,threading,unittest
from adapters import API
@unittest.skipUnless(os.environ.get('RUN_PRIVATE_HTTP_TESTS')=='1','disposable CI HTTP fixture only')
class Wire(unittest.TestCase):
 def test_actual_login_cookie_admin_key_shape_and_logout(self):
  seen=[]
  class Handler(http.server.BaseHTTPRequestHandler):
   def log_message(self,*args):pass
   def do_GET(self):self.handle_request()
   def do_POST(self):self.handle_request()
   def handle_request(self):
    data=json.loads(self.rfile.read(int(self.headers.get('Content-Length','0')))) if self.command=='POST' else None
    seen.append((self.command,self.path,data,self.headers.get('Cookie')))
    result={}
    if self.path=='/api/auth/login':
     assert data=={'username':'root','password':'PUBLIC_FIXTURE_ONLY'}
     self.send_response(200);self.send_header('Set-Cookie','muninn_session=public-fixture; Path=/; HttpOnly')
    else:
     assert self.headers.get('Cookie')=='muninn_session=public-fixture'
     self.send_response(201 if self.path=='/api/admin/keys' else 200)
     if self.path=='/api/vaults':result=['fixture_vault']
     if self.path=='/api/admin/keys':
      assert set(data)=={'vault','mode','label','expires'} and data['mode']=='observe'
      result={'token':'mk_'+'s'*40,'key':{'id':'fixture-key','vault':data['vault'],'mode':'observe','expires_at':data['expires']}}
    raw=json.dumps(result).encode();self.send_header('Content-Length',str(len(raw)));self.end_headers();self.wfile.write(raw)
  servers=[http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler) for _ in range(2)]
  threads=[threading.Thread(target=s.serve_forever,daemon=True) for s in servers]
  for t in threads:t.start()
  try:
   api=API(servers[0].server_port,servers[1].server_port);api.login('root','PUBLIC_FIXTURE_ONLY')
   self.assertEqual(api.request('GET','/api/vaults',admin=True),(200,['fixture_vault']))
   status,body=api.request('POST','/api/admin/keys',{'vault':'fixture_vault','mode':'observe','label':'fixture','expires':'2027-01-01T00:00:00Z'},admin=True)
   self.assertEqual(status,201);self.assertEqual(body['key']['mode'],'observe');self.assertIn('expires_at',body['key'])
   api.close_private_session();self.assertEqual(len(api.cookies),0)
   self.assertEqual([s[1] for s in seen],['/api/auth/login','/api/vaults','/api/admin/keys','/api/auth/logout'])
  finally:
   for s in servers:s.shutdown();s.server_close()
   for t in threads:t.join()
if __name__=='__main__':unittest.main()
