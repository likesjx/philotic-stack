"""Synthetic supervisor wrapper: same PID survives exec into the real hotel."""
import os, pathlib, sys
assert pathlib.Path('/.dockerenv').exists()
assert os.environ.get('PERCIVAL_ISOLATED_LINUX_FIXTURE') == '1'
env = dict(os.environ)
if env.get('LISTEN_PID') == 'self': env['LISTEN_PID'] = str(os.getpid())
os.execve(sys.argv[1], sys.argv[1:], env)
