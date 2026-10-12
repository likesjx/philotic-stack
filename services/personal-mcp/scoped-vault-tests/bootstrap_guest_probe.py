"""An actual materialized synthetic guest reports only descriptor isolation."""
import json, os, pathlib, socket, stat, sys
assert pathlib.Path('/.dockerenv').exists()
found = False
for name in os.listdir('/proc/self/fd'):
    try:
        fd = int(name)
        if not stat.S_ISSOCK(os.fstat(fd).st_mode): continue
        with socket.socket(fileno=os.dup(fd)) as probe:
            if probe.getsockname() == '/run/percival-personal-vault-broker.sock': found = True
    except (OSError, ValueError): pass
pathlib.Path(sys.argv[1]).write_text(json.dumps({'scoped_fd_inherited': found, 'uid': os.geteuid()}))
