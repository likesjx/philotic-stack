"""Dormant synthetic-only checkpoint codec; not an authorization boundary.

Caller must hold exclusive custody of the synthetic directory throughout and
bind the Mongo identity to independently verified fixture ownership. No output
or exception includes the token. This module does not launch or resume a test.
"""
import json
import os
import re
import stat

NAME = 'synthetic-restart-checkpoint.json'
ID = re.compile(r'[0-9a-f]{64}\Z')
NONCE = re.compile(r'[0-9a-f]{32}\Z')
TOKEN = re.compile(r'[A-Za-z0-9._~-]{1,4096}\Z')


def _dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    st = os.fstat(fd)
    if st.st_uid != os.geteuid() or stat.S_IMODE(st.st_mode) != 0o700:
        os.close(fd)
        raise ValueError('synthetic checkpoint directory unsafe')
    return fd


def _validate(value, nonce, mongo):
    if (not isinstance(nonce, str) or not NONCE.fullmatch(nonce)
            or not isinstance(mongo, str) or not ID.fullmatch(mongo)
            or type(value) is not dict or set(value) != {'schema','nonce','mongo','revision','token'}
            or type(value['schema']) is not int or value['schema'] != 1
            or value['nonce'] != nonce or value['mongo'] != mongo
            or type(value['revision']) is not int or not 0 <= value['revision'] <= 2**53-1
            or not isinstance(value['token'], str) or not TOKEN.fullmatch(value['token'])):
        raise ValueError('synthetic checkpoint invalid')
    return value


def create(path, nonce, mongo, revision, token):
    value = _validate({'schema':1,'nonce':nonce,'mongo':mongo,'revision':revision,'token':token}, nonce, mongo)
    data = json.dumps(value, separators=(',', ':'), ensure_ascii=True).encode()
    dfd = _dir(path)
    try:
        fd = os.open(NAME, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600, dir_fd=dfd)
        try:
            with os.fdopen(fd, 'wb', closefd=False) as out:
                out.write(data)
                out.flush()
                os.fsync(out.fileno())
        finally:
            os.close(fd)
        os.fsync(dfd)
    finally:
        os.close(dfd)


def load(path, nonce, mongo):
    dfd = _dir(path)
    try:
        fd = os.open(NAME, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=dfd)
        try:
            st = os.fstat(fd)
            if (not stat.S_ISREG(st.st_mode) or st.st_uid != os.geteuid()
                    or stat.S_IMODE(st.st_mode) != 0o600 or st.st_nlink != 1 or st.st_size > 8192):
                raise ValueError('synthetic checkpoint file unsafe')
            with os.fdopen(fd, 'rb', closefd=False) as source:
                data = source.read(8193)
            if len(data) != st.st_size:
                raise ValueError('synthetic checkpoint read uncertain')
        finally:
            os.close(fd)
    finally:
        os.close(dfd)
    try:
        def pairs(items):
            result = {}
            for key, val in items:
                if key in result: raise ValueError('duplicate key')
                result[key] = val
            return result
        value = json.loads(data, object_pairs_hook=pairs)
        return _validate(value, nonce, mongo)
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError, TypeError):
        raise ValueError('synthetic checkpoint invalid') from None
