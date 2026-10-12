#!/usr/bin/env python3
"""Conservative retirement: no force, branch deletion, pruning, or artifact exemption."""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import subprocess
import sys


class Unsafe(RuntimeError):
    pass


def command(args, accepted=(0,)):
    try:
        r = subprocess.run(args, capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise Unsafe(f"command unavailable: {args[0]}") from exc
    if r.returncode not in accepted:
        raise Unsafe(f"command failed ({r.returncode}): {' '.join(args[:4])}")
    return r


def git(repo, *args, accepted=(0,)):
    return command(['git', '--no-optional-locks', '-C', str(repo), *args], accepted)


def worktrees(repo):
    rows = []
    for block in git(repo, 'worktree', 'list', '--porcelain').stdout.strip().split('\n\n'):
        fields = dict(line.split(' ', 1) if ' ' in line else (line, '')
                      for line in block.splitlines())
        if 'worktree' not in fields or 'HEAD' not in fields:
            raise Unsafe('malformed worktree registration')
        rows.append(fields)
    return rows


def remote_tip(repo):
    rows = git(repo, 'ls-remote', '--exit-code', 'origin', 'refs/heads/develop').stdout.splitlines()
    if len(rows) != 1 or len(rows[0].split()) != 2:
        raise Unsafe('ambiguous remote integration tip')
    return rows[0].split()[0]


def ancestor(repo, head, tip):
    return git(repo, 'merge-base', '--is-ancestor', head, tip, accepted=(0, 1)).returncode == 0


def snapshot(repo, path):
    pointer = Path(path) / '.git'
    if not pointer.is_file() or pointer.is_symlink():
        raise Unsafe('not an ordinary registered worktree pointer')
    def common(p):
        return Path(git(p, 'rev-parse', '--path-format=absolute', '--git-common-dir').stdout.strip()).resolve()
    if common(path) != common(repo):
        raise Unsafe('different repository')
    return (git(path, 'rev-parse', 'HEAD').stdout.strip(),
            git(path, 'symbolic-ref', '--quiet', 'HEAD', accepted=(0, 1)).stdout.strip(),
            git(path, 'status', '--porcelain=v1', '-z', '--untracked-files=all',
                '--ignored=matching').stdout, pointer.read_bytes())


def in_use(path):
    r = command(['lsof', '-a', '-d', 'cwd', '-F', 'n'])
    if r.stderr.strip():
        raise Unsafe('process inspection incomplete')
    root = str(Path(path).resolve())
    return any(x[1:] == root or x[1:].startswith(root + '/')
               for x in r.stdout.splitlines() if x.startswith('n'))


def releases(file):
    if file is None:
        return {}, b''
    raw = file.read_bytes()
    data = json.loads(raw)
    if not isinstance(data, dict):
        raise Unsafe('invalid release file')
    return data, raw


def released(record, head, tip):
    if not isinstance(record, dict):
        return False
    try:
        expiry = dt.datetime.fromisoformat(record['expires_at'])
        return (record.get('released') is True and bool(record.get('owner'))
                and bool(record.get('evidence')) and record.get('head') == head
                and record.get('integration') == tip and expiry.tzinfo is not None
                and expiry > dt.datetime.now(dt.timezone.utc))
    except (KeyError, ValueError, TypeError):
        return False


def included(repo, branch, head, tip, records):
    if ancestor(repo, head, tip):
        return True
    short = branch.removeprefix('refs/heads/')
    return any(r.get('headRefName') == short and r.get('headRefOid') == head
               and r.get('baseRefName') == 'develop'
               and isinstance(r.get('mergeCommit'), dict)
               and r['mergeCommit'].get('oid')
               and ancestor(repo, r['mergeCommit']['oid'], tip) for r in records)


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    mode = p.add_mutually_exclusive_group()
    mode.add_argument('--apply', action='store_true')
    mode.add_argument('--dry-run', action='store_true')
    p.add_argument('--repo', type=Path, default=Path('/Users/jaredlikes/code/philotic-stack'))
    p.add_argument('--release-file', type=Path)
    args = p.parse_args(argv)
    repo = args.repo.resolve()
    try:
        if args.apply and args.release_file is None:
            raise Unsafe('apply requires explicit owner release file')
        tip = remote_tip(repo)
        cached = git(repo, 'rev-parse', 'refs/remotes/origin/develop').stdout.strip()
        if cached != tip:
            if not args.apply:
                raise Unsafe('cached develop differs from remote; refresh separately')
            git(repo, 'fetch', '--no-tags', 'origin', 'develop')
            if git(repo, 'rev-parse', 'refs/remotes/origin/develop').stdout.strip() != tip:
                raise Unsafe('integration moved during refresh')
        release, raw = releases(args.release_file)
        records = None
        failed = False
        keep = set(os.environ.get('PHILOTIC_WTGC_KEEP', '').replace(',', ' ').split())
        keep.add('codex/model-catalog-sync')
        for wt in worktrees(repo):
            path = Path(wt['worktree'])
            if path.resolve() == repo:
                continue
            try:
                original = snapshot(repo, path)
                head, branch, status, _ = original
                if head != wt['HEAD'] or branch != wt.get('branch', ''):
                    raise Unsafe('registration changed')
                record = release.get(str(path.resolve()))
                if not branch or branch.removeprefix('refs/heads/') in keep:
                    reason = 'detached or pinned'
                elif status:
                    reason = 'tracked, untracked, or ignored contents'
                elif not released(record, head, tip):
                    reason = 'no current explicit owner release'
                elif in_use(path):
                    reason = 'live process cwd'
                else:
                    if not ancestor(repo, head, tip) and records is None:
                        records = json.loads(command(['gh', 'pr', 'list', '--repo',
                            repository_name(repo), '--state', 'merged', '--limit', '300',
                            '--json', 'headRefName,headRefOid,baseRefName,mergeCommit']).stdout)
                        if not isinstance(records, list):
                            raise Unsafe('invalid merged PR result')
                    if not included(repo, branch, head, tip, records or []):
                        reason = 'work not proven included in develop'
                    elif not args.apply:
                        print(f'WOULD REMOVE: {path}')
                        continue
                    else:
                        if (snapshot(repo, path) != original or in_use(path)
                                or remote_tip(repo) != tip or releases(args.release_file)[1] != raw
                                or not released(record, head, tip)):
                            raise Unsafe('state or release changed before removal')
                        git(repo, 'worktree', 'remove', str(path))
                        print(f'REMOVED: {path}; branch retained')
                        continue
                print(f'PRESERVE ({reason}): {path}')
            except (Unsafe, OSError, ValueError, TypeError, AttributeError) as exc:
                failed = True
                print(f'PRESERVE (error: {exc}): {path}', file=sys.stderr)
        return 1 if failed else 0
    except (Unsafe, OSError, ValueError, TypeError) as exc:
        print(f'REFUSED: {exc}', file=sys.stderr)
        return 1


def repository_name(repo):
    url = git(repo, 'remote', 'get-url', 'origin').stdout.strip()
    return command(['gh', 'repo', 'view', url, '--json', 'nameWithOwner',
                    '--jq', '.nameWithOwner']).stdout.strip()


if __name__ == '__main__':
    sys.exit(main())
