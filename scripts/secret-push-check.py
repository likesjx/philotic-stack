#!/usr/bin/env python3
"""Deterministic pre-push secret scanner for outgoing commits."""

from __future__ import annotations

import argparse
import fnmatch
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import PurePosixPath


PLACEHOLDER_HINTS = (
    "your_",
    "example",
    "placeholder",
    "dummy",
    "startup-test",
    "secret://",
    "vault_",
    "{{",
    "token_here",
    "changeme",
)

SUSPICIOUS_KEY_PATTERN = re.compile(
    r"""(?ix)
    (?:
        ["']?
        (?:bot_token|telegram_bot_token|api_key|client_secret|refresh_token|access_token|webhook_secret|auth_token)
        ["']?
        \s*[:=]\s*
    |
        \b
        (?:BOT_TOKEN|TELEGRAM_BOT_TOKEN|API_KEY|CLIENT_SECRET|REFRESH_TOKEN|ACCESS_TOKEN|WEBHOOK_SECRET|AUTH_TOKEN)
        \s*=\s*
    )
    """
)

TELEGRAM_BOT_TOKEN_PATTERN = re.compile(r"\b[0-9]{8,12}:[A-Za-z0-9_-]{30,}\b")
LONG_SECRET_VALUE_PATTERN = re.compile(r"""[:=]\s*["']?[A-Za-z0-9_\-\/+=]{24,}["']?\s*$""")

ALLOWED_MESH_CONFIG_FILES = {
    "mesh-config.example.json",
}


@dataclass
class Finding:
    commit: str
    path: str
    reason: str
    line: str | None = None


def run_git(*args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return result.stdout


def is_placeholder(text: str) -> bool:
    lowered = text.lower()
    return any(hint in lowered for hint in PLACEHOLDER_HINTS)


def path_is_forbidden(path: str) -> str | None:
    name = PurePosixPath(path).name
    if fnmatch.fnmatch(name, "mesh-config.*") and name not in ALLOWED_MESH_CONFIG_FILES:
        return "tracked mesh-config variant is forbidden"
    if fnmatch.fnmatch(name, "secrets*.json"):
        return "tracked secrets*.json file is forbidden"
    if name in {"credentials.json"}:
        return "tracked credentials file is forbidden"
    return None


def line_is_secret(line: str) -> str | None:
    if is_placeholder(line):
        return None
    if TELEGRAM_BOT_TOKEN_PATTERN.search(line):
        return "Telegram bot token pattern detected"
    if SUSPICIOUS_KEY_PATTERN.search(line) and LONG_SECRET_VALUE_PATTERN.search(line):
        return "suspicious secret assignment detected"
    return None


def scan_commit(commit: str) -> list[Finding]:
    findings: list[Finding] = []

    changed_paths = run_git(
        "diff-tree",
        "--root",
        "--no-commit-id",
        "--name-only",
        "-r",
        "--diff-filter=AM",
        commit,
    ).splitlines()
    for path in changed_paths:
        reason = path_is_forbidden(path)
        if reason:
            findings.append(Finding(commit=commit, path=path, reason=reason))

    patch = run_git(
        "diff-tree",
        "--root",
        "--no-commit-id",
        "--no-color",
        "-U0",
        "-r",
        "--diff-filter=AM",
        commit,
    )

    current_path: str | None = None
    for raw_line in patch.splitlines():
        if raw_line.startswith("+++ b/"):
            current_path = raw_line[6:]
            continue
        if not raw_line.startswith("+") or raw_line.startswith("+++"):
            continue
        if current_path is None:
            continue
        reason = line_is_secret(raw_line[1:])
        if reason:
            findings.append(
                Finding(
                    commit=commit,
                    path=current_path,
                    reason=reason,
                    line=raw_line[1:].strip(),
                )
            )
    return findings


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--commit",
        action="append",
        default=[],
        help="commit sha to scan; may be repeated",
    )
    args = parser.parse_args()

    commits = [commit for commit in args.commit if commit]
    if not commits:
        print("secret-push-check: no commits provided; nothing to scan")
        return 0

    findings: list[Finding] = []
    for commit in commits:
        findings.extend(scan_commit(commit))

    if not findings:
        print(f"secret-push-check: scanned {len(commits)} commit(s); no forbidden secrets found")
        return 0

    print("secret-push-check: push blocked due to deterministic secret findings", file=sys.stderr)
    for finding in findings:
        print(
            f"- commit {finding.commit} path {finding.path}: {finding.reason}",
            file=sys.stderr,
        )
        if finding.line:
            print(f"  line: {finding.line}", file=sys.stderr)
    print(
        "Rotate leaked secrets if already pushed, remove the secret-bearing file/content, and recommit before pushing again.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
