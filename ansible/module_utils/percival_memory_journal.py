"""Owned public transaction records; service sequencing remains in Ansible.

A pending checkpoint never authorizes automatic retry. Recovery needs a new,
explicitly approved quiescence attestation and the original immutable inputs.
"""
import ctypes
import hashlib
import json
import os
from pathlib import Path
import stat
import sys
import uuid
import re
from ansible.module_utils.percival_memory_contract import canonical, digest, exact, require, validate, policy

STEPS = ('stop_gateway', 'write_policy', 'write_override', 'reload', 'start_socket', 'restart_hotel', 'start_gateway')
RECOVERY_STEPS = ('stop_gateway', 'restore_policy', 'remove_override', 'reload', 'start_socket', 'restart_hotel', 'start_discovery')


def bytes_sha(value):
    return hashlib.sha256(value).hexdigest()


def read_regular(path, limit=1048576, private=False):
    """Root-owned, no symlink traversal, bounded public source/journal only."""
    path = Path(path)
    require(path.is_absolute() and '..' not in path.parts)
    fd = os.open('/', os.O_RDONLY | os.O_DIRECTORY)
    try:
        for i, part in enumerate(path.parts[1:]):
            last = i == len(path.parts) - 2
            child = os.open(part, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | (0 if last else os.O_DIRECTORY), dir_fd=fd)
            os.close(fd); fd = child
            info = os.fstat(fd)
            require(info.st_uid == 0 and not info.st_mode & 0o022)
            require(stat.S_ISREG(info.st_mode) if last else stat.S_ISDIR(info.st_mode))
        before = os.fstat(fd)
        require(before.st_nlink == 1 and before.st_size <= limit)
        require(not private or stat.S_IMODE(before.st_mode) == 0o600)
        chunks=[]; size=0
        while size <= limit:
            b=os.read(fd, min(65536, limit + 1 - size))
            if not b: break
            chunks.append(b); size+=len(b)
        after=os.fstat(fd)
        require(size <= limit and (before.st_ino,before.st_size,before.st_mtime_ns,before.st_ctime_ns)==(after.st_ino,after.st_size,after.st_mtime_ns,after.st_ctime_ns))
        return b''.join(chunks)
    finally:
        os.close(fd)


def publish_noreplace(source, destination):
    """Atomic same-filesystem publication, with no transient extra hard link."""
    library=ctypes.CDLL(None,use_errno=True)
    if sys.platform=='linux':
        operation=library.renameat2
        operation.argtypes=[ctypes.c_int,ctypes.c_char_p,ctypes.c_int,ctypes.c_char_p,ctypes.c_uint]
        result=operation(-100,os.fsencode(source),-100,os.fsencode(destination),1)  # RENAME_NOREPLACE
    elif sys.platform=='darwin':
        operation=library.renamex_np
        operation.argtypes=[ctypes.c_char_p,ctypes.c_char_p,ctypes.c_uint]
        result=operation(os.fsencode(source),os.fsencode(destination),4)  # RENAME_EXCL; local tests only
    else:
        raise ValueError('atomic no-replace publication unavailable')
    if result!=0:
        error=ctypes.get_errno()
        raise OSError(error,os.strerror(error))


def atomic_publish(path, data, mode=0o600):
    """Complete/fsync bytes before the final name exists. Never replace it."""
    path=Path(path)
    temporary=path.parent/('.muninn-publish-'+uuid.uuid4().hex+'.tmp')
    fd=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
    try:
        os.fchmod(fd,mode)
        require(os.write(fd,data)==len(data))
        os.fsync(fd)
        os.close(fd);fd=None
        publish_noreplace(temporary,path)
        directory=os.open(str(path.parent),os.O_RDONLY|os.O_DIRECTORY)
        try:os.fsync(directory)
        finally:os.close(directory)
    finally:
        if fd is not None:os.close(fd)
        if os.path.lexists(temporary):os.unlink(temporary)


