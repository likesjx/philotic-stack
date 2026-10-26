"""Local tests of bounded fixture protocol handling; no helper or hotel runs."""
import contextlib
import json
import os
import time
import unittest
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


if __name__ == '__main__':
    unittest.main()
