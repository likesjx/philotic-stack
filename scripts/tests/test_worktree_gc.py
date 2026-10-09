"""All destructive integration cases use newly-created disposable repositories."""
import contextlib
import datetime as dt
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('gc', Path(__file__).parents[1] / 'worktree_gc.py')
gc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gc)


class RetirementTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / 'repo'
        self.remote = self.root / 'remote.git'
        self.wt = self.root / 'worktree'
        self.run_git(self.root, 'init', '--bare', str(self.remote))
        self.run_git(self.root, 'init', '-b', 'develop', str(self.repo))
        self.run_git(self.repo, 'config', 'user.email', 'synthetic@example.invalid')
        self.run_git(self.repo, 'config', 'user.name', 'Synthetic')
        (self.repo / 'file').write_text('initial\n')
        (self.repo / '.gitignore').write_text('ignored\n')
        self.run_git(self.repo, 'add', '.')
        self.run_git(self.repo, 'commit', '-m', 'initial')
        self.run_git(self.repo, 'remote', 'add', 'origin', str(self.remote))
        self.run_git(self.repo, 'push', '-u', 'origin', 'develop')
        self.run_git(self.repo, 'worktree', 'add', '-b', 'finished', str(self.wt))
        self.tip = gc.remote_tip(self.repo)
        self.release = self.root / 'release.json'
        self.record = dict(released=True, owner='synthetic-owner', evidence='owner done, reviewed',
                           head=self.tip, integration=self.tip,
                           expires_at=(dt.datetime.now(dt.timezone.utc) + dt.timedelta(hours=1)).isoformat())
        self.write_release()

    def run_git(self, repo, *args):
        subprocess.run(['git', '-C', str(repo), *args], check=True, capture_output=True)

    def write_release(self):
        self.release.write_text(json.dumps({str(self.wt.resolve()): self.record}))

    def run_gc(self, apply=False, release=True):
        args = ['--repo', str(self.repo), '--apply' if apply else '--dry-run']
        if release:
            args += ['--release-file', str(self.release)]
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return gc.main(args)

    def test_dry_run_changes_no_git_files_or_worktree(self):
        def state():
            return {str(p): (p.read_bytes(), p.stat().st_mtime_ns) for p in
                    (self.repo / '.git').rglob('*') if p.is_file()}
        before = state()
        with patch.object(gc, 'in_use', return_value=False):
            self.assertEqual(self.run_gc(), 0)
        self.assertEqual(state(), before)
        self.assertTrue(self.wt.exists())

    def test_apply_removes_only_disposable_released_clean_merged_tree(self):
        with patch.object(gc, 'in_use', return_value=False):
            self.assertEqual(self.run_gc(True), 0)
        self.assertFalse(self.wt.exists())
        self.run_git(self.repo, 'show-ref', '--verify', 'refs/heads/finished')

    def test_apply_without_release_refuses(self):
        self.assertEqual(self.run_gc(True, False), 1)
        self.assertTrue(self.wt.exists())

    def test_content_preserved(self):
        for kind in ['modified', 'staged', 'untracked', 'ignored', 'target_source']:
            with self.subTest(kind=kind):
                if kind in ['modified', 'staged']:
                    (self.wt / 'file').write_text('changed\n')
                    if kind == 'staged':
                        self.run_git(self.wt, 'add', 'file')
                elif kind == 'target_source':
                    (self.wt / 'target').mkdir()
                    (self.wt / 'target' / 'source.rs').write_text('source')
                else:
                    (self.wt / ('ignored' if kind == 'ignored' else 'new')).write_text('retain')
                with patch.object(gc, 'in_use', return_value=False):
                    self.assertEqual(self.run_gc(True), 0)
                self.assertTrue(self.wt.exists())

    def test_release_binding_and_expiry(self):
        for key, value in [('released', False), ('head', '0'*40), ('integration', '0'*40),
                           ('owner', ''), ('evidence', ''), ('expires_at', '2000-01-01T00:00:00+00:00')]:
            with self.subTest(key=key):
                r = dict(self.record); r[key] = value
                self.assertFalse(gc.released(r, self.tip, self.tip))

    def test_live_owner_preserved(self):
        with patch.object(gc, 'in_use', return_value=True):
            self.assertEqual(self.run_gc(True), 0)
        self.assertTrue(self.wt.exists())

    def test_process_error_preserved(self):
        with patch.object(gc, 'in_use', side_effect=gc.Unsafe('lsof failed')):
            self.assertEqual(self.run_gc(True), 1)
        self.assertTrue(self.wt.exists())

    def test_status_error_preserved(self):
        with patch.object(gc, 'snapshot', side_effect=gc.Unsafe('status failed')):
            self.assertEqual(self.run_gc(True), 1)
        self.assertTrue(self.wt.exists())

    def test_remote_error_refuses(self):
        with patch.object(gc, 'remote_tip', side_effect=gc.Unsafe('lookup failed')):
            self.assertEqual(self.run_gc(True), 1)
        self.assertTrue(self.wt.exists())

    def test_malformed_registration_preserved(self):
        pointer = self.wt / '.git'
        pointer.unlink(); pointer.mkdir()
        self.assertEqual(self.run_gc(True), 1)
        self.assertTrue(self.wt.exists())

    def test_state_change_before_remove_preserved(self):
        original = gc.snapshot(self.repo, self.wt)
        changed = (original[0], original[1], '?? late-source', original[3])
        with patch.object(gc, 'snapshot', side_effect=[original, changed]), patch.object(gc, 'in_use', return_value=False):
            self.assertEqual(self.run_gc(True), 1)
        self.assertTrue(self.wt.exists())

    def test_release_change_before_remove_preserved(self):
        def process(path):
            self.release.write_text('{}')
            return False
        with patch.object(gc, 'in_use', side_effect=process):
            self.assertEqual(self.run_gc(True), 1)
        self.assertTrue(self.wt.exists())

    def test_shared_artifact_symlink_preserved(self):
        external = self.root / 'shared-cache'
        external.mkdir(); (external / 'retain').write_text('cache')
        (self.wt / 'target').symlink_to(external, target_is_directory=True)
        self.assertEqual(self.run_gc(True), 0)
        self.assertTrue(self.wt.exists())
        self.assertEqual((external / 'retain').read_text(), 'cache')

    def test_detached_and_pinned_preserved(self):
        self.run_git(self.wt, 'checkout', '--detach')
        self.assertEqual(self.run_gc(True), 0)
        self.assertTrue(self.wt.exists())
        self.run_git(self.wt, 'checkout', 'finished')
        with patch.dict('os.environ', {'PHILOTIC_WTGC_KEEP': 'finished'}):
            self.assertEqual(self.run_gc(True), 0)
        self.assertTrue(self.wt.exists())

    def test_real_squash_and_rebase_proof(self):
        (self.wt / 'file').write_text('feature\n')
        self.run_git(self.wt, 'add', 'file')
        self.run_git(self.wt, 'commit', '-m', 'feature')
        head = gc.git(self.wt, 'rev-parse', 'HEAD').stdout.strip()
        self.run_git(self.repo, 'merge', '--squash', 'finished')
        self.run_git(self.repo, 'commit', '-m', 'squashed')
        merged = gc.git(self.repo, 'rev-parse', 'HEAD').stdout.strip()
        rows = [dict(headRefName='finished', headRefOid=head, baseRefName='develop',
                     mergeCommit=dict(oid=merged))]
        self.assertFalse(gc.ancestor(self.repo, head, merged))
        self.assertTrue(gc.included(self.repo, 'refs/heads/finished', head, merged, rows))
        self.assertFalse(gc.included(self.repo, 'refs/heads/finished', head, self.tip, rows))
        (self.wt / 'file').write_text('later\n')
        self.run_git(self.wt, 'add', 'file'); self.run_git(self.wt, 'commit', '-m', 'later')
        later = gc.git(self.wt, 'rev-parse', 'HEAD').stdout.strip()
        self.assertFalse(gc.included(self.repo, 'refs/heads/finished', later, merged, rows))

    def test_lsof_error_and_subdirectory_cwd(self):
        with patch.object(gc, 'command', return_value=subprocess.CompletedProcess([], 0,
                stdout='n' + str(self.wt.resolve() / 'subdir') + '\n', stderr='')):
            self.assertTrue(gc.in_use(self.wt))
        with patch.object(gc, 'command', return_value=subprocess.CompletedProcess([], 0,
                stdout='', stderr='cannot inspect')):
            with self.assertRaises(gc.Unsafe):
                gc.in_use(self.wt)

    def test_fetch_error_refuses(self):
        original_git = gc.git
        def intercept(repo, *args, **kwargs):
            if args[:2] == ('rev-parse', 'refs/remotes/origin/develop'):
                return subprocess.CompletedProcess([], 0, stdout='0'*40+'\n')
            if args and args[0] == 'fetch':
                raise gc.Unsafe('fetch failed')
            return original_git(repo, *args, **kwargs)
        with patch.object(gc, 'git', side_effect=intercept):
            self.assertEqual(self.run_gc(True), 1)
        self.assertTrue(self.wt.exists())

    def test_dry_run_stale_ref_never_fetches(self):
        real = gc.git
        called = []
        def intercept(repo, *args, **kwargs):
            called.append(args)
            if args[:2] == ('rev-parse', 'refs/remotes/origin/develop'):
                return subprocess.CompletedProcess([], 0, stdout='0'*40+'\n')
            return real(repo, *args, **kwargs)
        with patch.object(gc, 'git', side_effect=intercept):
            self.assertEqual(self.run_gc(), 1)
        self.assertFalse(any(x[0] == 'fetch' for x in called))

    def test_post_merge_commit_and_main_only_pr_preserved(self):
        records = [dict(headRefName='finished', headRefOid='reviewed', baseRefName='main',
                        mergeCommit=dict(oid='merge'))]
        with patch.object(gc, 'ancestor', return_value=False):
            self.assertFalse(gc.included(self.repo, 'refs/heads/finished', 'reviewed', self.tip, records))
        records[0]['baseRefName'] = 'develop'
        with patch.object(gc, 'ancestor', side_effect=[False, True]):
            self.assertTrue(gc.included(self.repo, 'refs/heads/finished', 'reviewed', self.tip, records))
        with patch.object(gc, 'ancestor', return_value=False):
            self.assertFalse(gc.included(self.repo, 'refs/heads/finished', 'later', self.tip, records))
            self.assertFalse(gc.included(self.repo, 'refs/heads/finished', 'reviewed', self.tip, records))


if __name__ == '__main__':
    unittest.main()
