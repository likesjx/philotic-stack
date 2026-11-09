"""Closed synthetic graph copy: preserve exact known compatibility links, never follow them.

Only for a trusted, exclusively owned fixture after verified writer shutdown. This
is not a production custody, power-loss, or general-purpose backup primitive.
"""
import hashlib
import os
from pathlib import Path
import shutil
import stat

LIMIT_ENTRIES=4096
LIMIT_BYTES=2*1024*1024*1024
EXCLUDED_ALIASES=frozenset(('audit_log','auth','databases','internal_modules','settings'))

class CopyFailure(Exception):
    ALLOWED=frozenset(('invalid_root','nofollow_unavailable','entry_bound','size_bound','file_changed','unsupported_link','unsupported_entry','inventory_unavailable','invalid_coordinates','target_exists','snapshot_missing','copy_failed','source_changed','target_mismatch'))
    def __init__(self,category):
        if category not in self.ALLOWED:raise ValueError('invalid copy category')
        self.category=category
        super().__init__(category)

def inventory(root):
    root=Path(root)
    if root.is_symlink() or not root.is_dir():raise CopyFailure('invalid_root')
    if not hasattr(os,'O_NOFOLLOW'):raise CopyFailure('nofollow_unavailable')
    result={}; total=0
    def failed(_):raise CopyFailure('inventory_unavailable')
    try:
        for base,dirs,files in os.walk(root,followlinks=False,onerror=failed):
            for name in dirs+files:
                if len(result)>=LIMIT_ENTRIES:raise CopyFailure('entry_bound')
                path=Path(base)/name;rel=path.relative_to(root);info=path.lstat();mode=stat.S_IMODE(info.st_mode)
                if stat.S_ISDIR(info.st_mode):value=('dir',mode)
                elif stat.S_ISREG(info.st_mode):
                    total+=info.st_size
                    if total>LIMIT_BYTES:raise CopyFailure('size_bound')
                    fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|getattr(os,'O_NONBLOCK',0))
                    try:
                        current=os.fstat(fd)
                        if (not stat.S_ISREG(current.st_mode) or (info.st_dev,info.st_ino,info.st_size,info.st_mode)!=(current.st_dev,current.st_ino,current.st_size,current.st_mode)):
                            raise CopyFailure('file_changed')
                        digest=hashlib.sha256()
                        while chunk:=os.read(fd,1024*1024):digest.update(chunk)
                        after=os.fstat(fd)
                        if (current.st_size,current.st_mtime_ns,current.st_ctime_ns)!=(after.st_size,after.st_mtime_ns,after.st_ctime_ns):raise CopyFailure('file_changed')
                    finally:os.close(fd)
                    value=('file',mode,info.st_size,digest.hexdigest())
                elif stat.S_ISLNK(info.st_mode):
                    if rel.parent!=Path('databases/memgraph') or name in ('.','..') or name.startswith('.') or name in EXCLUDED_ALIASES:
                        raise CopyFailure('unsupported_link')
                    target=os.readlink(path)
                    if target!='../../'+name:raise CopyFailure('unsupported_link')
                    value=('link',target)
                else:raise CopyFailure('unsupported_entry')
                result[rel.as_posix()]=value
    except OSError:raise CopyFailure('inventory_unavailable') from None
    return result

def copy_closed_fixture(source,target):
    source,target=Path(source),Path(target)
    if source.name!='state' or target.name!='restore' or source.parent!=target.parent:
        raise CopyFailure('invalid_coordinates')
    if target.exists() or target.is_symlink():raise CopyFailure('target_exists')
    before=inventory(source)
    if not any(k.startswith('snapshots/') and v[0]=='file' for k,v in before.items()):raise CopyFailure('snapshot_missing')
    try:shutil.copytree(source,target,symlinks=True)
    except (OSError,shutil.Error):raise CopyFailure('copy_failed') from None
    if inventory(source)!=before:raise CopyFailure('source_changed')
    if inventory(target)!=before:raise CopyFailure('target_mismatch')
    return 'graph_copy:verified'
