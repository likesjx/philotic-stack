#!/usr/bin/env python3
"""Export selected committed source for ordinary fallback CI; never fetch or alter a worktree."""
import argparse
import hashlib
import json
import os
import pathlib
import re
import subprocess
import tarfile
import tempfile

SELECTIONS = {
    "likesjx/philotic-stack": ["services/personal-mcp", "scripts", "crates", "Cargo.toml", "Cargo.lock",
                              "release", "docs/architecture/outbound-egress-inventory.json", ".github/workflows",
                              "ansible/roles/percival_personal_mcp", "rust-toolchain.toml", "vendor/neo4rs"],
    "likesjx/jaredlikes-com": ["ops", "scripts", "src", "tests", "package.json", "package-lock.json",
                              "tsconfig.json", "tsconfig.authority.json", "Dockerfile", ".github/workflows"],
}


def git(repo, *args, **kwargs):
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    env.update(GIT_OPTIONAL_LOCKS="0", GIT_NO_LAZY_FETCH="1", GIT_TERMINAL_PROMPT="0")
    return subprocess.run(["git", "--no-optional-locks", "-c", "core.fsmonitor=false", "-c", "protocol.allow=never",
                           "-C", str(repo), *args], env=env, check=True, **kwargs)


def export(repo, reference, project, destination):
    if not reference or reference.startswith("-") or not re.fullmatch(r"[A-Za-z0-9_./-]+", reference):
        raise ValueError("invalid local Git reference")
    if project not in SELECTIONS:
        raise ValueError("unsupported project")
    repo, destination = pathlib.Path(repo).resolve(), pathlib.Path(destination).absolute()
    if destination.exists() or destination.is_symlink():
        raise ValueError("destination must not already exist")
    sha = git(repo, "rev-parse", "--verify", reference + "^{commit}", capture_output=True, text=True).stdout.strip()
    tree = git(repo, "rev-parse", "--verify", sha + "^{tree}", capture_output=True, text=True).stdout.strip()
    with tempfile.TemporaryDirectory(prefix="fallback-export-") as temporary:
        archive_path = pathlib.Path(temporary) / "source.tar"
        git(repo, "archive", "--format=tar", "--output=" + str(archive_path), sha, "--", *SELECTIONS[project])
        with tarfile.open(archive_path) as archive:
            members = archive.getmembers()
            if len(members) > 50000 or sum(member.size for member in members) > 512 * 1024**2:
                raise ValueError("selected source snapshot exceeds bounded export size")
            seen = set()
            for member in members:
                path = pathlib.PurePosixPath(member.name)
                if path.is_absolute() or ".." in path.parts or member.name in seen or not (member.isfile() or member.isdir()):
                    raise ValueError("unsupported archive path or member")
                seen.add(member.name)
            destination.mkdir(mode=0o700)
            archive.extractall(destination, members=members, filter="data")
    files = {path.relative_to(destination).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
             for path in sorted(destination.rglob("*")) if path.is_file()}
    identity = {"project": project, "source_sha": sha, "source_tree": tree, "reference": reference,
                "selected_paths": SELECTIONS[project], "files": files,
                "scope": "Selected committed source; not a full checkout or GitHub attestation."}
    (destination / "SOURCE_IDENTITY.json").write_text(json.dumps(identity, indent=2) + "\n")
    return {"destination": str(destination), "project": project, "source_sha": sha, "files": len(files),
            "manifest_sha256": hashlib.sha256((destination / "SOURCE_IDENTITY.json").read_bytes()).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, type=pathlib.Path)
    parser.add_argument("--ref", required=True)
    parser.add_argument("--project", required=True, choices=sorted(SELECTIONS))
    parser.add_argument("--destination", required=True, type=pathlib.Path)
    args = parser.parse_args()
    try:
        print(json.dumps(export(args.repo, args.ref, args.project, args.destination), indent=2))
        return 0
    except (ValueError, OSError, subprocess.CalledProcessError, tarfile.TarError) as error:
        print(json.dumps({"status": "unavailable", "reason": str(error)}))
        return 78


if __name__ == "__main__":
    raise SystemExit(main())
