import contextlib,io,unittest
from types import SimpleNamespace
from unittest.mock import patch
import hosted_graph_auth as hosted
import layout_probe

class LayoutProbeTests(unittest.TestCase):
 def test_seed_and_observe_use_fixed_queries_once(self):
  for phase,keyword in [('seed','MERGE'),('observe','MATCH')]:
   with self.subTest(phase=phase),patch('layout_probe.query',return_value=[[1]]) as q:
    layout_probe.perform(object(),'synthetic-password',phase)
    self.assertEqual(q.call_count,1);self.assertTrue(q.call_args.args[-1].startswith(keyword))
    self.assertNotIn('synthetic-password',q.call_args.args[-1])
 def test_bad_result_and_unknown_phase_fail(self):
  with patch('layout_probe.query',return_value=[[0]]):
   with self.assertRaises(ValueError):layout_probe.perform(None,'secret','observe')
  with patch('layout_probe.query') as q:
   with self.assertRaises(ValueError):layout_probe.perform(None,'secret','other')
   q.assert_not_called()
 def test_seed_failure_is_not_retried_or_leaked(self):
  result=SimpleNamespace(returncode=1,stdout='SECRET')
  with patch.object(hosted,'command',return_value=result) as call,contextlib.redirect_stdout(io.StringIO()) as out:
   with self.assertRaisesRegex(hosted.Failure,'seed_failed_or_unknown'):hosted.run_layout_probe('a'*64,'seed')
  self.assertEqual(call.call_count,1);self.assertNotIn('SECRET',out.getvalue())
 def test_success_markers_are_fixed(self):
  for phase in ('seed','observe'):
   with self.subTest(phase=phase),patch.object(hosted,'command',return_value=SimpleNamespace(returncode=0,stdout='graph_layout:'+phase+':passed')),contextlib.redirect_stdout(io.StringIO()) as out:
    hosted.run_layout_probe('a'*64,phase)
    self.assertIn('graph_layout:',out.getvalue())
