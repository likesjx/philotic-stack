"""Classify fixture data-directory entries; never follow or print paths or targets."""
import os
from pathlib import Path
import stat

class InventoryFailure(Exception):
    pass

def inventory(root):
    root=Path(root)
    if root.is_symlink() or not root.is_dir(): raise InventoryFailure('invalid_root')
    counts={}; seen=0
    def fail(_): raise InventoryFailure('inventory_unavailable')
    try:
        for base,dirs,files in os.walk(root,followlinks=False,onerror=fail):
            for name in dirs+files:
                seen+=1
                if seen>4096: raise InventoryFailure('inventory_bound')
                path=Path(base)/name; rel=path.relative_to(root); mode=path.lstat().st_mode
                location='root' if len(rel.parts)==1 else rel.parts[0] if rel.parts[0] in ('snapshots','wal') else 'other'
                if stat.S_ISREG(mode): kind='regular'
                elif stat.S_ISDIR(mode): kind='directory'
                elif stat.S_ISLNK(mode):
                    target=os.readlink(path)
                    shape='absolute' if os.path.isabs(target) else 'relative_dotdot' if '..' in Path(target).parts else 'relative_no_dotdot'
                    kind='symlink_'+shape
                elif stat.S_ISSOCK(mode):kind='socket'
                elif stat.S_ISFIFO(mode):kind='fifo'
                elif stat.S_ISBLK(mode):kind='block'
                elif stat.S_ISCHR(mode):kind='character'
                else:kind='other'
                key=location+':'+kind; counts[key]=counts.get(key,0)+1
    except OSError: raise InventoryFailure('inventory_unavailable') from None
    return dict(sorted(counts.items()))
