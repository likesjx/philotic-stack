#!/usr/bin/env python3
"""Unwired synthetic systemd/socket rehearsal. Never target production.

Fresh exclusive GitHub-hosted VM is an external prerequisite; environment flags
are only an accidental-execution guard. Synthetic masks are created under /etc
and persist until checked cleanup. No polkit rules or account changes.
"""
import hashlib
import os
import pathlib
import re
import socket
import stat
import subprocess
import sys
import tempfile
import time
import uuid

class Stop(Exception):
    pass


def unit_names(token):
    if not re.fullmatch(r'[0-9a-f]{32}', token):
        raise Stop('invalid synthetic unit identity')
    prefix = 'philotic-synthetic-socket-' + token
    return prefix + '.service', prefix + '.socket'


def command(*args, timeout=10):
    try:
        return subprocess.run(['/usr/bin/systemctl', '--no-ask-password', *args],
                              stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, timeout=timeout, check=False,
                              env={'LC_ALL':'C', 'PATH':'/usr/bin:/bin'})
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise Stop('manager result uncertain') from exc


def must(*args, timeout=10):
    result = command(*args, timeout=timeout)
    if result.returncode != 0:
        raise Stop('manager operation failed or denied')
    return result.stdout.decode('utf-8', errors='strict').strip()


def prop(unit, name):
    return must('show', unit, '--property=' + name, '--value')


def safe_directory(path):
    m = os.lstat(path)
    if not stat.S_ISDIR(m.st_mode) or m.st_uid != 0 or m.st_mode & 0o022:
        raise Stop('synthetic manager directory unsafe')
    return (m.st_dev, m.st_ino)


def create_unit(path, content):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o644)
    try:
        m = os.fstat(fd)
        if not stat.S_ISREG(m.st_mode) or m.st_uid != 0 or m.st_nlink != 1:
            raise Stop('synthetic unit identity invalid')
        with os.fdopen(fd, 'wb', closefd=False) as out:
            out.write(content)
            out.flush()
            os.fsync(out.fileno())
        return (m.st_dev, m.st_ino, hashlib.sha256(content).hexdigest())
    finally:
        os.close(fd)


def same_unit(path, identity):
    try:
        m = os.lstat(path)
        if not stat.S_ISREG(m.st_mode) or m.st_uid != 0 or m.st_nlink != 1 or (m.st_dev, m.st_ino) != identity[:2]:
            return False
        with open(path, 'rb') as source:
            return hashlib.sha256(source.read(4097)).hexdigest() == identity[2]
    except OSError:
        return False


def masked(path):
    try:
        m = os.lstat(path)
        return stat.S_ISLNK(m.st_mode) and m.st_uid == 0 and os.readlink(path) == '/dev/null'
    except OSError:
        return False


def require_masked_start_denial(result, unit):
    diagnostic = result.stderr.decode('utf-8', errors='replace')[:4096]
    if result.returncode == 0 or ('Unit ' + unit + ' is masked') not in diagnostic:
        raise Stop('synthetic masked start outcome uncertain')


def cgroup_empty(cgroup):
    if not re.fullmatch(r'/system\.slice/philotic-synthetic-(socket|domain)-[0-9a-f]{32}\.service', cgroup):
        return False
    if not pathlib.Path('/sys/fs/cgroup/cgroup.controllers').is_file():
        return False
    events = pathlib.Path('/sys/fs/cgroup') / cgroup.lstrip('/') / 'cgroup.events'
    try:
        return 'populated 0' in events.read_text().splitlines()
    except FileNotFoundError:
        return True  # A removed cgroup-v2 directory cannot remain populated.
    except OSError:
        return False


def verified_domain_cgroup(unit):
    if not re.fullmatch(r'philotic-synthetic-domain-[0-9a-f]{32}\.service', unit):
        raise Stop('domain unit identity invalid')
    expected = '/system.slice/' + unit
    if prop(unit, 'Slice') != 'system.slice' or prop(unit, 'ControlGroup') not in ('', expected):
        raise Stop('synthetic domain cgroup unknown')
    return expected


