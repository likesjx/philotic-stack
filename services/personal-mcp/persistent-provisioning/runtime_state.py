"""State backup primitive for the existing owner operator; no CLI or import IO.

The trusted operator must bind exact profile/database paths, stop approved writer
services and hold its transaction lock before calling. This copies private data
server-locally and opens sources mode=rw for SQLite writer barriers, not inspection.
No function here authorizes a cutover or proves all state paths have been found.
"""
from contextlib import ExitStack, closing
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import stat
import tempfile
import time
from provision import require

MAX_BYTES = 1024 * 1024 * 1024


def identity(path):
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1)
    return {'dev': info.st_dev, 'ino': info.st_ino, 'size': info.st_size,
            'uid': info.st_uid, 'gid': info.st_gid, 'mode': stat.S_IMODE(info.st_mode),
            'mtime_ns': info.st_mtime_ns, 'ctime_ns': info.st_ctime_ns}


def digest_file(path, check=lambda: None):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        while True:
            check()
            part = stream.read(65536)
            if not part:break
            value.update(part)
    check()
    return value.hexdigest()


def write_private(path, value):
    raw = json.dumps(value, sort_keys=True, separators=(',', ':')).encode()
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        offset = 0
        while offset < len(raw):
            count = os.write(fd, raw[offset:])
            require(count > 0)
            offset += count
        os.fsync(fd)
    finally:
        os.close(fd)


