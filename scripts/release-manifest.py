#!/usr/bin/env python3
"""Release manifest tool (proposal:release-train R3/R7).

A release tarball has this layout:

    bin/<name>...      the binaries
    SHA256SUMS         "<sha256>  bin/<name>" per binary (sha256sum -c compatible)
    manifest.json      {version, tag, sha, built_at, target, bins:[{name, sha256}]}

Subcommands:

  generate --dir D --tag vX.Y.Z --sha <git sha> --target <triple> [--built-at ISO]
           [--require name ...]
      Hash D/bin/*, write D/SHA256SUMS and D/manifest.json. Fails if a
      --require'd binary is missing.

  verify --dir D [--check-aiua-version]
      Re-hash D/bin/* and check against D/manifest.json and D/SHA256SUMS.
      Exit 1 on any mismatch, missing or extra binary.
      The optional native smoke requires aiua --version to match tag/version/SHA.

  check-version --manifest M --output-file F
      Compare a captured aiua --version with the manifest without executing it.

  check-metadata --manifest M --tag T --target TARGET
      Bind a downloaded manifest to the requested release and platform.

  compare --manifest M --actual A [--installed I --presign P] [--label HOST]
      A holds sha256sum-format lines ("<sha256>  <path>") for the binaries
      on a host. On macOS, I is INSTALLED_SHA256SUMS (post-codesign hashes
      recorded at install time) and P is the release dir's SHA256SUMS (the
      pre-sign hashes the install verified). A binary whose hash differs from
      the manifest is RESIGNED (a pass) only if P matches the manifest AND A
      matches I. Prints a PASS/FAIL table; exit 1 on any FAIL or MISSING.

Standard library only.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

MANIFEST_NAME = "manifest.json"
SUMS_NAME = "SHA256SUMS"


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def list_bins(bin_dir: Path) -> list[Path]:
    if not bin_dir.is_dir():
        return []
    return sorted(p for p in bin_dir.iterdir() if p.is_file() and not p.name.startswith("."))


def version_from_tag(tag: str) -> str:
    number = r"(?:0|[1-9][0-9]*)"
    if not isinstance(tag, str) or not re.fullmatch(
        rf"v{number}\.{number}\.{number}(?:-(?:alpha|beta|rc)\.{number})?", tag
    ):
        raise ValueError("invalid release tag")
    return tag[1:]


def validate_metadata(manifest: dict) -> None:
    if not isinstance(manifest, dict):
        raise ValueError("manifest must be an object")
    if manifest.get("version") != version_from_tag(manifest.get("tag")):
        raise ValueError("manifest version differs from release tag")
    if not isinstance(manifest.get("sha"), str) or not re.fullmatch(r"[0-9a-f]{40}", manifest["sha"]):
        raise ValueError("release requires a full lowercase commit SHA")


def check_version(manifest: dict, reported: str) -> bool:
    validate_metadata(manifest)
    return reported.strip() == f"aiua {manifest['version']} ({manifest['sha']})"


def build_manifest(root: Path, tag: str, sha: str, target: str, built_at: str | None) -> dict:
    bins = [{"name": p.name, "sha256": sha256_file(p)} for p in list_bins(root / "bin")]
    return {
        "version": version_from_tag(tag),
        "tag": tag,
        "sha": sha,
        "built_at": built_at
        or _dt.datetime.now(_dt.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
        "target": target,
        "bins": bins,
    }


def parse_sums(text: str) -> dict[str, str]:
    """Parse sha256sum/shasum output into {basename: sha256}."""
    out: dict[str, str] = {}
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(None, 1)
        if len(parts) != 2:
            continue
        digest, name = parts
        name = name.lstrip("*").strip()  # binary-mode marker
        out[os.path.basename(name)] = digest.lower()
    return out


def cmd_generate(args: argparse.Namespace) -> int:
    root = Path(args.dir)
    try:
        validate_metadata({"tag": args.tag, "version": version_from_tag(args.tag), "sha": args.sha})
    except ValueError as exc:
        print(f"release-manifest: {exc}", file=sys.stderr)
        return 1
    bins = list_bins(root / "bin")
    if not bins:
        print(f"release-manifest: no binaries in {root / 'bin'}", file=sys.stderr)
        return 1
    present = {p.name for p in bins}
    missing = [r for r in (args.require or []) if r not in present]
    if missing:
        print(f"release-manifest: required binaries missing: {' '.join(missing)}", file=sys.stderr)
        return 1
    manifest = build_manifest(root, args.tag, args.sha, args.target, args.built_at)
    (root / SUMS_NAME).write_text("".join(f"{b['sha256']}  bin/{b['name']}\n" for b in manifest["bins"]))
    (root / MANIFEST_NAME).write_text(json.dumps(manifest, indent=2, sort_keys=False) + "\n")
    print(f"release-manifest: {len(manifest['bins'])} binaries, {args.target}, {args.tag} ({args.sha[:12]})")
    return 0


def cmd_verify(args: argparse.Namespace) -> int:
    root = Path(args.dir)
    try:
        manifest = json.loads((root / MANIFEST_NAME).read_text())
    except (OSError, ValueError) as exc:
        print(f"release-manifest: cannot read {root / MANIFEST_NAME}: {exc}", file=sys.stderr)
        return 1
    for key in ("version", "sha", "built_at", "target", "bins"):
        if key not in manifest:
            print(f"release-manifest: manifest.json lacks '{key}'", file=sys.stderr)
            return 1
    try:
        validate_metadata(manifest)
    except ValueError as exc:
        print(f"release-manifest: {exc}", file=sys.stderr)
        return 1
    expected = {b["name"]: b["sha256"] for b in manifest["bins"]}
    sums_path = root / SUMS_NAME
    sums = parse_sums(sums_path.read_text()) if sums_path.exists() else {}
    actual = {p.name: sha256_file(p) for p in list_bins(root / "bin")}
    errors = []
    for name, digest in expected.items():
        if name not in actual:
            errors.append(f"missing binary: {name}")
        elif actual[name] != digest:
            errors.append(f"hash mismatch: {name}")
        if sums and sums.get(name) != digest:
            errors.append(f"SHA256SUMS disagrees with manifest: {name}")
    for name in actual:
        if name not in expected:
            errors.append(f"binary not in manifest: {name}")
    if not sums_path.exists():
        errors.append("SHA256SUMS missing")
    for e in errors:
        print(f"  ✗ {e}", file=sys.stderr)
    if errors:
        return 1
    if args.check_aiua_version:
        try:
            # --version exits before hotel bootstrap; isolate logging and remove
            # ambient credentials/config from this native packaging smoke.
            with tempfile.TemporaryDirectory(prefix="philotic-version-") as work:
                result = subprocess.run(
                    [str((root / "bin/aiua").resolve()), "--version"],
                    cwd=work, env={"PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                                   "PHILOTIC_LOG_DIR": work},
                    capture_output=True, text=True, timeout=10, check=False,
                )
            if result.returncode != 0 or not check_version(manifest, result.stdout):
                raise ValueError("aiua --version does not match manifest version/SHA")
        except (OSError, ValueError, subprocess.TimeoutExpired) as exc:
            print(f"release-manifest: {exc}", file=sys.stderr)
            return 1
    print(f"release-manifest: {len(expected)} binaries verified against manifest ({manifest['version']}, {manifest['sha'][:12]})")
    return 0


def cmd_check_metadata(args: argparse.Namespace) -> int:
    try:
        manifest = json.loads(Path(args.manifest).read_text())
        validate_metadata(manifest)
        if manifest['tag'] != args.tag or manifest.get('target') != args.target:
            raise ValueError("manifest does not match requested tag/target")
    except (OSError, ValueError) as exc:
        print(f"release-manifest: {exc}", file=sys.stderr)
        return 1
    return 0


def cmd_check_version(args: argparse.Namespace) -> int:
    try:
        manifest = json.loads(Path(args.manifest).read_text())
        if not check_version(manifest, Path(args.output_file).read_text()):
            raise ValueError("aiua --version does not match manifest version/SHA")
    except (OSError, ValueError) as exc:
        print(f"release-manifest: {exc}", file=sys.stderr)
        return 1
    print(f"release-manifest: aiua version/SHA verified ({manifest['version']}, {manifest['sha'][:12]})")
    return 0


def compare(
    manifest: dict,
    actual: dict[str, str],
    installed: dict[str, str],
    presign: dict[str, str] | None = None,
) -> tuple[list[tuple[str, str, str, str]], bool]:
    presign = presign or {}
    rows: list[tuple[str, str, str, str]] = []
    ok = True
    for b in manifest["bins"]:
        name, want = b["name"], b["sha256"].lower()
        got = actual.get(name)
        if got is None:
            status = "MISSING"
            ok = False
        elif got == want:
            status = "PASS"
        elif presign.get(name) == want and installed.get(name) == got:
            # macOS: the unpacked (pre-sign) binary matched this manifest, was
            # re-signed at install time, and is unchanged since (matches the
            # post-sign hash recorded then).
            status = "RESIGNED"
        else:
            status = "FAIL"
            ok = False
        rows.append((name, want, got or "-", status))
    return rows, ok


def cmd_compare(args: argparse.Namespace) -> int:
    manifest = json.loads(Path(args.manifest).read_text())
    actual = parse_sums(Path(args.actual).read_text())
    installed = parse_sums(Path(args.installed).read_text()) if args.installed and Path(args.installed).exists() else {}
    presign = parse_sums(Path(args.presign).read_text()) if args.presign and Path(args.presign).exists() else {}
    rows, ok = compare(manifest, actual, installed, presign)
    width = max([len(r[0]) for r in rows] + [6])
    label = f" on {args.label}" if args.label else ""
    print(f"Release {manifest.get('tag', manifest['version'])} ({manifest['target']}, sha {manifest['sha'][:12]}){label}")
    print(f"  {'BINARY'.ljust(width)}  {'EXPECTED':12}  {'ACTUAL':12}  STATUS")
    for name, want, got, status in rows:
        print(f"  {name.ljust(width)}  {want[:12]:12}  {got[:12]:12}  {status}")
    counts: dict[str, int] = {}
    for r in rows:
        counts[r[3]] = counts.get(r[3], 0) + 1
    print("  " + ", ".join(f"{k}={v}" for k, v in sorted(counts.items())))
    return 0 if ok else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="cmd", required=True)

    g = sub.add_parser("generate")
    g.add_argument("--dir", required=True)
    g.add_argument("--tag", required=True)
    g.add_argument("--sha", required=True)
    g.add_argument("--target", required=True)
    g.add_argument("--built-at")
    g.add_argument("--require", nargs="*", default=[])
    g.set_defaults(func=cmd_generate)

    v = sub.add_parser("verify")
    v.add_argument("--dir", required=True)
    v.add_argument("--check-aiua-version", action="store_true")
    v.set_defaults(func=cmd_verify)

    cm = sub.add_parser("check-metadata")
    cm.add_argument("--manifest", required=True)
    cm.add_argument("--tag", required=True)
    cm.add_argument("--target", required=True)
    cm.set_defaults(func=cmd_check_metadata)

    cv = sub.add_parser("check-version")
    cv.add_argument("--manifest", required=True)
    cv.add_argument("--output-file", required=True)
    cv.set_defaults(func=cmd_check_version)

    c = sub.add_parser("compare")
    c.add_argument("--manifest", required=True)
    c.add_argument("--actual", required=True)
    c.add_argument("--installed")
    c.add_argument("--presign")
    c.add_argument("--label")
    c.set_defaults(func=cmd_compare)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
