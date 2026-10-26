"""Public fake-helper subprocess tests; no real helper, hotel or OS identity changes."""
import contextlib
import json
import os
from pathlib import Path
import sqlite3
import sys
import tempfile
import time
import unittest
from unittest.mock import patch
import known_source_helper_fixture as fixture
from known_source_helper_fixture import PREFIX, allocated, line, seed_helper_record


class Protocol(unittest.TestCase):
    @contextlib.contextmanager
    def pipe(self, raw, close=True):
        read, write = os.pipe()
        stream = os.fdopen(read, 'rb', buffering=0)
        try:
            os.write(write, raw)
            if close:
                os.close(write)
                write = None
            yield stream
        finally:
            stream.close()
            if write is not None:
                os.close(write)

    def test_two_messages_are_not_consumed_together(self):
        ref = PREFIX + 'a' * 32
        raw = json.dumps({'allocated_reference': ref}).encode() + b'\n' + json.dumps({'reference': ref}).encode() + b'\n'
        with self.pipe(raw) as stream:
            self.assertEqual(allocated(line(stream, time.monotonic() + 1)), ref)
            self.assertEqual(line(stream, time.monotonic() + 1), {'reference': ref})

    def test_eof_refuses_instead_of_waiting(self):
        with self.pipe(b'{') as stream:
            with self.assertRaises(ValueError):
                line(stream, time.monotonic() + 1)

    def test_silent_child_hits_deadline(self):
        with self.pipe(b'', close=False) as stream:
            with self.assertRaises(TimeoutError):
                line(stream, time.monotonic() + .01)

    def test_oversized_line_refuses(self):
        with self.pipe(b'x' * 4096) as stream:
            with self.assertRaises(ValueError):
                line(stream, time.monotonic() + 1)

    def test_unexpected_and_cross_kind_allocations_refuse(self):
        for value in [None, [], {}, {'allocated_reference': 1},
                      {'allocated_reference': PREFIX + 'A' * 32},
                      {'allocated_reference': PREFIX + 'a' * 31},
                      {'allocated_reference': 'secret://hotel/default/other/' + 'a' * 32},
                      {'allocated_reference': PREFIX + 'a' * 32, 'unexpected': True}]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                allocated(value)

    @unittest.skipUnless(os.uname().sysname != 'Linux' or os.geteuid() != 0,
                         'refusal case applies outside root Linux')
    def test_cannot_use_as_local_operator(self):
        with self.assertRaises(AssertionError):
            seed_helper_record(None, None)


