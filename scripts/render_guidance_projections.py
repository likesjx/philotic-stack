#!/usr/bin/env python3
"""Refresh only six checked-in guidance projections through canonical Rust renderers.

Default: compare exact bytes. --write: update those six files in this checkout.
Preserve checked-in role, skill and reference inputs; do not read graph desired
state or follow referenced home paths. No harness apply, settings, services or
memory access. Requires only Python's standard library and rustc.
"""
import argparse
import pathlib
import re
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
PROJECTIONS = [".claude/harness/claude-local.md"] + [
    f".agents/workflows/gemini-{role}-harness.md"
    for role in ("antigravity", "architect", "implementer", "reviewer", "verifier")
]


def projected_outputs(root=ROOT):
    """Return exact outputs; all Rust code comes from the canonical source."""
    source = (root / "crates/philotic-web/src/harness.rs").read_text()
    constant = re.search(r'const MUNINN_HARNESS_INSTRUCTIONS: &str = .*?^";',
                         source, re.S | re.M).group()
    functions = [re.search(r"^fn " + name + r"\(.*?^}", source, re.S | re.M).group()
                 for name in ("short_harness_id", "render_claude_code_markdown",
                              "render_antigravity_workspace_hook")]
    driver = r'''
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let output = match args[1].as_str() {
        "claude" => render_claude_code_markdown(&args[2], &args[3], &args[4..]),
        "hook" => render_antigravity_workspace_hook(&args[2], Path::new(&args[3])),
        _ => panic!("unsupported renderer"),
    };
    print!("{output}");
}
'''
    outputs = {}
    with tempfile.TemporaryDirectory(prefix="guidance-render-") as tmp:
        tmp = pathlib.Path(tmp)
        (tmp / "render.rs").write_text("use std::path::Path;\n" + constant + "\n"
                                      + "\n".join(functions) + driver)
        subprocess.run(["rustc", "--edition=2021", str(tmp / "render.rs"),
                        "-o", str(tmp / "render")], check=True)
        for relative in PROJECTIONS:
            path = root / relative
            # Never redirect an allowlisted write through a symlink.
            if path.resolve() != root.resolve() / relative:
                raise ValueError(f"non-canonical projection path: {relative}")
            text = path.read_text()
            if relative.startswith(".claude/"):
                role = re.search(r"## Role Charter\n\n([\w-]+)\n", text).group(1)
                section = text.split("## Active Skills\n\n", 1)[1].split("\n## ", 1)[0]
                skills = re.findall(r"^- ([\w-]+)$", section, re.M)
                if not skills:
                    raise ValueError("missing checked-in skill inputs")
                args = ["claude", "claude-local", role, *skills]
            else:
                harness = pathlib.Path(relative).name.removesuffix("-harness.md")
                reference = re.search(
                    r"^Please read the projected harness instructions at: (.+)$", text, re.M
                ).group(1)
                args = ["hook", harness, reference]
            outputs[relative] = subprocess.run(
                [str(tmp / "render"), *args], check=True, capture_output=True
            ).stdout
    return outputs


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="refresh allowlisted local files")
    args = parser.parse_args()
    outputs = projected_outputs()
    drift = [name for name, data in outputs.items() if (ROOT / name).read_bytes() != data]
    if args.write:
        for name in drift:
            (ROOT / name).write_bytes(outputs[name])
        print(f"Regenerated {len(drift)} local guidance projections; no graph state changed.")
    elif drift:
        print("STALE generated guidance:\n" + "\n".join(drift))
        return 1
    else:
        print("All six local projections exactly match canonical renderer output.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
