"""Exercise the unchanged inventory guard against isolated positive/negative inputs."""
import contextlib
import copy
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('egress_guard', ROOT / 'scripts/check-outbound-egress-inventory.py')
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)
PRODUCTION = 'crates/aiua/src/service/native_authority/runtime.rs'
COMPOSITION = 'crates/aiua/src/service/native_authority/runtime/composition.rs'


class InventoryGuard(unittest.TestCase):
    def run_guard(self, root, payload):
        inventory = root / 'inventory.json'
        inventory.write_text(json.dumps(payload))
        errors = io.StringIO()
        with patch.object(guard, 'ROOT', root), patch.object(guard, 'INVENTORY', inventory), contextlib.redirect_stderr(errors), contextlib.redirect_stdout(io.StringIO()):
            return guard.main(), errors.getvalue()

    def test_actual_inventory_and_each_native_omission(self):
        payload = json.loads(guard.INVENTORY.read_text())
        # Point the fixture's crates directory at real source; only the temporary
        # inventory is changed. No repository source is written.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'crates').symlink_to(ROOT / 'crates', target_is_directory=True)
            self.assertEqual(self.run_guard(root, payload)[0], 0)
            for missing in (PRODUCTION, COMPOSITION):
                altered = copy.deepcopy(payload)
                altered['direct_callers'] = [e for e in altered['direct_callers'] if e['path'] != missing]
                status, error = self.run_guard(root, altered)
                self.assertEqual(status, 1)
                self.assertIn('unclassified direct callers:', error)
                self.assertIn(missing, error)

    def test_new_nested_caller_is_not_broadly_excluded(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            caller = root / 'crates/aiua/src/service/native_authority/runtime/new_caller.rs'
            caller.parent.mkdir(parents=True)
            caller.write_text('fn test_named_but_not_excluded() { reqwest::Client::new(); }')
            status, error = self.run_guard(root, {'direct_callers': [], 'migrated_callers': []})
            self.assertEqual(status, 1)
            self.assertIn('runtime/new_caller.rs', error)

    def test_invalid_and_stale_entries_still_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            caller = root / 'crates/example/src/lib.rs'
            caller.parent.mkdir(parents=True)
            caller.write_text('fn client() { reqwest::Client::builder(); }')
            entry = {'path': 'crates/example/src/lib.rs', 'traffic_classes': ['general-api'],
                     'disposition': 'named-exception', 'authority': 'synthetic explicit binding', 'note': 'synthetic'}
            valid = {'direct_callers': [entry], 'migrated_callers': []}
            self.assertEqual(self.run_guard(root, valid)[0], 0)
            mutations = [({'traffic_classes': ['anything']}, 'invalid traffic_classes'),
                         ({'disposition': 'ignore'}, 'invalid disposition'),
                         ({'authority': ''}, 'authority and note are required'),
                         ({'note': ''}, 'authority and note are required'),
                         ({'path': 'crates/missing.rs'}, 'stale direct-caller entries')]
            for fields, expected in mutations:
                with self.subTest(fields=fields):
                    payload = {'direct_callers': [{**entry, **fields}], 'migrated_callers': []}
                    status, error = self.run_guard(root, payload)
                    self.assertEqual(status, 1)
                    self.assertIn(expected, error)
            status, error = self.run_guard(root, {'direct_callers': [entry, entry], 'migrated_callers': []})
            self.assertEqual(status, 1)
            self.assertIn('duplicate direct-caller path', error)


if __name__ == '__main__':
    unittest.main(verbosity=2)
