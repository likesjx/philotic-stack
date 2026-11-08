import json
import unittest
from types import SimpleNamespace
from retire_runtime import retire_owned_runtime, LABEL

R='a'*64
M='b'*64
N='c'*32

def state(ident, running, pid, nonce=N):
    return {'Id':ident,'Config':{'Labels':{LABEL:nonce}},
            'HostConfig':{'RestartPolicy':{'Name':'no','MaximumRetryCount':0}},
            'RestartCount':0,'State':{'Running':running,'Dead':False,'Pid':pid}}

class Fake:
    def __init__(self):
        self.items={R:state(R,True,42),M:state(M,True,43)}
        self.calls=[]
        self.fail_stop=False
    def __call__(self,args):
        self.calls.append(args)
        if args[0]=='inspect': return SimpleNamespace(returncode=0,stdout=json.dumps([self.items[args[1]]]))
        if self.fail_stop: return SimpleNamespace(returncode=1,stdout='')
        self.items[R]=state(R,False,0)
        return SimpleNamespace(returncode=0,stdout=R+'\n')

class Group:
    def __init__(self): self.closed=False; self.value=0
    def populated(self): return self.value
    def close(self): self.closed=True

def fake_capture(_runtime, _pid): return Group()

class RetireTest(unittest.TestCase):
    def test_exact_owned_stop_and_mongo_retained(self):
        f=Fake(); retire_owned_runtime(f,R,M,N,fake_capture)
        self.assertEqual(f.calls,[['inspect',M],['inspect',R],['inspect',R],['stop','--time','10',R],['inspect',R],['inspect',M]])
    def test_wrong_identity_and_unsafe_state_fail_before_stop(self):
        for key,value in [('Id','d'*64),('RestartCount',1),('State',{'Running':True,'Dead':False,'Pid':0}),('Config',{'Labels':{LABEL:'d'*32}})]:
            f=Fake();f.items[R][key]=value
            with self.assertRaises(RuntimeError): retire_owned_runtime(f,R,M,N,fake_capture)
            self.assertFalse(any(x[0]=='stop' for x in f.calls))
    def test_stop_failure_is_not_retried(self):
        f=Fake();f.fail_stop=True
        with self.assertRaises(RuntimeError): retire_owned_runtime(f,R,M,N,fake_capture)
        self.assertEqual(sum(x[0]=='stop' for x in f.calls),1)
    def test_mongo_incarnation_change_fails(self):
        f=Fake();original=f.__call__
        def changed(args):
            result=original(args)
            if args[0]=='stop': f.items[M]=state(M,True,44)
            return result
        with self.assertRaises(RuntimeError): retire_owned_runtime(changed,R,M,N,fake_capture)
    def test_populated_or_unknown_cgroup_refuses_resume_and_closes(self):
        for value in (1, None):
            f=Fake(); group=Group(); group.value=value
            with self.assertRaises(RuntimeError): retire_owned_runtime(f,R,M,N,lambda *_: group)
            self.assertTrue(group.closed)
    def test_bad_identifiers_never_call_docker(self):
        f=Fake()
        with self.assertRaises(ValueError): retire_owned_runtime(f,R,R,N,fake_capture)
        self.assertEqual(f.calls,[])

if __name__=='__main__': unittest.main()
