"""Verification-only history/CAS tests using the actual private Journal and fake IO."""
import copy
import json
import os
from pathlib import Path
import stat
import tempfile
import unittest
from provision import Journal, Refused, digest
import introspection as intro


class Finalize(unittest.TestCase):
    def fixture(self):
        td = tempfile.TemporaryDirectory()
        self.addCleanup(td.cleanup)
        journal = Journal(Path(td.name) / 'journal')
        self.addCleanup(journal.close)
        manifest = {'version': 2, 'scope': intro.INTROSPECTION, 'role': intro.ROLE,
                    'guest': intro.GUEST, 'helper_sha256': 'a' * 64}
        journal.data = {'phase': 'introspection_adoption_uncertain',
                        'manifest_sha256': digest(manifest),
                        'introspection_secret_ref': intro.PREFIX + 'a' * 32,
                        'introspection_verifier': 'b' * 64,
                        'pair_capture': {'public': 'synthetic'},
                        'steps': {'store_introspection': 'done', **{
                            'adopt_' + name: 'pending' if name == 'verify_pair' else 'done'
                            for name in intro.ADOPT}}}
        journal.save()
        replacement = {'schema': 1, 'host': 'srv875272', 'source': 'c' * 40,
                       'runtime_sha256': 'd' * 64, 'baseline_sha256': 'e' * 64,
                       'expires_at_ms': 2000}
        observed = {k: replacement[k] for k in ('host', 'source', 'runtime_sha256', 'baseline_sha256')}
        observed.update(observed_at_ms=1000, identity_verified=True, policy_matches=True,
                        verifier_matches=True, pairing_matches=True, gateway_stopped=True,
                        oauth_disabled=True, prior_operation_finished=True)
        return manifest, journal, replacement, observed

    def run_finalize(self, m, j, r, observe):
        return intro.finalize_verified_adoption(m, j, r, lambda d: 'FINALIZE_INTROSPECTION ' + d,
                                               observe, now_ms=lambda: 1000)

    def test_success_retains_exact_history_and_only_finalizes_pending_verification(self):
        m, j, r, observed = self.fixture()
        before = j.path.read_bytes()
        prior = copy.deepcopy(j.data)
        result = self.run_finalize(m, j, r, lambda *_: observed)
        history = j.path.with_name(j.path.name + '.before-verified-finalization')
        self.assertEqual(history.read_bytes(), before)
        self.assertEqual(stat.S_IMODE(history.stat().st_mode), 0o400)
        self.assertEqual(j.data['phase'], 'introspection_adopted')
        self.assertEqual(j.data['steps'], {**prior['steps'], 'adopt_verify_pair': 'done'})
        self.assertEqual(j.data['introspection_secret_ref'], prior['introspection_secret_ref'])
        self.assertEqual(j.data['introspection_verifier'], prior['introspection_verifier'])
        self.assertFalse(result['adoptionReplayed'])
        self.assertNotIn(prior['introspection_verifier'], json.dumps(result))
        self.assertRaises(Refused, self.run_finalize, m, j, r, lambda *_: self.fail('replayed observation'))

    def test_mismatch_stale_unknown_or_extra_receipt_refuses_without_writes(self):
        for field, value in [('pairing_matches', False), ('identity_verified', None),
                             ('policy_matches', False), ('verifier_matches', False),
                             ('gateway_stopped', False), ('oauth_disabled', False),
                             ('prior_operation_finished', False), ('source', 'f' * 40),
                             ('observed_at_ms', -30001), ('extra', True)]:
            with self.subTest(field=field):
                m, j, r, observed = self.fixture()
                before = j.path.read_bytes()
                observed[field] = value
                self.assertRaises(Refused, self.run_finalize, m, j, r, lambda *_: observed)
                self.assertEqual(j.path.read_bytes(), before)
                self.assertFalse(j.path.with_name(j.path.name + '.before-verified-finalization').exists())

    def test_earlier_uncertain_effect_cannot_be_relabelled_as_verified(self):
        m, j, r, observed = self.fixture()
        j.data['steps']['adopt_write_policy'] = 'pending'
        j.save()
        self.assertRaises(Refused, self.run_finalize, m, j, r, lambda *_: self.fail('unexpected observation'))

    def test_foreign_journal_change_during_observation_refuses_history_and_save(self):
        m, j, r, observed = self.fixture()
        def observe(*_):
            j.path.write_text('{"foreign":true}')
            return observed
        self.assertRaises(Refused, self.run_finalize, m, j, r, observe)
        self.assertEqual(json.loads(j.path.read_text()), {'foreign': True})
        self.assertFalse(j.path.with_name(j.path.name + '.before-verified-finalization').exists())

    def test_pending_save_and_occupied_history_are_preserved(self):
        for pending in (True, False):
            m, j, r, observed = self.fixture()
            name = j.temporary if pending else j.path.name + '.before-verified-finalization'
            existing = j.path.with_name(name)
            existing.write_text('public incomplete evidence')
            with self.assertRaises((Refused, FileExistsError)):
                self.run_finalize(m, j, r, lambda *_: self.fail('occupied evidence must precede observation'))
            self.assertEqual(existing.read_text(), 'public incomplete evidence')

    def test_missing_or_malformed_verifier_refuses_before_observation(self):
        for value in (None, '', 'not-a-verifier'):
            m, j, r, observed = self.fixture()
            j.data['introspection_verifier'] = value
            j.save()
            self.assertRaises(Refused, self.run_finalize, m, j, r,
                              lambda *_: self.fail('invalid verifier must precede observation'))

    def test_confirmation_binds_exact_pending_journal_and_freezes_caller_inputs(self):
        bounds = []
        for suffix in ('a', 'b'):
            m, j, r, observed = self.fixture()
            j.data['introspection_secret_ref'] = intro.PREFIX + suffix * 32
            j.save()
            def confirm(bound):
                bounds.append(bound)
                r['source'] = 'f' * 40
                m['helper_sha256'] = 'f' * 64
                return 'FINALIZE_INTROSPECTION ' + bound
            result = intro.finalize_verified_adoption(m, j, r, confirm, lambda *_: observed,
                                                     now_ms=lambda: 1000)
            self.assertEqual(result['outcome'], 'introspection_adopted')
            self.assertEqual(j.data['verification_reconciliation']['source'], 'c' * 40)
        self.assertNotEqual(bounds[0], bounds[1])

    def test_confirmation_cannot_extend_approval_into_private_observation(self):
        m, j, r, observed = self.fixture()
        clock = [1000]
        def confirm(bound):
            clock[0] = 2001
            return 'FINALIZE_INTROSPECTION ' + bound
        self.assertRaises(Refused, intro.finalize_verified_adoption, m, j, r, confirm,
                          lambda *_: self.fail('expired approval must precede observation'),
                          now_ms=lambda: clock[0])

    def test_final_save_failure_never_returns_success_or_discards_history(self):
        m, j, r, observed = self.fixture()
        before = j.path.read_bytes()
        def fail():
            raise OSError('public simulated persistence failure')
        j.save = fail
        self.assertRaises(OSError, self.run_finalize, m, j, r, lambda *_: observed)
        self.assertEqual(j.path.read_bytes(), before)
        self.assertEqual(j.path.with_name(j.path.name + '.before-verified-finalization').read_bytes(), before)


if __name__ == '__main__':
    unittest.main()
