"""Disposable Linux fixture only: actual helper record -> actual candidate hotel.

All inputs are public synthetic constants. This module must never be used as an
operator tool. It refuses occupied fixed paths and cleans only its own fixture.
"""
import base64
from contextlib import closing
import json
import os
from pathlib import Path
import re
import select
import shutil
import socket
import sqlite3
import subprocess
import sys
import time

PREFIX = 'secret://hotel/default/percival-issuer-introspection/'
HELPER = Path('/helper-target/release/persistent-broker-helper')
ROOT = Path('/opt/philotic')
KEY = base64.b64encode(bytes([7]) * 32)
VALUE = 'i' * 43


def line(stream, deadline):
    result = bytearray()
    while len(result) < 4096:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([stream], [], [], remaining)[0]:
            raise TimeoutError('synthetic helper response deadline')
        part = os.read(stream.fileno(), 1)
        if not part:
            raise ValueError('synthetic helper response ended')
        result += part
        if part == b'\n':
            return json.loads(result)
    raise ValueError('synthetic helper response bound')


def allocated(value):
    if (type(value) is not dict or set(value) != {'allocated_reference'}
            or type(value['allocated_reference']) is not str
            or re.fullmatch(re.escape(PREFIX) + '[0-9a-f]{32}', value['allocated_reference']) is None):
        raise ValueError('synthetic allocation contract')
    return value['allocated_reference']


def seed_helper_record(work, previous_reference):
    assert sys.platform == 'linux' and os.geteuid() == 0
    assert Path('/.dockerenv').is_file()
    assert os.environ.get('PERCIVAL_ISOLATED_LINUX_FIXTURE') == '1'
    assert os.environ.get('PERCIVAL_KNOWN_SOURCE_CONTRACT') == '1'
    assert HELPER.is_file() and not ROOT.exists() and not ROOT.is_symlink()
    process = None
    home = work / 'helper-home'
    assert not home.exists()
    ROOT.mkdir(mode=0o755)
    try:
        data = ROOT / 'data'
        data.mkdir(mode=0o755)
        os.chown(data, 999, 987)
        target = data / 'aiua_context.db'
        # Closed fixture snapshots only. No runtime database is transferred.
        with closing(sqlite3.connect(work / 'aiua_context.db')) as src, closing(sqlite3.connect(target)) as dest:
            src.backup(dest)
        target.chmod(0o600)
        os.chown(target, 999, 987)
        (home / '.philotic').mkdir(parents=True, mode=0o700)
        for p in (home, home / '.philotic'):
            p.chmod(0o700)
            os.chown(p, 999, 987)
        deadline = time.monotonic() + 10
        parent, child = socket.socketpair()
        with parent, child:
            parent.settimeout(5)
            process = subprocess.Popen(
                [str(HELPER), '--apply-scoped-persistent', '--root-private-start', str(child.fileno())],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                env={'PATH': '/usr/bin:/bin', 'HOME': str(home), 'PHILOTIC_VAULT_KEYCHAIN': '0'},
                pass_fds=(child.fileno(),), close_fds=True, bufsize=0)
            child.close()
            ready = b''
            while len(ready) < 6:
                part = parent.recv(6 - len(ready))
                if not part:
                    raise ValueError('synthetic helper readiness ended')
                ready += part
            assert ready == b'READY\n'
            parent.sendall(b'PHILOTIC_VAULT_MASTER_KEY=' + KEY + b'\n')
            parent.shutdown(socket.SHUT_WR)
            request = {'operation': 'create_introspection', 'kind': 'percival-issuer-introspection',
                       'scope': 'percival_gateway_persistent_recall_v1',
                       'allowed_roles': ['percival-personal-recall'],
                       'allowed_guests': ['percival-personal-gateway'], 'secret': VALUE}
            process.stdin.write(json.dumps(request).encode() + b'\n')
            reference = allocated(line(process.stdout, deadline))
            process.stdin.write(json.dumps({'commit': reference}).encode() + b'\n')
            assert line(process.stdout, deadline) == {'reference': reference}
            out, err = process.communicate(timeout=max(.01, deadline - time.monotonic()))
            assert process.returncode == 0 and not out and not err
        with closing(sqlite3.connect(target)) as src:
            row = src.execute('SELECT data_json FROM graph_nodes WHERE node_key=?', ('secret:' + reference,)).fetchone()
            assert row
            record = json.loads(row[0])
            assert record['scope'] == 'percival_gateway_persistent_recall_v1'
            assert record['allowed_roles'] == ['percival-personal-recall']
            assert record['allowed_guests'] == ['percival-personal-gateway']
            src.execute('DELETE FROM graph_nodes WHERE node_key=?', ('secret:' + previous_reference,))
            src.commit()
            with closing(sqlite3.connect(work / 'aiua_context.db')) as dest:
                src.backup(dest)
        return reference
    finally:
        if process is not None and process.poll() is None:
            process.kill()
            process.communicate(timeout=3)
        shutil.rmtree(ROOT)
        shutil.rmtree(home, ignore_errors=True)
