"""Disposable synthetic IPC test; no model HTTP or installed hotel access."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

OLD = "57b13ba0087722a2de062ff86710ae64a510d445"
NEW = "65494817fa4190ac2f5a26138297925650fb128a"
HERE = Path(__file__).resolve().parent


def run(argv, cwd, env=None):
    subprocess.run(argv, cwd=cwd, env=env, check=True)


def main():
    old, new = [Path(p).resolve() for p in sys.argv[1:]]
    for checkout, revision, patch in [
        (old, OLD, "old-aiua-test.patch"),
        (new, NEW, "new-controller-test.patch"),
    ]:
        actual = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=checkout, text=True
        ).strip()
        if actual != revision:
            raise RuntimeError("source revision mismatch")
        if subprocess.check_output(["git", "status", "--porcelain"], cwd=checkout):
            raise RuntimeError("source checkout must be clean")
        run(["git", "apply", "--check", str(HERE / patch)], checkout)
        run(["git", "apply", str(HERE / patch)], checkout)
    # No production credential variables enter either test process. Rust tooling
    # uses the runner's existing public toolchain/cache; no software setup here.
    allowed = ["PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "LD_LIBRARY_PATH",
               "CARGO_INCREMENTAL", "CARGO_PROFILE_DEV_DEBUG",
               "CARGO_PROFILE_TEST_DEBUG", "CMAKE_POLICY_VERSION_MINIMUM"]
    env = {key: os.environ[key] for key in allowed if key in os.environ}
    env["PHILOTIC_KEYCHAIN_ENABLED"] = "0"
    with tempfile.TemporaryDirectory(prefix="beacon-public-ipc-") as scratch:
        fixture = Path(scratch) / "wire.json"
        env["BEACON_COMPAT_WIRE_FIXTURE"] = str(fixture)
        for checkout, package, test in [
            (old, "aiua", "beacon_metadata_cross_version_old_hotel"),
            (new, "model-router", "beacon_metadata_cross_version_new_controller"),
        ]:
            # cargo permits a misspelled filter to pass with zero tests. List and
            # require exactly one match before executing it.
            target = ["--bin", "aiua"] if package == "aiua" else ["--lib"]
            command = ["cargo", "test", "--locked", "-p", package] + target + [test]
            listing = subprocess.check_output(command + ["--", "--list"],
                                              cwd=checkout, env=env, text=True)
            matches = [line for line in listing.splitlines() if line.endswith(": test")]
            if len(matches) != 1 or test not in matches[0]:
                raise RuntimeError("expected exactly one synthetic test")
            run(command + ["--", "--nocapture", "--test-threads=1"], checkout, env)
            if checkout == old:
                frames = json.loads(fixture.read_text())
                if len(frames) != 3:
                    raise RuntimeError("old hotel did not export three frames")
        print("PASS: three old-hotel IPC frames parsed by candidate; synthetic provider reached")
        print("UNPROVEN: real providers, role materialization, systemd, binary backport and rollout")


if __name__ == "__main__":
    main()
