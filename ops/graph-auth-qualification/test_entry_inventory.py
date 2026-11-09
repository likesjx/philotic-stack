import os,stat,tempfile,unittest
from pathlib import Path
from unittest.mock import patch
from entry_inventory import inventory,InventoryFailure

class InventoryTests(unittest.TestCase):
 def test_classifies_without_paths_or_targets(self):
  with tempfile.TemporaryDirectory() as d:
   root=Path(d);(root/'snapshots').mkdir();(root/'snapshots'/'private').write_bytes(b'private');(root/'private-link').symlink_to('../PRIVATE-TARGET');os.mkfifo(root/'private-fifo')
   result=inventory(root); self.assertEqual(result['root:symlink_relative_dotdot'],1);self.assertEqual(result['root:fifo'],1);self.assertEqual(result['snapshots:regular'],1)
   self.assertNotIn('private',str(result).lower());self.assertNotIn('TARGET',str(result))
 def test_absolute_and_relative_links_are_not_followed(self):
  with tempfile.TemporaryDirectory() as d:
   root=Path(d);(root/'escape').symlink_to('/etc');(root/'inside').symlink_to('no-such-target')
   result=inventory(root);self.assertEqual(result,{'root:symlink_absolute':1,'root:symlink_relative_no_dotdot':1})
 def test_socket_and_location_without_socket_creation(self):
  with tempfile.TemporaryDirectory() as d:
   root=Path(d);(root/'wal').mkdir();(root/'wal'/'placeholder').write_bytes(b'')
   real_lstat=Path.lstat
   def fake_lstat(path,*args,**kwargs):
    if path.name=='placeholder': return type('S',(),{'st_mode':stat.S_IFSOCK})()
    return real_lstat(path,*args,**kwargs)
   with patch.object(Path,'lstat',fake_lstat):self.assertEqual(inventory(root)['wal:socket'],1)
 def test_devices_and_unknown_modes_are_fixed_categories(self):
  with tempfile.TemporaryDirectory() as d:
   root=Path(d)
   for name in ('block','character','other'):(root/name).write_bytes(b'')
   real_lstat=Path.lstat
   def fake_lstat(path,*args,**kwargs):
    modes={'block':stat.S_IFBLK,'character':stat.S_IFCHR,'other':0}
    if path.name in modes:return type('S',(),{'st_mode':modes[path.name]})()
    return real_lstat(path,*args,**kwargs)
   with patch.object(Path,'lstat',fake_lstat):self.assertEqual(inventory(root),{'root:block':1,'root:character':1,'root:other':1})
 def test_inventory_count_is_bounded(self):
  with tempfile.TemporaryDirectory() as d:
   root=Path(d);(root/'x').write_bytes(b'')
   with patch('entry_inventory.os.walk',return_value=iter([(str(root),[],['x']*4097)])):
    with self.assertRaisesRegex(InventoryFailure,'inventory_bound'):inventory(root)
 def test_walk_errors_fail_closed_without_raw_text(self):
  with tempfile.TemporaryDirectory() as d, patch('entry_inventory.os.walk',side_effect=OSError('SECRET')):
   with self.assertRaises(InventoryFailure) as e:inventory(d)
   self.assertNotIn('SECRET',str(e.exception))
 def test_root_symlink_refused(self):
  with tempfile.TemporaryDirectory() as d:
   link=Path(d)/'link';link.symlink_to('/tmp')
   with self.assertRaises(InventoryFailure):inventory(link)
