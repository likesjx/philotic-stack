import unittest
import datetime
from driver import denied, rpc_ok, run, EXPECTED_SHA, A, B, MARK_A, MARK_B


def result(text):
    return 200, {'jsonrpc': '2.0', 'id': 17, 'result': {'content': [{'type': 'text', 'text': text}]}}


class Fixture:
    def __init__(self, leak=False, break_recall=False):
        self.leak, self.break_recall = leak, break_recall
        self.revoked = False

    def request(self, method, path, body=None, **kwargs):
        if method == 'DELETE':
            self.revoked = True
            return 204, None
        if path == '/api/vaults': return 200, ['default']
        if path.startswith('/api/engrams?') and method == 'GET' and kwargs.get('admin'): return 200, {'engrams': [{'id': A + '_id'}]}
        if path.startswith('/api/stats'): return 200, {'engram_count': 0 if 'default' in path else 1}
        if path == '/api/admin/vaults/config': return 200, body
        if path == '/api/admin/keys': return 201, {'token': 'public-fixture-token', 'key': {'id': 'fixture-key', 'expires_at': body['expires']}}
        if path.startswith('/api/engrams?') and kwargs.get('admin'):
            if path != '/api/engrams?vault=' + body['vault']: return 400, None
            return 201, {'id': body['vault'] + '_id'}
        return 403, None

    def rpc(self, token, tool, args, query=''):
        if self.break_recall and tool == 'muninn_recall': raise TimeoutError('uncertain')
        if tool == 'muninn_read' and args['id'] == A + '_id': return result(MARK_A)
        if tool == 'muninn_recall' and args['vault'] == A and not query: return result(MARK_A)
        if self.leak and tool == 'muninn_recall': return result(MARK_B)
        return 200, {'id': 17, 'error': {'code': -32001, 'message': 'not authorized'}}


class Tests(unittest.TestCase):
    def expiry(self):
        return (datetime.datetime.now(datetime.timezone.utc)+datetime.timedelta(seconds=30)).isoformat()
    def test_error_is_not_denial(self):
        self.assertFalse(denied((500, None)))
        self.assertFalse(denied((200, {'id': 17, 'error': {'code': -32000, 'message': 'storage unavailable'}})))
        self.assertFalse(denied((200, {'id': 18, 'error': {'code': -32001, 'message': 'not authorized'}})))

    def test_empty_or_error_result_is_not_success(self):
        self.assertFalse(rpc_ok((200, {})))
        self.assertFalse(rpc_ok((200, {'jsonrpc': '2.0', 'id': 17, 'result': {'isError': True}})))

    def test_fixture_success_retains_unsupported_claims(self):
        fixture = Fixture()
        out = run(fixture, EXPECTED_SHA, self.expiry())
        self.assertTrue(fixture.revoked)
        for name, value in out['cases'].items():
            if name not in ('all_cognitive_storage_writes_suppressed', 'actual_gateway_adapter', 'expired_key_rejected'):
                self.assertEqual(value, 'pass', name)
        self.assertTrue(out['cases']['all_cognitive_storage_writes_suppressed'].startswith('unsupported'))
        self.assertTrue(out['cases']['actual_gateway_adapter'].startswith('not_run'))

    def test_cross_vault_leak_is_failure(self):
        out = run(Fixture(leak=True), EXPECTED_SHA, self.expiry())
        self.assertEqual(out['cases']['mcp_body_cross_vault_denied'], 'fail')
        self.assertEqual(out['cases']['mcp_query_no_foreign_content'], 'fail')

    def test_added_record_fails_even_when_stats_count_is_unchanged(self):
        class Added(Fixture):
            inventories = 0
            def request(self, method, path, body=None, **kwargs):
                status, result = super().request(method, path, body, **kwargs)
                if method == 'GET' and path.startswith('/api/engrams?') and kwargs.get('admin'):
                    self.inventories += 1
                    if self.inventories > 1: result['engrams'].append({'id': 'unexpected-record'})
                return status, result
        out = run(Added(), EXPECTED_SHA, self.expiry())
        self.assertEqual(out['cases']['denied_writes_did_not_add_records'], 'fail')

    def test_readiness_never_discards_a_foreign_result(self):
        class ForeignFirst(Fixture):
            recalls = 0
            def rpc(self, token, tool, args, query=''):
                if tool == 'muninn_recall':
                    self.recalls += 1
                    if self.recalls == 1: return result(MARK_B)
                return super().rpc(token, tool, args, query)
        fixture = ForeignFirst()
        out = run(fixture, EXPECTED_SHA, self.expiry())
        self.assertIn('setup_or_transport', out)
        self.assertEqual(fixture.recalls, 1)

    def test_unknown_outcome_stops_and_revokes_without_retry(self):
        fixture = Fixture(break_recall=True)
        out = run(fixture, EXPECTED_SHA, self.expiry())
        self.assertIn('failed_or_uncertain', out['setup_or_transport'])
        self.assertTrue(fixture.revoked)

    def test_hash_guard_precedes_requests(self):
        with self.assertRaises(ValueError): run(None, 'wrong', 'fixture-expiry')

    def test_long_lived_fixture_key_refused_and_revoked(self):
        fixture = Fixture()
        expiry = (datetime.datetime.now(datetime.timezone.utc)+datetime.timedelta(hours=1)).isoformat()
        out = run(fixture, EXPECTED_SHA, expiry)
        self.assertEqual(out['cases']['test_key_expiry_metadata_bounded'], 'fail')
        self.assertTrue(fixture.revoked)

    def test_denial_without_active_positive_control_is_inconclusive(self):
        class Expired(Fixture):
            reads=0
            def rpc(self, token, tool, args, query=''):
                if tool=='muninn_read':
                    self.reads+=1
                    if self.reads>1:return 401,None
                return super().rpc(token,tool,args,query)
        out=run(Expired(),EXPECTED_SHA,self.expiry())
        self.assertTrue(out['cases']['mcp_body_cross_vault_denied'].startswith('inconclusive'))


if __name__ == '__main__': unittest.main()
