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

  verify --dir D
      Re-hash D/bin/* and check against D/manifest.json and D/SHA256SUMS.
      Exit 1 on any mismatch, missing or extra binary.

  compare --manifest M --actual A [--installed I] [--label HOST]
      A holds sha256sum-format lines ("<sha256>  <path>") for the binaries
      on a host. I (optional) holds the post-codesign hashes recorded at
      install time on macOS (INSTALLED_SHA256SUMS). Prints a PASS/FAIL table;
      exit 1 when any binary is missing or matches neither.

Standard library only.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import json
import os
import sys
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
    return tag[1:] if tag.startswith("v") else tag


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
    print(f"release-manifest: {len(expected)} binaries verified against manifest ({manifest['version']}, {manifest['sha'][:12]})")
    return 0


def compare(manifest: dict, actual: dict[str, str], installed: dict[str, str]) -> tuple[list[tuple[str, str, str, str]], bool]:
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
        elif installed.get(name) == got:
            # macOS: re-signed at install time after the pre-sign hash was
            # verified against this manifest; matches the recorded post-sign hash.
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
    rows, ok = compare(manifest, actual, installed)
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
    v.set_defaults(func=cmd_verify)

    c = sub.add_parser("compare")
    c.add_argument("--manifest", required=True)
    c.add_argument("--actual", required=True)
    c.add_argument("--installed")
    c.add_argument("--label")
    c.set_defaults(func=cmd_compare)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