class FullFixture(unittest.TestCase):
    """Exercise actual entrypoint/SQLite/pipes/FDs with simulated Linux guards.

    This does not qualify setuid/capability/dumpability or real helper encryption.
    Those remain requirements of the disposable Linux run.
    """
    def exercise(self, mode='success', occupied=False):
        with tempfile.TemporaryDirectory(prefix='public-helper-regression-') as td:
            top = Path(td)
            work = top / 'work'
            work.mkdir()
            root = top / 'fixture-root'
            marker = top / 'fake-helper-started'
            previous = PREFIX + 'b' * 32
            current = PREFIX + 'a' * 32
            with contextlib.closing(sqlite3.connect(work / 'aiua_context.db')) as db, db:
                if mode == 'wal':
                    db.execute('PRAGMA journal_mode=WAL')
                db.execute('CREATE TABLE graph_nodes(node_key TEXT PRIMARY KEY, data_json TEXT)')
                db.executemany('INSERT INTO graph_nodes VALUES(?, ?)',
                               [('secret:' + previous, '{}'), ('public-sentinel', '{"preserve":true}')])
            helper = top / 'fake-helper'
            helper.write_text(f'''#!{sys.executable}
import base64,json,pathlib,socket,sqlite3,sys
pathlib.Path({str(marker)!r}).write_text('public')
with socket.socket(fileno=int(sys.argv[3])) as channel:
 channel.sendall(b'READY\\n')
 data=b''
 while True:
  part=channel.recv(130)
  if not part:break
  data+=part
 assert data==b'PHILOTIC_VAULT_MASTER_KEY='+base64.b64encode(bytes([7])*32)+b'\\n'
request=json.loads(sys.stdin.readline())
assert request=={{'operation':'create_introspection','kind':'percival-issuer-introspection','scope':'percival_gateway_persistent_recall_v1','allowed_roles':['percival-personal-recall'],'allowed_guests':['percival-personal-gateway'],'secret':'i'*43}}
print(json.dumps({{'allocated_reference':{current!r}}}),flush=True)
assert json.loads(sys.stdin.readline())=={{'commit':{current!r}}}
record={{'scope':'percival_gateway_persistent_recall_v1','allowed_roles':['percival-personal-recall'],'allowed_guests':['percival-personal-gateway']}}
if {mode!r}=='wrong_acl':record['allowed_guests']=['outsider']
with sqlite3.connect({str(root / 'data/aiua_context.db')!r}) as db:
 db.execute('INSERT INTO graph_nodes VALUES(?,?)',('secret:'+{current!r},json.dumps(record)))
print(json.dumps({{'reference':{current!r}}}),flush=True)
if {mode!r}=='extra_output':print('unexpected fixture output',flush=True)
''')
            helper.chmod(0o700)
            if occupied:
                root.mkdir()
                (root / 'sentinel').write_text('preserve')
            error = None
            result = None
            actual_is_file = Path.is_file
            actual_connect = sqlite3.connect
            connections = []

            def connect(*args, **kwargs):
                connection = actual_connect(*args, **kwargs)
                connections.append(connection)
                return connection

            def is_file(path):
                return True if str(path) == '/.dockerenv' else actual_is_file(path)

            with patch.object(fixture, 'ROOT', root), patch.object(fixture, 'HELPER', helper), \
                    patch.object(fixture.sys, 'platform', 'linux'), \
                    patch.object(fixture.os, 'geteuid', return_value=0), \
                    patch.object(fixture.os, 'chown'), patch.object(Path, 'is_file', is_file), \
                    patch.object(fixture.sqlite3, 'connect', side_effect=connect), \
                    patch.dict(fixture.os.environ, {'PERCIVAL_ISOLATED_LINUX_FIXTURE': '1',
                                                  'PERCIVAL_KNOWN_SOURCE_CONTRACT': '1'}):
                try:
                    result = fixture.seed_helper_record(work, previous)
                except Exception as caught:
                    error = caught
            closed = []
            for connection in connections:
                try:
                    connection.execute('SELECT 1')
                    closed.append(False)
                    connection.close()
                except sqlite3.ProgrammingError:
                    closed.append(True)
            with contextlib.closing(sqlite3.connect(work / 'aiua_context.db')) as db:
                rows = dict(db.execute('SELECT node_key,data_json FROM graph_nodes'))
            return {'result': result, 'error': error, 'rows': rows, 'started': marker.exists(),
                    'root_exists': root.exists(), 'home_exists': (work / 'helper-home').exists(),
                    'sentinel_preserved': (root / 'sentinel').exists(), 'connections_closed': closed, 'current': current, 'previous': previous}

    def test_actual_fixture_entrypoint_completes_protocol_and_snapshot(self):
        result = self.exercise()
        self.assertIsNone(result['error'], repr(result['error']))
        self.assertEqual(result['result'], result['current'])
        self.assertTrue(result['started'])
        self.assertTrue(result['connections_closed'])
        self.assertTrue(all(result['connections_closed']))
        self.assertIn('secret:' + result['current'], result['rows'])
        self.assertNotIn('secret:' + result['previous'], result['rows'])
        self.assertIn('public-sentinel', result['rows'])
        self.assertFalse(result['root_exists'])
        self.assertFalse(result['home_exists'])

    def test_wal_fixture_closes_every_connection_before_snapshot_return(self):
        result = self.exercise('wal')
        self.assertIsNone(result['error'], repr(result['error']))
        self.assertEqual(result['result'], result['current'])
        self.assertTrue(result['connections_closed'])
        self.assertTrue(all(result['connections_closed']))
        self.assertFalse(result['root_exists'])
        self.assertFalse(result['home_exists'])

    def test_helper_extra_output_refuses_and_preserves_original_snapshot(self):
        self.assert_refusal(self.exercise('extra_output'))

    def test_helper_wrong_acl_refuses_and_preserves_original_snapshot(self):
        self.assert_refusal(self.exercise('wrong_acl'))

    def assert_refusal(self, result):
        self.assertIsInstance(result['error'], AssertionError)
        self.assertTrue(result['started'])
        self.assertTrue(all(result['connections_closed']))
        self.assertIn('secret:' + result['previous'], result['rows'])
        self.assertNotIn('secret:' + result['current'], result['rows'])
        self.assertFalse(result['root_exists'])
        self.assertFalse(result['home_exists'])

    def test_occupied_fixture_root_refuses_without_touching_sentinel(self):
        result = self.exercise(occupied=True)
        self.assertIsInstance(result['error'], AssertionError)
        self.assertFalse(result['started'])
        self.assertTrue(result['root_exists'])
        self.assertTrue(result['sentinel_preserved'])


if __name__ == '__main__':
    unittest.main()