def snapshot(paths, target, assert_quiescent, *, data_identity=(999, 987), timeout=30):
    """Actual SQLite backup with both writer barriers held, no automatic cleanup.

    paths are the exact approved context/training pair, context first; training
    absence is preserved. The private parent is precreated by the owner installer.
    Failed or interrupted snapshots remain for reconciliation, never overwrite.
    Private manifest hashes/metadata are not a public receipt or model output.
    Deadline checks are cooperative; the owner runner must enforce an outer
    process deadline. A manifest alone never authorizes the next cutover effect.
    """
    require(type(paths) in (list, tuple) and len(paths) == 2)
    paths = [Path(path) for path in paths]
    target = Path(target)
    require(all(path.is_absolute() and '..' not in path.parts for path in paths))
    require(paths[0].name == 'aiua_context.db' and paths[1].name == 'whisper_training.db')
    require(paths[0].parent == paths[1].parent and paths[0].exists())
    require(0 < timeout <= 60)
    parent = target.parent.lstat()
    require(stat.S_ISDIR(parent.st_mode) and parent.st_uid == os.geteuid()
            and stat.S_IMODE(parent.st_mode) == 0o700)
    assert_quiescent()  # Trusted service/writer adapter, not a manifest boolean.
    before = {}
    for path in paths:
        require(not path.is_symlink())
        info = identity(path) if path.exists() else None
        if info:
            require((info['uid'], info['gid']) == data_identity and info['mode'] in (0o600, 0o644)
                    and 512 <= info['size'] <= MAX_BYTES)
        before[path.name] = info
    require(len({(info['dev'], info['ino']) for info in before.values() if info})
            == sum(info is not None for info in before.values()))
    target.mkdir(mode=0o700)  # Exclusive creation; partial evidence is preserved.
    deadline = time.monotonic() + timeout
    records = {}

    def remaining():
        require(time.monotonic() < deadline)

    with ExitStack() as stack:
        parent_fd = os.open(target.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        stack.callback(os.close, parent_fd)
        actual_parent = os.fstat(parent_fd)
        require((actual_parent.st_dev, actual_parent.st_ino) == (parent.st_dev, parent.st_ino))
        directory_fd = os.open(target, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        stack.callback(os.close, directory_fd)
        directory_identity = os.fstat(directory_fd)
        os.fsync(parent_fd) # Make the new snapshot directory entry durable too.
        barriers = {}
        for path in paths:
            if before[path.name] is None:
                continue
            remaining()
            connection = stack.enter_context(closing(sqlite3.connect(path.as_uri() + '?mode=rw',
                                                                    uri=True, timeout=.1)))
            connection.execute('BEGIN IMMEDIATE')
            barriers[path.name] = connection
        # Once both barriers are held, conforming SQLite writers cannot commit
        # changes to either source while separate read-only connections back up.
        assert_quiescent()
        for path in paths:
            info = before[path.name]
            if info is None:
                records[path.name] = {'present': False}
                continue
            require(identity(path) == info)
            output = target / path.name
            fd = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
            os.close(fd)
            with closing(sqlite3.connect(path.as_uri() + '?mode=ro', uri=True, timeout=.1)) as source, \
                    closing(sqlite3.connect(output)) as destination:
                remaining()
                page_size = source.execute('PRAGMA page_size').fetchone()[0]
                page_count = source.execute('PRAGMA page_count').fetchone()[0]
                require(512 <= page_size <= 65536 and page_count >= 1 and page_size * page_count <= MAX_BYTES)
                source.set_progress_handler(lambda: int(time.monotonic() >= deadline), 1000)
                destination.set_progress_handler(lambda: int(time.monotonic() >= deadline), 1000)
                source.backup(destination, pages=128, progress=lambda *_: remaining(), sleep=.01)
                remaining()
                require(output.stat().st_size <= MAX_BYTES)
                require(destination.execute('PRAGMA quick_check').fetchall() == [('ok',)])
                remaining()
            fd = os.open(output, os.O_RDONLY | os.O_NOFOLLOW)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
            remaining()
            records[path.name] = {'present': True, 'original': info, 'sha256': digest_file(output, remaining)}
        assert_quiescent()
        for path in paths:
            require((identity(path) if path.exists() else None) == before[path.name])
        remaining()
        current_parent = target.parent.lstat();current_target = target.lstat()
        require((current_parent.st_dev,current_parent.st_ino)==(parent.st_dev,parent.st_ino))
        require((current_target.st_dev,current_target.st_ino)==(directory_identity.st_dev,directory_identity.st_ino))
        write_private(target / 'state.json', {'schema': 1, 'completed': True,
                                            'source_directory': str(paths[0].parent), 'files': records})
        os.fsync(directory_fd)
        os.fsync(parent_fd)
        remaining()
    return {'backupComplete': True, 'databaseCount': len(barriers),
            'trainingAbsentPreserved': before[paths[1].name] is None}


def restore(paths, baseline, failed_snapshot, assert_quiescent, *, manifest_sha256,
            data_identity=(999, 987), timeout=60):
    """Preserve failed-candidate state, then restore the exact private baseline.

    Requires writer services stopped through the whole operation. No executable,
    configuration, policy, key or journal is changed here. A partial restore is
    uncertain: the owner transaction must stop, preserving both state snapshots.
    """
    require(0 < timeout <= 60)
    deadline = time.monotonic() + timeout
    def remaining():require(time.monotonic() < deadline)
    paths = [Path(path) for path in paths]
    baseline = Path(baseline)
    folder = baseline.lstat()
    require(stat.S_ISDIR(folder.st_mode) and folder.st_uid == os.geteuid()
            and stat.S_IMODE(folder.st_mode) == 0o700)
    manifest_path = baseline / 'state.json'
    meta = identity(manifest_path)
    require(meta['uid'] == os.geteuid() and meta['mode'] == 0o600 and meta['size'] <= 65536)
    raw = manifest_path.read_bytes()
    require(hashlib.sha256(raw).hexdigest() == manifest_sha256)
    manifest = json.loads(raw)
    require(type(manifest) is dict and set(manifest) == {'schema', 'completed', 'source_directory', 'files'}
            and manifest['schema'] == 1 and manifest['completed'] is True)
    require(len(paths) == 2 and paths[0].name == 'aiua_context.db'
            and paths[1].name == 'whisper_training.db'
            and all(path.parent == paths[0].parent for path in paths)
            and str(paths[0].parent) == manifest['source_directory'])
    require(set(manifest['files']) == {path.name for path in paths})
    for path in paths:
        entry = manifest['files'][path.name]
        if entry['present']:
            original = entry['original']
            require((original['uid'], original['gid']) == data_identity
                    and original['mode'] in (0o600, 0o644))
            saved = baseline / path.name
            saved_meta = identity(saved)
            require(saved_meta['uid'] == os.geteuid() and saved_meta['mode'] == 0o600
                    and saved_meta['size'] <= MAX_BYTES)
            require(digest_file(saved, remaining) == entry['sha256'])
        else:
            require(entry == {'present': False} and path.name == 'whisper_training.db')
    remaining()
    # Capture candidate-only writes before replacing or deleting any live file.
    snapshot(paths, failed_snapshot, assert_quiescent, data_identity=data_identity,
             timeout=min(30, max(.001, deadline - time.monotonic())))
    remaining()
    assert_quiescent()
    current = {str(path): identity(path) if path.exists() else None for path in paths}
    sidecars = {}
    for path in paths:
        for suffix in ('-wal', '-shm', '-journal'):
            sidecar = Path(str(path) + suffix)
            if sidecar.exists() or sidecar.is_symlink():
                info = identity(sidecar)
                require(info['uid'] in (os.geteuid(), data_identity[0]) and not info['mode'] & 0o022)
                sidecars[sidecar] = info
    for sidecar, info in sidecars.items():
        remaining();require(identity(sidecar) == info);sidecar.unlink()
    for path in paths:
        remaining();assert_quiescent()
        require((identity(path) if path.exists() else None) == current[str(path)])
        entry = manifest['files'][path.name]
        if not entry['present']:
            if path.exists():path.unlink()
        else:
            fd, temporary = tempfile.mkstemp(prefix='.muninn-state-restore-', dir=path.parent)
            try:
                with (baseline / path.name).open('rb') as source:
                    while True:
                        remaining();chunk = source.read(65536)
                        if not chunk:break
                        offset = 0
                        while offset < len(chunk):
                            count = os.write(fd, chunk[offset:]);require(count > 0);offset += count
                os.fchmod(fd, entry['original']['mode'])
                os.fchown(fd, entry['original']['uid'], entry['original']['gid'])
                os.fsync(fd);os.close(fd);fd = None
                remaining();require(digest_file(Path(temporary), remaining) == entry['sha256'])
                os.replace(temporary, path)
            finally:
                if fd is not None:os.close(fd)
                # Do not delete partial restoration evidence on uncertainty.
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:os.fsync(directory)
        finally:os.close(directory)
    remaining()
    return {'stateRestored': True, 'failedCandidateStatePreserved': True,
            'trainingAbsenceRestored': not manifest['files']['whisper_training.db']['present']}
