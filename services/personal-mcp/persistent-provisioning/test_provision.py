import datetime,json,tempfile,unittest,os
from pathlib import Path
from provision import Journal,execute,plan,digest,CLIENTS,RESOURCE,SUBJECT,Refused
SECRET='mk_'+'s'*40
class API:
    def __init__(self,m,unknown=False):self.m=m;self.calls=[];self.unknown=unknown;self.closed=False
    def request(self,method,path,body=None,admin=False):
        self.calls.append((method,path,body))
        if method=='GET':return 200,[self.m['vault']]
        if self.unknown:raise TimeoutError(SECRET)
        return 201,{'token':SECRET,'key':{'id':'fixture-id','vault':self.m['vault'],'mode':'observe','expires_at':self.m['expires_at']}}
    def close_private_session(self):self.closed=True
class Broker:
    def __init__(self,fail=False):self.calls=[];self.fail=fail
    def store_exact_acl(self,**kw):
        self.calls.append(kw)
        if self.fail:raise TimeoutError(SECRET)
        return {'reference':'secret://hotel/default/'+kw['kind']+'/'+'a'*32}
class Tests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.path=Path(self.tmp.name)/'journal.json';self.j=Journal(self.path)
        self.m={'version':1,'resource':RESOURCE,'subject':SUBJECT,'vault':'fixture_vault','include_future_records':True,
            'clients':CLIENTS[:],'expires_at':(datetime.datetime.now(datetime.timezone.utc)+datetime.timedelta(days=1)).isoformat()}
    def tearDown(self):self.j.close();self.tmp.cleanup()
    def confirm(self,d):return 'CREATE_AND_STORE '+d
    def test_unset_scope_fails_before_handoff(self):
        for k in ['vault','include_future_records','expires_at']:
            m={**self.m,k:None}
            with self.assertRaises(Exception):execute(m,self.j,self.confirm,lambda:self.fail('handoff'))
    def test_confirmation_precedes_private_handoff(self):
        with self.assertRaises(Refused):execute(self.m,self.j,lambda d:'no',lambda:self.fail('handoff'))
        self.assertFalse(self.path.exists())
    def test_only_observe_and_exact_acl_no_memory_reads_or_activation(self):
        api=API(self.m);broker=Broker();out=execute(self.m,self.j,self.confirm,lambda:(api,broker))
        self.assertEqual(out['outcome'],'stored_not_activated');self.assertTrue(api.closed)
        self.assertEqual([(x[0],x[1]) for x in api.calls],[('GET','/api/vaults'),('POST','/api/admin/keys')])
        self.assertEqual(api.calls[1][2]['mode'],'observe')
        self.assertEqual(len(broker.calls),2)
        self.assertTrue(all(x['allowed_roles']==['percival-personal-recall'] and x['allowed_guests']==['percival-personal-gateway'] for x in broker.calls))
        self.assertNotIn(SECRET,self.path.read_text());self.assertNotIn(broker.calls[1]['secret'],self.path.read_text())
        self.assertNotIn('verifier',json.dumps(out))
        with self.assertRaises(Refused):execute(self.m,self.j,self.confirm,lambda:self.fail('repeat'))
    def test_unknown_create_is_pending_and_never_repeated(self):
        api=API(self.m,True);out=execute(self.m,self.j,self.confirm,lambda:(api,Broker()))
        self.assertEqual(out['outcome'],'recovery_required');self.assertEqual(self.j.data['steps']['create_observe'],'pending')
        self.assertNotIn(SECRET,self.path.read_text())
        with self.assertRaises(Refused):execute(self.m,self.j,self.confirm,lambda:self.fail('retry'))
    def test_store_failure_retains_known_key_for_recovery(self):
        out=execute(self.m,self.j,self.confirm,lambda:(API(self.m),Broker(True)))
        self.assertEqual(out['outcome'],'recovery_required');self.assertEqual(self.j.data['key_ids']['observe'],'fixture-id')
    def test_mutating_original_manifest_cannot_change_confirmed_scope(self):
        api=API(self.m);broker=Broker()
        def confirm(d):self.m['vault']='other';return self.confirm(d)
        # API returns the now changed vault; contract validation must reject it.
        out=execute(self.m,self.j,confirm,lambda:(api,broker))
        self.assertEqual(out['outcome'],'recovery_required');self.assertFalse(broker.calls)
    def test_plan_does_not_create_journal_or_call_services(self):
        self.assertFalse(plan(self.m)['live_changes']);self.assertFalse(self.path.exists())
    def test_journal_rejects_shared_or_symlink_parent(self):
        shared=Path(self.tmp.name)/'shared';shared.mkdir(mode=0o755)
        with self.assertRaises(Refused):Journal(shared/'journal')
        alias=Path(self.tmp.name)/'alias';alias.symlink_to(self.tmp.name)
        with self.assertRaises(OSError):Journal(alias/'journal')
    def test_journal_directory_fd_survives_parent_path_replacement(self):
        private=Path(self.tmp.name)/'private';private.mkdir(mode=0o700)
        j=Journal(private/'receipt');moved=private.with_name('moved');private.rename(moved);private.mkdir(mode=0o700)
        try:
            j.save();self.assertTrue((moved/'receipt').exists());self.assertFalse((private/'receipt').exists())
        finally:j.close()
    def test_pending_persistence_and_existing_lock_fail_closed(self):
        with self.assertRaises(BlockingIOError):Journal(self.path)
        Path(str(self.path)+'.pending').write_text('interrupted')
        with self.assertRaises(FileExistsError):self.j.save()
if __name__=='__main__':unittest.main()
