"""Native-only lifecycle: fake trusted adapters and real disposable Journal."""
import copy
import datetime
import json
from pathlib import Path
import tempfile
import unittest
import provision as p

NOW = datetime.datetime(2026, 10, 26, 16, tzinfo=datetime.timezone.utc)
TOKEN = 'mk_' + 'S' * 43


class Api:
    def __init__(self, mode='ok'):
        self.mode = mode
        self.calls = []
        self.cookies = ['public dummy cookie']
        self.closed = False

    def request(self, method, path, payload=None, admin=False):
        self.calls.append((method, path, copy.deepcopy(payload), admin))
        if method == 'GET':
            return 200, ['default']
        if self.mode == 'create_timeout':
            raise TimeoutError('public simulated lost reply')
        return 201, {'key': {'id': 'public-native-id', 'vault': 'default',
                             'mode': 'learn' if self.mode == 'wrong_mode' else 'observe',
                             'expires_at': '2026-11-25T16:00:00Z'}, 'token': TOKEN}

    def close_private_session(self):
        self.closed = True
        self.cookies.clear()
        if self.mode == 'logout_error':
            raise TimeoutError('public simulated logout error')


class Broker:
    def __init__(self, mode='ok'):
        self.mode = mode
        self.calls = []
        self.closed = False

    def store_exact_acl(self, **kw):
        self.calls.append(kw)
        if self.mode == 'store_timeout':
            raise TimeoutError('public simulated lost storage reply')
        return {'reference': 'secret://hotel/default/percival-muninn-observe/' + 'a' * 32}

    def close(self):
        self.closed = True


class NativeOnly(unittest.TestCase):
    def fixture(self, mode='ok'):
        td = tempfile.TemporaryDirectory()
        self.addCleanup(td.cleanup)
        j = p.Journal(Path(td.name) / 'native-journal')
        self.addCleanup(j.close)
        m = {'version': 3, 'vault': 'default', 'client': 'claude_muninn_recall_v1',
             'expires_at': '2026-11-25T16:00:00Z', 'approval_expires_at':'2026-10-26T16:30:00Z', 'helper_sha256': 'a' * 64,
             'introspection_secret_ref': 'secret://hotel/default/percival-issuer-introspection/' + 'b' * 32}
        return m, j, Api(mode), Broker(mode)

    def execute(self, m, j, api, broker):
        return p.execute_native_only(m, j, lambda d: 'CREATE_NATIVE_OBSERVE ' + d,
                                     lambda: (api, broker), now=NOW)

    def test_only_one_observe_key_and_one_native_record_no_introspection_or_policy_change(self):
        m, j, api, broker = self.fixture()
        result = self.execute(m, j, api, broker)
        self.assertEqual(result['outcome'], 'native_stored_not_adopted')
        self.assertFalse(result['introspectionChanged'])
        self.assertEqual([c[:2] for c in api.calls], [('GET', '/api/vaults'), ('POST', '/api/admin/keys')])
        self.assertEqual(api.calls[1][2]['mode'], 'observe')
        self.assertEqual(len(broker.calls), 1)
        self.assertEqual(broker.calls[0], {'kind': 'percival-muninn-observe', 'scope': 'default',
                                         'secret': TOKEN, 'allowed_roles': [p.ROLE], 'allowed_guests': [p.GUEST]})
        self.assertEqual(j.data['retained_introspection_ref'], m['introspection_secret_ref'])
        self.assertNotIn(TOKEN, j.path.read_text())
        self.assertNotIn(TOKEN, json.dumps(result))
        self.assertNotIn('introspection_verifier', j.data)
        self.assertTrue(api.closed and broker.closed)
        self.assertTrue(result['clientCookieCleared'])
        self.assertRaises(p.Refused, self.execute, m, j, api, broker)
        self.assertEqual(len(api.calls), 2)

    def test_lost_create_reply_is_unknown_and_cannot_be_blindly_repeated(self):
        m, j, api, broker = self.fixture('create_timeout')
        result = self.execute(m, j, api, broker)
        self.assertIsNone(result['nativeKeyCreated'])
        self.assertEqual(j.data['steps'], {'create_observe': 'pending'})
        self.assertFalse(broker.calls)
        self.assertRaises(p.Refused, self.execute, m, j, api, broker)
        self.assertEqual(len(api.calls), 2)

    def test_lost_store_reply_preserves_owned_key_id_without_adoption_or_retry(self):
        m, j, api, broker = self.fixture('store_timeout')
        result = self.execute(m, j, api, broker)
        self.assertEqual(result['outcome'], 'native_reconciliation_required')
        self.assertTrue(result['nativeKeyCreated'])
        self.assertEqual(j.data['key_ids'], {'observe': 'public-native-id'})
        self.assertEqual(j.data['steps']['store_observe'], 'pending')
        self.assertNotIn('secret_ref', j.data)
        self.assertTrue(api.closed and broker.closed)

    def test_wrong_access_mode_never_reaches_encrypted_store(self):
        m, j, api, broker = self.fixture('wrong_mode')
        self.assertEqual(self.execute(m, j, api, broker)['outcome'], 'native_reconciliation_required')
        self.assertFalse(broker.calls)
        self.assertIn('observe', j.data['key_ids'])

    def test_scope_client_expiry_and_confirmation_refuse_before_private_handoff(self):
        for key, value in [('vault', 'other'), ('client', 'chatgpt_muninn_recall_v1'),
                           ('expires_at', '2026-10-26T15:00:00Z'), ('expires_at', '2026-12-26T16:00:00Z')]:
            m, j, api, broker = self.fixture()
            m[key] = value
            self.assertRaises(p.Refused, self.execute, m, j, api, broker)
            self.assertFalse(api.calls)
            self.assertFalse(j.path.exists())
        m, j, api, broker = self.fixture()
        self.assertRaises(p.Refused, p.execute_native_only, m, j, lambda _: 'cancel',
                          lambda: self.fail('private input before confirmation'), now=NOW)
        self.assertFalse(j.path.exists())

    def test_expiry_during_confirmation_or_handoff_prevents_key_creation(self):
        for stage in ('confirmation', 'handoff'):
            m, j, api, broker = self.fixture()
            clock = [NOW]
            def expire():
                clock[0] = NOW + datetime.timedelta(minutes=31)
            def confirm(bound):
                if stage == 'confirmation':expire()
                return 'CREATE_NATIVE_OBSERVE ' + bound
            def handoff():
                if stage == 'handoff':expire()
                return api, broker
            if stage == 'confirmation':
                self.assertRaises(p.Refused, p.execute_native_only, m, j, confirm, handoff,
                                  now=lambda: clock[0])
                self.assertFalse(api.calls)
            else:
                result = p.execute_native_only(m, j, confirm, handoff, now=lambda: clock[0])
                self.assertFalse(result['nativeKeyCreated'])
                self.assertEqual([c[0] for c in api.calls], ['GET'])
                self.assertTrue(api.closed and broker.closed)
            self.assertFalse(broker.calls)

    def test_logout_failure_is_reported_without_claiming_session_revocation(self):
        m, j, api, broker = self.fixture('logout_error')
        result = self.execute(m, j, api, broker)
        self.assertTrue(result['logoutAttempted'])
        self.assertFalse(result['logoutCallCompleted'])
        self.assertTrue(result['clientCookieCleared'])
        self.assertNotIn('sessionRevoked', result)


if __name__ == '__main__':
    unittest.main()
