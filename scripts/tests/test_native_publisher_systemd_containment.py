#!/usr/bin/env python3
"""Disposable Linux experiment; never target an existing unit or production."""
import os
import pathlib
import signal
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
import uuid


def service(root):
    root = pathlib.Path(root)
    child = os.fork()
    if child == 0:
        os.setsid()
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        (root / 'descendant.tmp').write_text(str(os.getpid()))
        os.replace(root / 'descendant.tmp', root / 'descendant')
        i = 0
        while True:
            (root / 'effect').write_text(str(i))
            i += 1
            time.sleep(0.03)
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    (root / 'leader.tmp').write_text(str(os.getpid()))
    os.replace(root / 'leader.tmp', root / 'leader')
    os.waitpid(child, 0)


def cmd(*args, timeout=10):
    return subprocess.run(args, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, timeout=timeout, check=False)


class SystemdContainment(unittest.TestCase):
    def test_disposable_unit_stop_contains_setsid_descendant(self):
        if os.environ.get('PHILOTIC_DISPOSABLE_SYSTEMD_TEST') != '1':
            self.skipTest('requires explicit disposable Linux systemd CI')
        self.assertEqual(os.geteuid(), 0)
        self.assertTrue(pathlib.Path('/run/systemd/system').is_dir())
        runner = cmd('/usr/bin/id', '-u', 'runner')
        self.assertEqual(runner.returncode, 0, runner.stderr)
        self.assertGreater(int(runner.stdout.strip()), 0)
        self.assertTrue(pathlib.Path('/sys/fs/cgroup/cgroup.controllers').is_file(), 'requires cgroup v2')
        unit = 'philotic-native-qualify-' + uuid.uuid4().hex + '.service'
        absent = cmd('/usr/bin/systemctl', 'show', unit, '--property=LoadState')
        self.assertIn('LoadState=not-found', absent.stdout.splitlines())
        directory = tempfile.mkdtemp(prefix='philotic-containment-')
        cg_events = pathlib.Path('/sys/fs/cgroup/system.slice') / unit / 'cgroup.events'
        try:
            start = cmd('/usr/bin/systemd-run', '--system', '--unit=' + unit,
            '--slice=system.slice', '--property=Type=exec', '--property=User=root',
            '--property=KillMode=control-group', '--property=TimeoutStopSec=2s',
            '--property=RuntimeMaxSec=45s', '--property=NoNewPrivileges=yes',
            '--property=CapabilityBoundingSet=', '--property=ProtectControlGroups=yes',
            '--property=ProtectSystem=strict', '--property=ReadWritePaths=' + directory,
            '--property=PrivateNetwork=yes', '--property=UMask=0077',
            '/usr/bin/python3', str(pathlib.Path(__file__).resolve()), '--service', directory)
            self.assertEqual(start.returncode, 0, start.stderr)
            root = pathlib.Path(directory)
            limit = time.monotonic() + 12
            while not all((root / name).is_file() for name in ('leader', 'descendant', 'effect')):
                self.assertLess(time.monotonic(), limit, 'service did not start')
                time.sleep(0.05)
            main = int((root / 'leader').read_text())
            desc = int((root / 'descendant').read_text())
            self.assertNotEqual(main, desc)
            props = cmd('/usr/bin/systemctl', 'show', unit, '--property=MainPID',
                        '--property=ControlGroup', '--property=InvocationID')
            self.assertEqual(props.returncode, 0, props.stderr)
            values = dict(x.split('=', 1) for x in props.stdout.splitlines() if '=' in x)
            self.assertEqual(int(values['MainPID']), main)
            self.assertTrue(values['ControlGroup'].endswith('/' + unit))
            self.assertEqual(len(values['InvocationID']), 32)
            for pid in (main, desc):
                status = pathlib.Path('/proc', str(pid), 'status').read_text().splitlines()
                uids = next(x for x in status if x.startswith('Uid:')).split()[1:]
                self.assertEqual(uids, ['0', '0', '0', '0'])
                membership = pathlib.Path('/proc', str(pid), 'cgroup').read_text().splitlines()
                self.assertIn('0::' + values['ControlGroup'], membership)
            events = pathlib.Path('/sys/fs/cgroup') / values['ControlGroup'].lstrip('/') / 'cgroup.events'
            self.assertTrue(events.is_file(), 'cgroup v2 population evidence unavailable')
            self.assertIn('populated 1', events.read_text().splitlines())
            denied = cmd('/usr/sbin/runuser', '-u', 'runner', '--', '/usr/bin/systemctl',
                         '--no-ask-password', 'stop', unit, timeout=8)
            self.assertNotEqual(denied.returncode, 0, 'service account stopped root unit')
            self.assertEqual(cmd('/usr/bin/systemctl', 'is-active', unit).returncode, 0)
            stopped = cmd('/usr/bin/systemctl', '--no-ask-password', 'stop', unit, timeout=12)
            self.assertEqual(stopped.returncode, 0, stopped.stderr)
            self.assertNotEqual(cmd('/usr/bin/systemctl', 'is-active', unit).returncode, 0)
            if events.exists():
                self.assertIn('populated 0', events.read_text().splitlines())
            before = (root / 'effect').read_bytes()
            time.sleep(0.5)
            self.assertEqual((root / 'effect').read_bytes(), before,
                             'descendant still made an effect after stop')
        finally:
            # Reconcile the exact fresh unit even if start acknowledgement failed.
            # Preserve synthetic evidence if either service or cgroup is uncertain.
            try:
                cmd('/usr/bin/systemctl', '--no-ask-password', 'stop', unit, timeout=12)
                state = cmd('/usr/bin/systemctl', 'show', unit, '--property=LoadState', '--property=ActiveState')
                fields = dict(x.split('=', 1) for x in state.stdout.splitlines() if '=' in x)
                inactive = fields.get('LoadState') == 'not-found' or (
                    state.returncode == 0 and fields.get('ActiveState') in ('inactive', 'failed'))
                empty = not cg_events.exists() or 'populated 0' in cg_events.read_text().splitlines()
                if not inactive or not empty:
                    raise RuntimeError('unit or cgroup state uncertain')
            except Exception as error:
                raise RuntimeError('synthetic evidence retained at ' + directory) from error
            else:
                shutil.rmtree(directory)


if __name__ == '__main__':
    if len(sys.argv) == 3 and sys.argv[1] == '--service':
        service(sys.argv[2])
    else:
        unittest.main()
