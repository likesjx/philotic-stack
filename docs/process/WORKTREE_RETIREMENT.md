# Worktree retirement

Run `just worktree-gc` for a read-only report. It reads the remote develop tip
without changing refs, files, indexes, or logs. If the cached tip differs, refresh
it separately and repeat the report. Read failures return nonzero, never clean.

Retirement requires an operator-reviewed owner release file outside the retiring
checkout. Do not create a release from a merged PR alone: its owner must have
finished and released the checkout, including editor/task attachments. Records
are keyed by canonical absolute worktree path:

```json
{
  "/absolute/path/to/finished-worktree": {
    "released": true,
    "owner": "completed task or owner identity",
    "evidence": "owner release and reviewed integration evidence",
    "head": "full reviewed worktree HEAD",
    "integration": "full current origin/develop SHA",
    "expires_at": "2026-10-09T18:00:00+00:00"
  }
}
```

Choose a short expiry after review. A changed HEAD, integration tip, release file,
or expired record invalidates the plan. `just worktree-gc-apply /path/to/releases.json`
performs the same checks again before each removal. It never uses force, deletes
branches, or prunes metadata. The main checkout, detached heads and operator pins
remain preserved. A failing process inspection also preserves the checkout.

All staged, modified, untracked **and ignored** contents block retirement. That
includes `target`, `target 2`, shared-target symlinks, generated `ui-dist` and
dependency caches. Artifacts need a separate reviewed cleanup; this command does
not infer disposability from their names or delete their external targets.

Merge proof uses current develop ancestry. Squash/rebase fallback requires the
exact current branch HEAD to match a merged PR into develop, and that PR's actual
merge result to be an ancestor of current develop. Added post-PR commits, main-only
merges, missing merge objects and unknown API results preserve the checkout. The
300-PR lookup limit can conservatively preserve older merged branches.

Owner release remains a human coordination contract, not a cryptographic task
lock. The final checks narrow concurrent-change races; ordinary Git removal adds
another dirty-file guard. Owners must not resume a released checkout during apply.

The launchd installer now schedules reports only. The previously installed
`gui/501/com.philotic.worktree-gc` job was disabled and unloaded on October 9;
source changes do not re-enable it. Scheduling changes require separate approval.

Verification: `python3 -B -m unittest discover -s scripts/tests -p test_worktree_gc.py -v`.
All removal tests use disposable synthetic repositories; never test apply against
operator worktrees.
