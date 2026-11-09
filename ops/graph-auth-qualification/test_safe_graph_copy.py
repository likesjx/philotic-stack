import os,tempfile,unittest
from pathlib import Path
from unittest.mock import patch
import safe_graph_copy as copy

class CopyTests(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.root=Path(self.tmp.name);self.src=self.root/'state';self.dst=self.root/'restore'
  (self.src/'snapshots').mkdir(parents=True);(self.src/'snapshots'/'private').write_bytes(b'synthetic graph data');(self.src/'wal').mkdir();(self.src/'wal'/'wal-file').write_bytes(b'synthetic wal')
  (self.src/'databases'/'memgraph').mkdir(parents=True);(self.src/'databases'/'memgraph'/'snapshots').symlink_to('../../snapshots');(self.src/'databases'/'memgraph'/'rocksdb_wal').symlink_to('../../rocksdb_wal')
  (self.src/'auth').mkdir();(self.src/'auth'/'synthetic').write_bytes(b'auth');(self.src/'settings').mkdir();(self.src/'settings'/'synthetic').write_bytes(b'settings')
 def test_preserves_all_files_and_exact_links_without_dereference(self):
  self.assertEqual(copy.copy_closed_fixture(self.src,self.dst),'graph_copy:verified')
  self.assertEqual(os.readlink(self.dst/'databases/memgraph/snapshots'),'../../snapshots')
  self.assertTrue((self.dst/'databases/memgraph/rocksdb_wal').is_symlink());self.assertFalse((self.dst/'databases/memgraph/rocksdb_wal').exists())
  self.assertEqual((self.dst/'wal/wal-file').read_bytes(),b'synthetic wal')
  self.assertEqual((self.dst/'auth/synthetic').read_bytes(),b'auth');self.assertEqual((self.dst/'settings/synthetic').read_bytes(),b'settings')
 def test_escape_absolute_wrong_name_and_nested_are_rejected(self):
  for target in ('/etc','../../../etc','../../wal','../snapshots'):
   with self.subTest(target=target):
    link=self.src/'databases/memgraph/snapshots';link.unlink();link.symlink_to(target)
    with self.assertRaisesRegex(copy.CopyFailure,'unsupported_link') as error:copy.copy_closed_fixture(self.src,self.dst)
    self.assertNotIn(target,str(error.exception));self.assertFalse(self.dst.exists())
  (self.src/'databases/memgraph/snapshots').unlink();(self.src/'bad').symlink_to('snapshots')
  with self.assertRaisesRegex(copy.CopyFailure,'unsupported_link'):copy.copy_closed_fixture(self.src,self.dst)
 def test_symlink_in_snapshot_and_fifo_are_rejected(self):
  (self.src/'snapshots'/'bad').symlink_to('/etc')
  with self.assertRaisesRegex(copy.CopyFailure,'unsupported_link'):copy.copy_closed_fixture(self.src,self.dst)
  (self.src/'snapshots'/'bad').unlink();os.mkfifo(self.src/'special')
  with self.assertRaisesRegex(copy.CopyFailure,'unsupported_entry'):copy.copy_closed_fixture(self.src,self.dst)
 def test_excluded_alias_and_root_chain_are_rejected(self):
  (self.src/'databases/memgraph/auth').symlink_to('../../auth')
  with self.assertRaisesRegex(copy.CopyFailure,'unsupported_link'):copy.copy_closed_fixture(self.src,self.dst)
  (self.src/'databases/memgraph/auth').unlink();(self.src/'chain').symlink_to('snapshots');(self.src/'databases/memgraph/chain').symlink_to('../../chain')
  with self.assertRaisesRegex(copy.CopyFailure,'unsupported_link'):copy.copy_closed_fixture(self.src,self.dst)
 def test_leaf_replacement_never_follows_link(self):
  original=copy.os.open;target=self.src/'snapshots/private'; calls=[]
  def swapped(path,flags,*args,**kwargs):
   if Path(path)==target and not calls:
    calls.append(True);target.unlink();target.symlink_to('/etc/passwd')
   return original(path,flags,*args,**kwargs)
  with patch('safe_graph_copy.os.open',side_effect=swapped):
   with self.assertRaisesRegex(copy.CopyFailure,'inventory_unavailable'):copy.copy_closed_fixture(self.src,self.dst)
  self.assertFalse(self.dst.exists())

 def test_existing_target_and_missing_snapshot_fail_closed(self):
  self.dst.mkdir()
  with self.assertRaisesRegex(copy.CopyFailure,'target_exists'):copy.copy_closed_fixture(self.src,self.dst)
  self.dst.rmdir();(self.src/'snapshots/private').unlink()
  with self.assertRaisesRegex(copy.CopyFailure,'snapshot_missing'):copy.copy_closed_fixture(self.src,self.dst)
 def test_changed_source_or_copy_and_interrupted_copy_fail(self):
  real=copy.shutil.copytree
  def mutate_source(source,target,*a,**kw):
   result=real(source,target,*a,**kw)
   if Path(source)==self.src:(self.src/'wal/wal-file').write_bytes(b'changed')
   return result
  with patch('safe_graph_copy.shutil.copytree',side_effect=mutate_source):
   with self.assertRaisesRegex(copy.CopyFailure,'source_changed'):copy.copy_closed_fixture(self.src,self.dst)
  copy.shutil.rmtree(self.dst)
  def mutate_target(source,target,*a,**kw):
   result=real(source,target,*a,**kw)
   if Path(source)==self.src:(self.dst/'wal/wal-file').write_bytes(b'target-mutated')
   return result
  with patch('safe_graph_copy.shutil.copytree',side_effect=mutate_target):
   with self.assertRaisesRegex(copy.CopyFailure,'target_mismatch'):copy.copy_closed_fixture(self.src,self.dst)
  copy.shutil.rmtree(self.dst)
  with patch('safe_graph_copy.shutil.copytree',side_effect=OSError('SECRET')):
   with self.assertRaisesRegex(copy.CopyFailure,'copy_failed') as e:copy.copy_closed_fixture(self.src,self.dst)
  self.assertNotIn('SECRET',str(e.exception))
 def test_category_is_closed_and_cannot_echo_raw_text(self):
  with self.assertRaises(ValueError):copy.CopyFailure('SECRET')
  self.assertNotIn('SECRET',str(copy.CopyFailure('unsupported_link')))
 def test_count_size_and_bad_coordinates_refused(self):
  with patch('safe_graph_copy.LIMIT_BYTES',1):
   with self.assertRaisesRegex(copy.CopyFailure,'size_bound'):copy.copy_closed_fixture(self.src,self.dst)
  with patch('safe_graph_copy.LIMIT_ENTRIES',1):
   with self.assertRaisesRegex(copy.CopyFailure,'entry_bound'):copy.copy_closed_fixture(self.src,self.dst)
  with self.assertRaisesRegex(copy.CopyFailure,'invalid_coordinates'):copy.copy_closed_fixture(self.src,self.root/'elsewhere')