def wait_for(predicate, limit=10):
    end = time.monotonic() + limit
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(0.05)
    raise Stop('synthetic activation was not observed')


def run(binary, expected_sha):
    if not (sys.platform == 'linux' and os.geteuid() == 0
            and os.environ.get('GITHUB_ACTIONS') == 'true'
            and os.environ.get('RUNNER_ENVIRONMENT') == 'github-hosted'
            and os.environ.get('PHILOTIC_DISPOSABLE_SYSTEMD_SOCKET') == '1'):
        raise Stop('fresh hosted disposable root runner required')
    binary = pathlib.Path(binary)
    target = pathlib.Path(os.environ.get('RUNNER_TEMP', '/nonexistent')) / 'synthetic-custody-target'
    if binary.is_symlink() or target.resolve() not in binary.resolve().parents or not binary.is_file():
        raise Stop('synthetic binary location invalid')
    if not re.fullmatch(r'[0-9a-f]{64}', expected_sha):
        raise Stop('synthetic binary digest invalid')
    run_dir = pathlib.Path('/run'); unit_dir = run_dir / 'systemd/system'; mask_dir = pathlib.Path('/etc/systemd/system')
    cgroup_root = pathlib.Path('/sys/fs/cgroup')
    identities = {p: safe_directory(p) for p in (run_dir, unit_dir, mask_dir, cgroup_root)}
    if not (cgroup_root / 'cgroup.controllers').is_file():
        raise Stop('cgroup v2 unavailable')
    if pathlib.Path('/proc/1/comm').read_text().strip() != 'systemd':
        raise Stop('systemd manager unavailable')
    token = uuid.uuid4().hex
    service, sock = unit_names(token)
    domain = 'philotic-synthetic-domain-' + token + '.service'
    units = (service, sock)
    sources = {u: unit_dir / u for u in units}
    masks = {u: mask_dir / u for u in units}
    for unit in (*units, domain):
        if os.path.lexists(unit_dir / unit) or os.path.lexists(mask_dir / unit) or prop(unit, 'LoadState') != 'not-found':
            raise Stop('synthetic unit identity already exists')
    root = pathlib.Path(tempfile.mkdtemp(prefix='philotic-systemd-fixture-', dir=run_dir))
    root_id = (os.lstat(root).st_dev, os.lstat(root).st_ino)
    os.chmod(root, 0o700)
    sock_path = root / 'activation.sock'
    contents = {
        service: b'[Unit]\nDescription=Synthetic custody service\n[Service]\nType=simple\nExecStart=/usr/bin/sleep 120\nKillMode=control-group\nRestart=no\nTimeoutStopSec=5\nRuntimeMaxSec=120\n',
        sock: ('[Unit]\nDescription=Synthetic custody socket\n[Socket]\nListenStream=' + str(sock_path) + '\nSocketMode=0600\nRemoveOnStop=yes\nService=' + service + '\n').encode(),
    }
    owned = {}; stop_attempted = set(); mask_attempted = set(); manager_uncertain = False
    domain_submitted = False; domain_cgroup = None; binary_copy = root / 'fixture-binary'
    error = None
    def mutate(*args):
        nonlocal manager_uncertain
        try:
            return must(*args)
        except Exception:
            manager_uncertain = True
            raise
    try:
        # Pin a private root-owned executable snapshot before starting systemd.
        # The input digest must be supplied by the independently reviewed job.
        src = os.open(binary, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            meta = os.fstat(src)
            if not stat.S_ISREG(meta.st_mode) or meta.st_size > 512 * 1024 * 1024:
                raise Stop('synthetic binary invalid')
            dst = os.open(binary_copy, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o500)
            hasher = hashlib.sha256()
            try:
                with os.fdopen(dst, 'wb', closefd=False) as out:
                    while True:
                        block = os.read(src, 1024 * 1024)
                        if not block: break
                        hasher.update(block); out.write(block)
                    out.flush(); os.fsync(out.fileno())
            finally:
                os.close(dst)
            if hasher.hexdigest() != expected_sha: raise Stop('synthetic binary digest mismatch')
        finally:
            os.close(src)
        for unit in units:
            # Track creation even if writing later fails.
            try:
                owned[unit] = create_unit(sources[unit], contents[unit])
            except Exception:
                manager_uncertain = True
                raise
        mutate('daemon-reload')
        mutate('start', sock)
        if prop(sock, 'ActiveState') != 'active': raise Stop('synthetic socket not active')
        m = os.lstat(sock_path)
        if not stat.S_ISSOCK(m.st_mode) or m.st_uid != 0: raise Stop('synthetic socket identity invalid')
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(3); client.connect(str(sock_path))
        wait_for(lambda: prop(service, 'ActiveState') == 'active')
        pid = int(prop(service, 'MainPID')); cgroup = prop(service, 'ControlGroup')
        if pid <= 1 or cgroup != '/system.slice/' + service: raise Stop('synthetic service identity invalid')
        events = pathlib.Path('/sys/fs/cgroup') / cgroup.lstrip('/') / 'cgroup.events'
        if 'populated 1' not in events.read_text().splitlines(): raise Stop('synthetic service population unknown')
        for unit in (sock, service):
            stop_attempted.add(unit); mutate('stop', unit)
        if prop(service, 'MainPID') != '0' or prop(service, 'ActiveState') != 'inactive' or not cgroup_empty(cgroup):
            raise Stop('synthetic service not quiescent')
        for unit in units:
            mask_attempted.add(unit); mutate('mask', unit)
            if not masked(masks[unit]): raise Stop('synthetic mask identity unknown')
        mutate('daemon-reload')
        for unit in units:
            if prop(unit, 'LoadState') != 'masked': raise Stop('synthetic mask not loaded')
            try:
                denied = command('start', unit)
                require_masked_start_denial(denied, unit)
            except Exception:
                manager_uncertain = True
                raise
        if os.path.lexists(sock_path): raise Stop('synthetic socket remained after stop')
        # The domain fixture and its descendants are managed in their own
        # cgroup, so a timeout can be stopped and checked without guessing PIDs.
        domain_submitted = True
        try:
            launched = subprocess.run(['/usr/bin/systemd-run', '--system', '--no-block', '--unit=' + domain,
            '--property=Type=oneshot', '--property=RemainAfterExit=yes', '--property=KillMode=control-group',
            '--property=TimeoutStartSec=90s', '--setenv=GITHUB_ACTIONS=true',
            '--setenv=RUNNER_ENVIRONMENT=github-hosted', '--setenv=PHILOTIC_DISPOSABLE_LINUX_CUSTODY=1',
            str(binary_copy), '--exact', 'linux_custody_fixture::disposable_linux_filesystem_and_interruption', '--ignored', '--nocapture'],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10, check=False)
        except (OSError, subprocess.TimeoutExpired):
            manager_uncertain = True
            raise
        if launched.returncode != 0:
            manager_uncertain = True
            raise Stop('synthetic domain unit submission failed or uncertain')
        wait_for(lambda: prop(domain, 'ActiveState') in ('active', 'failed'), limit=95)
        if prop(domain, 'ActiveState') != 'active' or prop(domain, 'SubState') != 'exited' or prop(domain, 'Result') != 'success' or prop(domain, 'ExecMainStatus') != '0':
            raise Stop('synthetic domain unit failed or uncertain')
        invocation = prop(domain, 'InvocationID')
        if not re.fullmatch(r'[0-9a-f]{32}', invocation): raise Stop('synthetic domain invocation unknown')
        domain_cgroup = verified_domain_cgroup(domain)
        journal = subprocess.run(['/usr/bin/journalctl', '--no-pager', '--output=cat', '_SYSTEMD_INVOCATION_ID=' + invocation],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=False)
        output = journal.stdout.decode('utf-8', errors='replace')
        if (journal.returncode != 0 or 'test result: ok. 1 passed; 0 failed;' not in output
                or 'synthetic_domain_success_and_endpoint_revocation_observed' not in output
                or 'synthetic_domain_interrupt_and_later_endpoint_revocation_observed' not in output):
            raise Stop('synthetic domain fixture failed or was not selected')
        for unit in units:
            if prop(unit, 'LoadState') != 'masked': raise Stop('synthetic mask changed during operation')
        if not cgroup_empty(cgroup): raise Stop('synthetic service cgroup repopulated')
        print('synthetic_socket_masked_during_domain_and_interruption=passed')
    except BaseException as exc:
        error = exc
    finally:
        problems = []
        if domain_submitted:
            try:
                observed_cgroup = verified_domain_cgroup(domain)
                if domain_cgroup is None: domain_cgroup = observed_cgroup
                if domain_cgroup != observed_cgroup: raise Stop('domain cgroup changed')
                must('stop', domain)
                if prop(domain, 'ActiveState') != 'inactive' or not cgroup_empty(domain_cgroup):
                    raise Stop('domain descendants not quiescent')
            except Exception: problems.append('domain-cleanup-uncertain')
        # Never repeat a failed/uncertain stop; preserve the units for VM disposal.
        for unit in (sock, service):
            try:
                active = prop(unit, 'ActiveState')
                if active not in ('inactive', 'failed'):
                    if unit in stop_attempted: problems.append('stop-uncertain')
                    else: stop_attempted.add(unit); must('stop', unit)
            except Exception: problems.append('stop-unverified')
        if manager_uncertain: problems.append('unit-creation-uncertain')
        if not problems:
            for unit in units:
                try:
                    if os.path.lexists(masks[unit]):
                        if unit not in mask_attempted or not masked(masks[unit]): raise Stop('mask ownership uncertain')
                        must('unmask', unit)
                    if unit in owned:
                        if not same_unit(sources[unit], owned[unit]): raise Stop('unit identity uncertain')
                        os.unlink(sources[unit])
                except Exception: problems.append('unit-cleanup-uncertain')
            if not problems:
                try: must('daemon-reload')
                except Exception: problems.append('manager-reload-uncertain')
        if not problems:
            try:
                if any(os.path.lexists(sources[u]) or os.path.lexists(masks[u]) for u in units): raise Stop('unit file remains')
                if any(safe_directory(p) != identities[p] for p in identities): raise Stop('manager directory changed')
                m = os.lstat(root)
                if (m.st_dev, m.st_ino) != root_id or not stat.S_ISDIR(m.st_mode): raise Stop('fixture identity changed')
                if os.path.lexists(binary_copy):
                    m = os.lstat(binary_copy)
                    if not stat.S_ISREG(m.st_mode) or m.st_uid != 0 or m.st_mode & 0o7777 != 0o500: raise Stop('binary copy identity uncertain')
                    os.unlink(binary_copy)
                os.rmdir(root)
            except Exception: problems.append('fixture-cleanup-uncertain')
        if problems:
            raise Stop('synthetic cleanup uncertain: ' + ','.join(sorted(set(problems)))) from error
    if error:
        raise Stop('synthetic rehearsal failed') from error
    print('synthetic_systemd_cleanup=confirmed')


if __name__ == '__main__':
    if len(sys.argv) != 3:
        raise SystemExit('usage: systemd_socket_rehearsal.py BINARY SHA256')
    try:
        run(sys.argv[1], sys.argv[2])
    except Stop as exc:
        print(str(exc), file=sys.stderr)
        raise SystemExit(1)
