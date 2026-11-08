"""Fail-closed launch for a fresh, synthetic graph. Not a production installer."""
import os
from pathlib import Path
import re
import stat
import sys

PASSFILE = "/fixture/passfile"
BINARY = "/usr/lib/memgraph/memgraph"
FLAGS = [
    "--bolt-address=127.0.0.1", "--bolt-port=7687",
    "--telemetry-enabled=false", "--log-level=WARNING",
    "--auth-password-permit-null=false",
    "--data-directory=/var/lib/memgraph", "--data-recovery-on-startup=true",
    "--storage-wal-enabled=true", "--storage-wal-file-flush-every-n-tx=1",
    "--storage-snapshot-interval-sec=300", "--storage-snapshot-on-exit=true",
]

def read_private(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        st = os.fstat(fd)
        if (not stat.S_ISREG(st.st_mode) or st.st_uid != os.getuid()
                or stat.S_IMODE(st.st_mode) != 0o600 or st.st_nlink != 1
                or st.st_size > 4096):
            raise ValueError("private_file")
        data = os.read(fd, 4097)
        if len(data) != st.st_size:
            raise ValueError("private_file")
        return data
    finally:
        os.close(fd)

def validate_passfile(data):
    # Avoid the upstream escaping/parser edge cases. One exact synthetic pair.
    if not re.fullmatch(rb"beta_fixture:[a-f0-9]{64}\n", data):
        raise ValueError("passfile_format")

def launch(path=PASSFILE, environ=None, execute=os.execve):
    env = dict(os.environ if environ is None else environ)
    if any(k in env for k in ("MEMGRAPH_USER", "MEMGRAPH_PASSWORD", "MEMGRAPH_CONFIG")):
        raise ValueError("conflicting_environment")
    validate_passfile(read_private(path))
    env["MEMGRAPH_PASSFILE"] = path
    env["HOME"] = "/nonexistent"
    execute(BINARY, [BINARY, *FLAGS], env)

if __name__ == "__main__":
    try:
        launch()
    except Exception:
        print("graph_start_refused:invalid_private_bootstrap", file=sys.stderr)
        sys.exit(1)