def is_publication_temporary(path):
    """Recognize only incomplete private journal publications in owned directory."""
    path=Path(path)
    if re.fullmatch(r'\.muninn-publish-[a-f0-9]{32}\.tmp',path.name) is None:return False
    info=path.lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_uid==0 and info.st_nlink==1 and stat.S_IMODE(info.st_mode)==0o600 and info.st_size<=1048576)
    return True


def write_new(path, value):
    atomic_publish(path,canonical(value)+b'\n')


def initial(manifest, qualification, approved_sha, baseline_policy, now=None):
    validate(manifest,qualification,approved_sha,now)
    policy(json.loads(baseline_policy))
    require(bytes_sha(baseline_policy)==manifest['baseline']['policy_sha256'])
    return {'schema':1,'activation':manifest,'qualification':qualification,'activation_sha256':approved_sha,
            'baseline_policy':baseline_policy.decode(),'history':[],'recovery':None}


def inspect(record):
    exact(record,'schema activation qualification activation_sha256 baseline_policy history recovery')
    require(record['schema']==1 and type(record['history']) is list and len(record['history'])<=160)
    validate(record['activation'],record['qualification'],record['activation_sha256'],record['activation']['approved_at'])
    require(type(record['baseline_policy']) is str)
    policy(json.loads(record['baseline_policy']))
    require(bytes_sha(record['baseline_policy'].encode())==record['activation']['baseline']['policy_sha256'])
    recovery=record['recovery']
    if recovery is not None:
        exact(recovery,'manifest sha256')
        validate(recovery['manifest'],record['qualification'],recovery['sha256'],recovery['manifest']['approved_at'],recovery=True)
        require(recovery['manifest']['recovery']['activation_sha256']==record['activation_sha256'])
        for k in ('transaction','config_sha256','hotel_sha256','gateway_source','qualification_sha256','baseline','policy','issuer_attestation','native_key_attestation','operator_sha256'):
            require(recovery['manifest'][k]==record['activation'][k])
    previous=None; index={'activate':0,'recover':0}; pending=None
    for entry in record['history']:
        exact(entry,'mode step status')
        mode=entry['mode']
        if mode=='authorization':
            require(recovery is not None and entry=={'mode':'authorization','step':'begin_recovery','status':'authorized'})
            previous='recover';pending=None;index['recover']=0;continue
        require(mode in ('activate','recover'))
        require(not (previous=='recover' and mode=='activate'))
        steps=STEPS if mode=='activate' else RECOVERY_STEPS
        if mode=='recover' and previous!='recover':
            require(recovery is not None);pending=None
        require(index[mode]<len(steps) and entry['step']==steps[index[mode]])
        if entry['status']=='pending':
            require(pending is None);pending=(mode,entry['step'])
        else:
            require(entry['status']=='done' and pending==(mode,entry['step']))
            pending=None;index[mode]+=1
        previous=mode
    return {'mode':previous,'pending':pending,'next_activate':index['activate'],'next_recover':index['recover']}


def advance(record, mode, step, status):
    # Caller persists each new record in a NEW numbered file, never overwrites it.
    candidate=json.loads(json.dumps(record))
    candidate['history'].append({'mode':mode,'step':step,'status':status})
    inspect(candidate)
    return candidate


def begin_recovery(record, manifest, approved_sha, now=None):
    state=inspect(record)
    require(manifest['recovery']['previous_record_sha256']==digest(record))
    require(manifest['recovery']['previous_approval_sha256']==(record['recovery']['sha256'] if record['recovery'] else None))
    validate(manifest,record['qualification'],approved_sha,now,recovery=True)
    candidate=json.loads(json.dumps(record));candidate['recovery']={'manifest':manifest,'sha256':approved_sha}
    candidate['history'].append({'mode':'authorization','step':'begin_recovery','status':'authorized'})
    inspect(candidate)
    return candidate
