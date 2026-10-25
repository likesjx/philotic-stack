import copy,json,unittest
from unittest.mock import patch
import introspection_compose as c
import introspection_owner as o
from test_introspection_owner import Fixture

OVERRIDE=c.APP_DIR+'/deployment-baselines/issuer-configurations/39299702815-2/production-digest-promotion.override.yml'

class Tests(unittest.TestCase):
 def test_versioned_manifest_keeps_historical_path_contract(self):
  f=Fixture();self.addCleanup(f.close);m=copy.deepcopy(f.m)
  m['compose'].update(project='jaredlikes-com',files=[{'path':c.PERSISTENT_BASE,'sha256':c.PERSISTENT_SHA},{'path':OVERRIDE,'sha256':'a'*64}])
  self.assertRaises(Exception,o.validate,m)
  m['schema']=2;o.validate(m)
  for change in ('hash','legacy','foreign','duplicate'):
   bad=copy.deepcopy(m)
   if change=='hash':bad['compose']['files'][0]['sha256']='b'*64
   elif change=='legacy':bad['compose']['files'][0]['path']=c.APP_DIR+'/docker-compose.yml'
   elif change=='foreign':bad['compose']['files'][1]['path']='/tmp/production.override.yml'
   else:bad['compose']['files'].append(copy.deepcopy(bad['compose']['files'][1]))
   with self.subTest(change=change):self.assertRaises(Exception,o.validate,bad)
 def test_exact_base_hash_and_closed_override_dependencies(self):
  raw=b'{"services":{"app":{"image":"synthetic-only"}}}'
  self.assertRaises(c.Refusal,c.checked_compose,c.PERSISTENT_BASE,raw,True)
  # Synthetic hash substitution exercises mechanism, not the private live bytes.
  with patch.object(c,'PERSISTENT_SHA',o.sha(raw)):self.assertTrue(c.checked_compose(c.PERSISTENT_BASE,raw,True))
  good={'services':{'app':{'image':o.REGISTRY,'environment':{'PERSONAL_OAUTH_ENABLED':'false'},'env_file':[c.VERIFIER_PATH]}}}
  self.assertTrue(c.checked_compose(OVERRIDE,json.dumps(good).encode(),True))
  self.assertRaises(c.Refusal,c.checked_compose,OVERRIDE,json.dumps(good).encode(),False)
  for bad in ({'include':'/unapproved'}, {'services':{'other':{}}}, {'services':{'app':{'env_file':['/unapproved']}}}, {'services':{'app':{'build':'.'}}}):
   self.assertRaises(c.Refusal,c.checked_compose,OVERRIDE,json.dumps(bad).encode(),True)
  self.assertRaises(c.Refusal,c.checked_compose,OVERRIDE,b'{"services":{},"services":{}}',True)
 def test_persistent_real_guard_still_refuses_source_and_configuration_drift(self):
  f=Fixture();self.addCleanup(f.close);raw=b'{"services":{"app":{}}}';f.put(c.PERSISTENT_BASE,raw);f.put(OVERRIDE,b'services:\n  app:\n    environment:\n      PERSONAL_OAUTH_ENABLED: "false"\n')
  m=copy.deepcopy(f.m);m['schema']=2;m['compose'].update(project='jaredlikes-com',files=[{'path':p,'sha256':o.sha(f.fs.read(p)[0])}for p in(c.PERSISTENT_BASE,OVERRIDE)])
  f.owner.m=m
  with patch.object(c,'PERSISTENT_SHA',o.sha(raw)),patch.object(o,'PERSISTENT_SHA',o.sha(raw)):
   f.owner.guard({'steps':{}})
   f.fs.path(OVERRIDE).write_bytes(b'changed')
   self.assertRaises(Exception,f.owner.guard,{'steps':{}})
  self.assertFalse(any('restart'in a or 'stop'in a for a in f.calls))

if __name__=='__main__':unittest.main()
