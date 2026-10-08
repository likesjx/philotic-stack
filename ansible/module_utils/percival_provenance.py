"""Read-only hotel executable provenance; never return unit argv/environment."""
import hashlib
import os
import re
import stat
import subprocess


class ProvenanceError(Exception):
    pass


def systemd_properties():
    result = subprocess.run(
        ['systemctl', 'show', 'philotic-hotel.service', '--property=ExecStart',
         '--property=MainPID', '--property=NeedDaemonReload'],
        capture_output=True, text=True, timeout=10, check=False)
    if result.returncode:
        raise ProvenanceError('Cannot inspect effective hotel service properties')
    values = {}
    for line in result.stdout.splitlines():
        key, separator, value = line.partition('=')
        if not separator or key not in ('ExecStart', 'MainPID', 'NeedDaemonReload') or key in values:
            raise ProvenanceError('Unexpected hotel service properties')
        values[key] = value
    return values


def artifact(path):
    if not os.path.isabs(path):
        raise ProvenanceError('Hotel executable must be an absolute path')
    resolved = os.path.realpath(path)
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NONBLOCK), 'rb') as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or not before.st_mode & 0o111:
            raise ProvenanceError('Hotel artifact is not an executable file')
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        after = os.fstat(stream.fileno())
    current = os.stat(path)
    identity = (before.st_dev, before.st_ino)
    if (after.st_size, after.st_mtime_ns, after.st_ctime_ns) != (before.st_size, before.st_mtime_ns, before.st_ctime_ns) or identity != (current.st_dev, current.st_ino):
        raise ProvenanceError('Hotel artifact changed during verification')
    return resolved, digest, identity


def verify(properties, approved_path, approved_sha256, require_running=False, proc_root='/proc'):
    if properties.get('NeedDaemonReload') != 'no':
        raise ProvenanceError('Hotel unit needs an operator-approved daemon reload before rollout')
    command = properties.get('ExecStart', '')
    # Fail closed on wrappers, multiple commands, unsupported escaping or spaces.
    paths = re.findall(r'(?:^|\{ )path=([^ ;{}]+) ;', command)
    if len(paths) != 1 or not command.startswith('{ path='):
        raise ProvenanceError('Cannot identify exactly one effective hotel executable')
    approved = artifact(approved_path)
    effective = artifact(paths[0])
    if approved[1] != approved_sha256 or effective != approved:
        raise ProvenanceError('Effective ExecStart does not resolve to the approved hotel artifact')
    result = {'resolved_executable': approved[0], 'sha256': approved[1]}
    if require_running:
        pid = properties.get('MainPID', '')
        if not re.fullmatch(r'[1-9][0-9]*', pid):
            raise ProvenanceError('Hotel has no running main process')
        executable = os.path.join(proc_root, pid, 'exe')
        if os.readlink(executable).endswith(' (deleted)'):
            raise ProvenanceError('Running hotel executable has been deleted')
        if artifact(executable) != approved:
            raise ProvenanceError('Running hotel executable differs from the approved artifact')
        result['main_pid'] = int(pid)
    return result
