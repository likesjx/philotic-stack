import contextlib,io,json,os,tempfile,unittest
from pathlib import Path
from unittest.mock import patch
import hosted_graph_auth as h
from safe_graph_copy import CopyFailure

class HostedRestoreTests(unittest.TestCase):
 def execute(self, failure=''):
  temp=tempfile.TemporaryDirectory();self.addCleanup(temp.cleanup);root=Path(temp.name);deps=root/'deps';(deps/'neo4j').mkdir(parents=True);(deps/'neo4j/__init__.py').write_text('')
  ids=['a'*64,'b'*64];calls=[];removals=[];create_count=0;captured=io.StringIO()
  def call(args,timeout=30):
   nonlocal create_count
   calls.append(args)
   if args[:2]==['image','inspect']:return json.dumps([{'Id':'sha256:'+'c'*64,'RepoDigests':[h.IMAGE]}])
   if args[0]=='create':
    create_count+=1
    if create_count==2 and failure=='create_uncertain':raise h.Failure('docker_command_failed')
    return ids[create_count-1]
   if args[:4]==['ps','-aq','--no-trunc','--filter']:return ids[1] if failure=='create_uncertain' else ''
   if args==['ps','-aq','--no-trunc'] and failure=='remaining':return 'd'*64
   return ''
  def remove(ident,nonce,image):
   removals.append(ident)
   if ident==ids[1] and failure=='restore_remove':raise h.Failure('remove_failed')
  env={'GITHUB_ACTIONS':'true','CI':'true','RUNNER_OS':'Linux','GITHUB_REPOSITORY':'likesjx/philotic-stack','GITHUB_REF':'refs/heads/codex/synthetic-memgraph-link-restore','RUNNER_TEMP':str(root)}
  state={'State':{'Running':False,'ExitCode':0,'Dead':False,'Pid':0}}
  copier=CopyFailure('copy_failed') if failure=='copy' else None
  with patch.dict(os.environ,env),patch.object(h,'call',side_effect=call),patch.object(h,'owned',return_value=state),patch.object(h,'isolated'),patch.object(h,'ready'),patch.object(h,'run_layout_probe'),patch.object(h,'stop_for_restart'),patch.object(h,'inventory',return_value={}),patch.object(h,'copy_closed_fixture',side_effect=copier),patch.object(h,'remove',side_effect=remove),contextlib.redirect_stdout(captured):
   if failure:
    with self.assertRaises(h.Failure):h.main(str(deps))
   else:h.main(str(deps))
  return calls,removals,captured.getvalue(),root
 def test_two_containers_reaped_after_restored_observation(self):
  calls,removed,out,root=self.execute()
  self.assertEqual(removed,['b'*64,'a'*64]);self.assertIn('graph_restore:record:passed',out);self.assertIn('graph_auth:cleanup:passed',out)
  creates=[x for x in calls if x[0]=='create'];self.assertEqual(len(creates),2)
  self.assertTrue(any('src=' in x and '/state,dst=/var/lib/memgraph' in x for x in creates[0]));self.assertTrue(any('/restore,dst=/var/lib/memgraph' in x for x in creates[1]))
  self.assertEqual(list(root.glob('graph-auth-*')),[])
 def test_failed_copy_never_creates_restore(self):
  calls,removed,out,_=self.execute('copy');self.assertEqual(len([x for x in calls if x[0]=='create']),1);self.assertEqual(removed,['a'*64]);self.assertNotIn('graph_restore:record:passed',out);self.assertIn('graph_auth:cleanup:passed',out)
 def test_lost_second_create_response_is_reconciled_and_both_removed(self):
  _,removed,out,_=self.execute('create_uncertain');self.assertEqual(removed,['b'*64,'a'*64]);self.assertIn('graph_auth:cleanup:passed',out);self.assertNotIn('qualification:passed',out)
 def test_failed_restore_removal_attempts_original_and_preserves_scratch(self):
  _,removed,out,root=self.execute('restore_remove');self.assertEqual(removed,['b'*64,'a'*64]);self.assertIn('graph_auth:cleanup:inconclusive',out);self.assertNotIn('qualification:passed',out);self.assertEqual(len(list(root.glob('graph-auth-*'))),1)

 def test_unexpected_container_preserves_scratch_and_fails_cleanup(self):
  _,removed,out,root=self.execute('remaining');self.assertEqual(removed,['b'*64,'a'*64]);self.assertIn('graph_auth:cleanup:inconclusive',out);self.assertNotIn('qualification:passed',out);self.assertEqual(len(list(root.glob('graph-auth-*'))),1)
