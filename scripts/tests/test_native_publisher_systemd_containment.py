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
        decoy = 'philotic-native-decoy-' + uuid.uuid4().hex + '.service'
        rule = pathlib.Path('/etc/polkit-1/rules.d') / ('49-' + unit + '.rules')
        rule_owned = False
        rule_created = False
        absent = cmd('/usr/bin/systemctl', 'show', unit, '--property=LoadState')
        self.assertIn('LoadState=not-found', absent.stdout.splitlines())
        directory = tempfile.mkdtemp(prefix='philotic-containment-')
        cg_events = pathlib.Path('/sys/fs/cgroup/system.slice') / unit / 'cgroup.events'
        decoy_events = pathlib.Path('/sys/fs/cgroup/system.slice') / decoy / 'cgroup.events'
        # The restricted root service has no DAC override; do not depend on
        # traversing the runner's private home directory.
        fixture_script = pathlib.Path(directory) / 'fixture.py'
        try:
            shutil.copyfile(pathlib.Path(__file__).resolve(), fixture_script)
            os.chmod(fixture_script, 0o600)
            start = cmd('/usr/bin/systemd-run', '--system', '--unit=' + unit,
            '--slice=system.slice', '--property=Type=exec', '--property=User=root',
            '--property=KillMode=control-group', '--property=TimeoutStopSec=2s',
            '--property=RuntimeMaxSec=45s', '--property=NoNewPrivileges=yes',
            '--property=CapabilityBoundingSet=', '--property=ProtectControlGroups=yes',
            '--property=ProtectSystem=strict', '--property=ReadWritePaths=' + directory,
            '--property=PrivateNetwork=yes', '--property=UMask=0077',
            '/usr/bin/python3', str(fixture_script), '--service', directory)
            self.assertEqual(start.returncode, 0, start.stderr)
            root = pathlib.Path(directory)
            limit = time.monotonic() + 12
            while not all((root / name).is_file() for name in ('leader', 'descendant', 'effect')):
                if time.monotonic() >= limit or cmd('/usr/bin/systemctl', 'is-failed', unit).returncode == 0:
                    state = cmd('/usr/bin/systemctl', 'show', unit, '--property=ActiveState',
                                '--property=Result', '--property=ExecMainStatus')
                    journal = cmd('/usr/bin/journalctl', '-u', unit, '-n', '15', '--no-pager')
                    self.fail('synthetic service did not start: ' + state.stdout + journal.stdout)
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
            decoy_start = cmd('/usr/bin/systemd-run', '--system', '--unit=' + decoy,
                              '--property=RuntimeMaxSec=45s', '/usr/bin/sleep', '45')
            self.assertEqual(decoy_start.returncode, 0, decoy_start.stderr)
            # This exact temporary rule grants the disposable runner only the
            # stop verb on this randomly named synthetic unit.
            rule_text = ('polkit.addRule(function(action, subject) {\n'
                         ' if (action.id == "org.freedesktop.systemd1.manage-units" && '
                         'subject.user == "runner" && action.lookup("unit") == "' + unit +
                         '" && action.lookup("verb") == "stop") return polkit.Result.YES;\n'
                         '});\n')
            fd = os.open(rule, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o644)
            rule_owned = True
            rule_created = True
            with os.fdopen(fd, 'w') as handle:
                handle.write(rule_text)
                handle.flush()
                os.fsync(handle.fileno())
            # polkit reloads its rules asynchronously; negative checks alone
            # cannot establish that this rule has loaded.
            time.sleep(2)
            policy_check = cmd('/usr/sbin/runuser', '-u', 'runner', '--', '/bin/sh', '-c',
                               'exec /usr/bin/pkcheck --action-id org.freedesktop.systemd1.manage-units '
                               '--process $$ --detail unit "$1" --detail verb stop', 'policy-check', unit,
                               timeout=8)
            self.assertEqual(policy_check.returncode, 0, 'temporary exact policy not active')
            denied_restart = cmd('/usr/sbin/runuser', '-u', 'runner', '--', '/usr/bin/systemctl',
                                 '--no-ask-password', 'restart', unit, timeout=8)
            self.assertNotEqual(denied_restart.returncode, 0, 'restart unexpectedly allowed')
            denied_decoy = cmd('/usr/sbin/runuser', '-u', 'runner', '--', '/usr/bin/systemctl',
                               '--no-ask-password', 'stop', decoy, timeout=8)
            self.assertNotEqual(denied_decoy.returncode, 0, 'decoy stop unexpectedly allowed')
            self.assertEqual(cmd('/usr/bin/systemctl', 'is-active', decoy).returncode, 0)
            stopped = cmd('/usr/sbin/runuser', '-u', 'runner', '--', '/usr/bin/systemctl',
                          '--no-ask-password', 'stop', unit, timeout=12)
            self.assertEqual(stopped.returncode, 0, stopped.stderr)
            self.assertNotEqual(cmd('/usr/bin/systemctl', 'is-active', unit).returncode, 0)
            if events.exists():
                self.assertIn('populated 0', events.read_text().splitlines())
            before = (root / 'effect').read_bytes()
            time.sleep(0.5)
            self.assertEqual((root / 'effect').read_bytes(), before,
                             'descendant still made an effect after stop')
            rule.unlink()
            rule_owned = False
            self.assertFalse(rule.exists(), 'temporary rule was not removed')
            # Recreate only the same synthetic unit as root and verify that
            # removing the rule also removes the runner's stop authorization.
            # The polkit directory watcher handles rule reload asynchronously.
            time.sleep(2)
            recreated = cmd('/usr/bin/systemd-run', '--system', '--unit=' + unit,
                            '--property=RuntimeMaxSec=30s', '/usr/bin/sleep', '30')
            self.assertEqual(recreated.returncode, 0, recreated.stderr)
            denied_after_cleanup = cmd('/usr/sbin/runuser', '-u', 'runner', '--',
                                       '/usr/bin/systemctl', '--no-ask-password', 'stop', unit,
                                       timeout=8)
            self.assertNotEqual(denied_after_cleanup.returncode, 0,
                                'stop allowed after temporary rule removal')
            self.assertEqual(cmd('/usr/bin/systemctl', 'is-active', unit).returncode, 0)
        finally:
            # Reconcile the exact fresh unit even if start acknowledgement failed.
            # Preserve synthetic evidence if either service or cgroup is uncertain.
            try:
                rule_error = None
                try:
                    if rule_owned:
                        rule.unlink(missing_ok=True)
                        if rule.exists():
                            raise RuntimeError('temporary polkit rule remains')
                        rule_owned = False
                except Exception as error:
                    rule_error = error
                cleanup_errors = []
                if rule_created and not rule_owned:
                    try:
                        time.sleep(2)
                        revoked = cmd('/usr/sbin/runuser', '-u', 'runner', '--', '/bin/sh', '-c',
                                      'exec /usr/bin/pkcheck --action-id org.freedesktop.systemd1.manage-units '
                                      '--process $$ --detail unit "$1" --detail verb stop',
                                      'policy-check', unit, timeout=8)
                        if revoked.returncode != 1:
                            raise RuntimeError('temporary policy revocation not confirmed')
                    except Exception as error:
                        cleanup_errors.append(error)
                for name in (unit, decoy):
                    try:
                        cmd('/usr/bin/systemctl', '--no-ask-password', 'stop', name, timeout=12)
                    except Exception as error:
                        cleanup_errors.append(error)
                state = cmd('/usr/bin/systemctl', 'show', unit, '--property=LoadState', '--property=ActiveState')
                fields = dict(x.split('=', 1) for x in state.stdout.splitlines() if '=' in x)
                inactive = fields.get('LoadState') == 'not-found' or (
                    state.returncode == 0 and fields.get('ActiveState') in ('inactive', 'failed'))
                decoy_state = cmd('/usr/bin/systemctl', 'show', decoy, '--property=LoadState', '--property=ActiveState')
                decoy_fields = dict(x.split('=', 1) for x in decoy_state.stdout.splitlines() if '=' in x)
                decoy_inactive = decoy_fields.get('LoadState') == 'not-found' or (
                    decoy_state.returncode == 0 and decoy_fields.get('ActiveState') in ('inactive', 'failed'))
                empty = not cg_events.exists() or 'populated 0' in cg_events.read_text().splitlines()
                decoy_empty = not decoy_events.exists() or 'populated 0' in decoy_events.read_text().splitlines()
                if rule_error or cleanup_errors or not inactive or not decoy_inactive or not empty or not decoy_empty:
                    raise RuntimeError('rule, unit, decoy, or cgroup state uncertain') from rule_error
            except Exception as error:
                raise RuntimeError('synthetic evidence retained at ' + directory) from error
            else:
                shutil.rmtree(directory)


if __name__ == '__main__':
    if len(sys.argv) == 3 and sys.argv[1] == '--service':
        service(sys.argv[2])
    else:
        unittest.main()
