"""Make only tracked public fixture sources readable to disposable numeric UIDs."""
from pathlib import Path
import os
import subprocess

assert os.environ.get('RUNNER_ENVIRONMENT') == 'github-hosted'
assert os.environ.get('GITHUB_EVENT_NAME') == 'pull_request'
root = Path('/tmp/percival-hosted-fixtures')
root.mkdir(mode=0o755)
root.chmod(0o755)
paths = subprocess.check_output(['git', 'ls-files', '-z', 'services/personal-mcp']).decode().split('\0')
for value in filter(None, paths):
    path = Path(value)
    assert path.parts[:2] == ('services', 'personal-mcp') and '..' not in path.parts
    assert path.is_file() and not path.is_symlink() and path.stat().st_size <= 1024 * 1024
    output = root / Path(*path.parts[1:])
    output.parent.mkdir(parents=True, exist_ok=True, mode=0o755)
    for directory in output.parent.relative_to(root).parents:
        (root / directory).chmod(0o755)
    output.parent.chmod(0o755)
    output.write_bytes(path.read_bytes()); output.chmod(0o644)
print('PASS tracked public fixtures staged separately; checkout permissions unchanged')
