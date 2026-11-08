"""Cgroup-v2 observation for the synthetic Docker runtime, Linux only.

This requires a trusted exclusive disposable host and the exact Docker ID. It
is an observation, not a proof about external writers or production custody.
"""
import os
import re
import stat

_ID = re.compile(r'[0-9a-f]{64}\Z')
_COMPONENT = re.compile(r'[A-Za-z0-9_.:@-]+\Z')


def capture(runtime_id, pid, proc_root='/proc', cgroup_root='/sys/fs/cgroup'):
    if not _ID.fullmatch(runtime_id) or type(pid) is not int or pid <= 1:
        raise ValueError('invalid synthetic cgroup identity')
    with open(f'{proc_root}/{pid}/cgroup', encoding='ascii') as source:
        lines = source.read(4097).splitlines()
    if len(lines) != 1 or not lines[0].startswith('0::/'):
        raise RuntimeError('synthetic cgroup v2 membership unavailable')
    components = lines[0][4:].split('/')
    if (not components or any(not _COMPONENT.fullmatch(c) or c in ('.','..') for c in components)
            or components[-1] not in (runtime_id, 'docker-'+runtime_id+'.scope')):
        raise RuntimeError('synthetic cgroup identity unavailable')
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
    fd = os.open(cgroup_root, flags)
    try:
        for part in components:
            next_fd = os.open(part, flags, dir_fd=fd)
            os.close(fd)
            fd = next_fd
        meta = os.fstat(fd)
        if not stat.S_ISDIR(meta.st_mode): raise RuntimeError('synthetic cgroup unavailable')
        instance = Cgroup(fd, meta.st_dev, meta.st_ino)
        fd = -1
        try:
            if instance.populated() != 1:
                raise RuntimeError('synthetic runtime cgroup not populated')
            return instance
        except BaseException:
            instance.close()
            raise
    finally:
        if fd != -1: os.close(fd)


class Cgroup:
    def __init__(self, fd, dev, ino):
        self.fd, self.dev, self.ino = fd, dev, ino

    def populated(self):
        try:
            m = os.fstat(self.fd)
            if (m.st_dev,m.st_ino) != (self.dev,self.ino): raise ValueError()
            event = os.open('cgroup.events', os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=self.fd)
            try:
                data = os.read(event, 4097).decode('ascii')
            finally:
                os.close(event)
            if len(data) > 4096: raise ValueError()
            matches = re.findall(r'^populated ([01])$', data, re.MULTILINE)
            if len(matches) != 1: raise ValueError()
            return int(matches[0])
        except (OSError, UnicodeError, ValueError) as exc:
            raise RuntimeError('synthetic cgroup state uncertain') from exc

    def close(self):
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None
