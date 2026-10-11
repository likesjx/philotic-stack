"""Package one public candidate binary only. Never install or access runtime state."""
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import tarfile
from source_contract import BASE, check

def sha(raw): return hashlib.sha256(raw).hexdigest()
def canonical(value): return (json.dumps(value, sort_keys=True, separators=(',', ':')) + '\n').encode()

check()
source = os.environ['PHILOTIC_BUILD_SHA']
assert re.fullmatch('[0-9a-f]{40}', source)
assert subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip() == source
assert not subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=all'])
binary = Path('/tmp/percival-hotel-target/release/aiua')
assert binary.is_file() and not binary.is_symlink() and 64 <= binary.stat().st_size <= 256 * 1024 * 1024
raw = binary.read_bytes()
assert raw[:6] == b'\x7fELF\x02\x01' and struct.unpack_from('<HH', raw, 16) in ((2, 62), (3, 62))
assert source.encode() in raw
metadata = subprocess.check_output(['readelf', '--dynamic', '--version-info', '--wide', str(binary)], timeout=10)
assert len(metadata) <= 262144
text = metadata.decode('ascii'); needed = []
for line in text.splitlines():
    if '(NEEDED)' in line:
        match = re.fullmatch(r'.*\(NEEDED\)\s+Shared library: \[([^\]\n]+)\]\s*', line)
        assert match
        needed.append(match.group(1))
libraries = sorted(set(needed))
assert set(libraries) <= {'ld-linux-x86-64.so.2', 'libc.so.6', 'libm.so.6', 'libgcc_s.so.1',
    'libstdc++.so.6', 'libdl.so.2', 'libpthread.so.0', 'librt.so.1', 'libssl.so.3', 'libcrypto.so.3'}
glibc = max([tuple(map(int, version.split('.'))) for version in re.findall(r'GLIBC_([0-9]+\.[0-9]+)', text)], default=(0, 0))
assert glibc <= (2, 39)
manifest = {'schema': 1, 'source_sha': source, 'installed_source_baseline': BASE,
    'target': 'x86_64-unknown-linux-gnu', 'component': 'aiua', 'sha256': sha(raw),
    'bytes': len(raw), 'needed_libraries': libraries, 'glibc_minimum': '.'.join(map(str, glibc)),
    'scoped_broker_default_enabled': False, 'namespace': 'percival_connection_test',
    'activation_authorized': False, 'includes_credentials_or_grants': False}
entries = {'bin/aiua': raw, 'manifest.json': canonical(manifest)}
buffer = io.BytesIO()
with gzip.GzipFile(fileobj=buffer, mode='wb', mtime=0, filename='') as zipped:
    with tarfile.open(fileobj=zipped, mode='w') as tar:
        for name, value in sorted(entries.items()):
            info = tarfile.TarInfo(name); info.size = len(value); info.mode = 0o755 if name == 'bin/aiua' else 0o644
            info.mtime = 0; info.uid = info.gid = 0
            tar.addfile(info, io.BytesIO(value))
archive = buffer.getvalue()
with tarfile.open(fileobj=io.BytesIO(archive), mode='r:gz') as tar:
    members = tar.getmembers(); assert len(members) == 2 and {m.name for m in members} == set(entries)
    for member in members: assert member.isfile() and tar.extractfile(member).read() == entries[member.name]
receipt = {'source_sha': source, 'archive_sha256': sha(archive), 'manifest_sha256': sha(entries['manifest.json']), 'manifest': manifest}
output = Path('dist-ci/muninn-scoped-57b'); output.mkdir(parents=True, exist_ok=False)
(output / 'muninn-scoped-broker-57b-x86_64.tar.gz').write_bytes(archive)
(output / 'manifest.json').write_bytes(entries['manifest.json'])
(output / 'package-receipt.json').write_bytes(canonical(receipt))
print(canonical(receipt).decode(), end='')
