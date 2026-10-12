"""Cold-state rollback fixture only. Production paths and running fixtures deny."""
import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile

FILES = ('hotel.db', 'training.db', 'agent-graphs/synthetic-agent.db', 'blobs/synthetic.txt')
MAX_FILE = 4 * 1024**2
SEAL = {'schema': 1, 'kind': 'muninn-synthetic-rollback', 'active_processes': 0}


def require(value, message):
    if not value:
        raise ValueError(message)


def data(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as f:
        info = os.fstat(f.fileno())
        require(stat.S_ISREG(info.st_mode) and info.st_size <= MAX_FILE, 'fixture file bound/type')
        value = f.read(MAX_FILE + 1)
        require(len(value) == info.st_size, 'fixture changed during read')
        return value


def guard(root):
    root = Path(root).absolute()
    require(root == root.resolve() and root.is_relative_to(Path(tempfile.gettempdir()).resolve())
            and root.name.startswith('muninn-synthetic-'), 'only sealed temporary synthetic fixture roots')
    require(json.loads(data(root / 'quiescent.json')) == SEAL, 'fixture is not quiescent')
    for name in FILES:
        path = root / name
        require(path.parent == path.parent.resolve(), 'fixture parent alias')
        require(not Path(str(path) + '-wal').exists() and not Path(str(path) + '-shm').exists(),
                'SQLite sidecar present: close all fixture connections before snapshot/restore')
    return root


def snapshot(root):
    root = guard(root)
    values = {name: data(root / name) for name in FILES}
    backup = root / 'rollback'
    backup.mkdir(mode=0o700)
    for name, value in values.items():
        path = backup / name
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        path.write_bytes(value)
        path.chmod(0o600)
    manifest = {'schema': 1, 'kind': SEAL['kind'],
                'sha256': {name: hashlib.sha256(value).hexdigest() for name, value in values.items()}}
    (backup / 'manifest.json').write_text(json.dumps(manifest, sort_keys=True))
    (backup / 'manifest.json').chmod(0o600)
    return manifest


def restore(root):
    root = guard(root)
    backup = root / 'rollback'
    require(backup == backup.resolve(), 'snapshot directory alias')
    manifest = json.loads(data(backup / 'manifest.json'))
    require(set(manifest) == {'schema', 'kind', 'sha256'} and manifest['schema'] == 1
            and manifest['kind'] == SEAL['kind'] and set(manifest['sha256']) == set(FILES), 'snapshot schema')
    values = {}
    for name in FILES:
        require((backup / name).parent == (backup / name).parent.resolve(), 'snapshot parent alias')
        value = data(backup / name)
        require(hashlib.sha256(value).hexdigest() == manifest['sha256'][name], 'snapshot digest')
        values[name] = value
    # Validate every snapshot before the first write. On interruption callers
    # must keep the runtime stopped and rerun restoration; never start from a
    # partially restored tree. The immutable snapshot remains available.
    require(all(stat.S_ISREG((root / name).lstat().st_mode) for name in FILES),
            'restore targets must remain regular')
    for name, value in values.items():
        path = root / name
        require(stat.S_ISREG(path.lstat().st_mode), 'restore target must remain regular')
        # Unique exclusive files make leftovers from a killed process harmless
        # to retries. Never follow, reuse, or delete another attempt's residue.
        fd, tmp_name = tempfile.mkstemp(prefix=path.name + '.restore.', dir=path.parent)
        tmp = Path(tmp_name)
        try:
            with os.fdopen(fd, 'wb') as f:
                f.write(value)
                f.flush()
                os.fsync(f.fileno())
            os.replace(tmp, path)
            directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            tmp.unlink(missing_ok=True)
    require(all(hashlib.sha256(data(root / n)).hexdigest() == manifest['sha256'][n] for n in FILES),
            'restored state mismatch')
    return {'schema': 1, 'restored_files': len(FILES), 'synthetic_only': True}
