"""Package the exact hotel trio offline. No download, deployment, or credentials."""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import struct
import tarfile

BASELINE = '57b13ba0087722a2de062ff86710ae64a510d445'
CANDIDATE = '3144e42930d575532249e0133dc15e10b45afb58'
TRIO = ('aiua', 'philote', 'heal-dispatcher')
CURRENT = {
    'aiua': 'd1a87c641ca95ccd2571ffc2801dbecdd60f7aebb7a0a82a5e00d5d1f49f43e4',
    'philote': '5dcfabc4655805af344e21385bb2851eb7800a87a4d0e4426c05d2d1afe0734f',
    'heal-dispatcher': '88066269cb3e58a68d19778bfce6d384a5d8e5e7a86f908b5b1027e8d18c022c',
}
OPENROUTER = '2ecb669b801417a3b749893e83eab99f0818aa63a5a81f90f6145835666884d2'
MAX_BINARY = 1024**3
MAX_TOTAL = 512 * 1024**2
MARKERS = (b'/run/percival-personal-vault-broker.sock', b'percival-scoped-vault',
           b'PHILOTIC_SCOPED_VAULT_ENABLED')


def require(condition, message):
    if not condition:
        raise ValueError(message)


def canonical(value):
    return (json.dumps(value, sort_keys=True, separators=(',', ':')) + '\n').encode()


def regular_bytes(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as stream:
        info = os.fstat(stream.fileno())
        require(stat.S_ISREG(info.st_mode) and 64 <= info.st_size <= MAX_TOTAL,
                'bounded regular executable required')
        require(info.st_mode & 0o111 and not info.st_mode & 0o6000,
                'executable must not have setuid/setgid bits')
        content = stream.read(MAX_BINARY + 1)
        require(len(content) == info.st_size, 'binary changed while reading')
        return content


def elf_x86_64(content):
    require(content[:7] == b'\x7fELF\x02\x01\x01', 'Linux ELF64 little-endian required')
    kind, machine = struct.unpack_from('<HH', content, 16)
    require(kind in (2, 3) and machine == 62, 'x86_64 executable or PIE required')


def validate_manifest(value):
    require(isinstance(value, dict) and set(value) == {
        'schema', 'baseline_source', 'candidate_source', 'platform', 'workflow_run_id',
        'components', 'preserve_openrouter_sha256', 'activation', 'acceptance'},
        'unknown or missing manifest fields')
    require(value['schema'] == 1 and value['platform'] == 'linux-x86_64', 'manifest schema/platform')
    require(value['baseline_source'] == BASELINE and value['candidate_source'] == CANDIDATE,
            'unreviewed source pair')
    require(type(value['workflow_run_id']) is int and value['workflow_run_id'] > 0, 'workflow provenance')
    require(value['preserve_openrouter_sha256'] == OPENROUTER, 'OpenRouter preservation pin')
    require(value['activation'] == {'broker': False, 'issuer': False, 'client_admissions': False,
                                    'lifegraph': False}, 'activation remains separately gated')
    require(all(v is False for v in value['activation'].values()), 'activation flags must be false booleans')
    require(value['acceptance'] == {'mixed_version': 'pending', 'state_restore': 'pending',
                                    'systemd_isolation': 'pending'}, 'packaging cannot assert acceptance')
    require(isinstance(value['components'], dict) and set(value['components']) == set(TRIO),
            'exact binary trio required')
    for name in TRIO:
        item = value['components'][name]
        require(isinstance(item, dict) and set(item) == {'path', 'sha256', 'size', 'expected_current_sha256'},
                'component fields')
        require(item['path'] == 'bin/' + name and item['expected_current_sha256'] == CURRENT[name],
                'component path/current pin')
        require(isinstance(item['sha256'], str) and re.fullmatch('[0-9a-f]{64}', item['sha256']),
                'component digest')
        require(type(item['size']) is int and 64 <= item['size'] <= MAX_BINARY, 'component size')
    return value


def package(binaries, output, source_sha, run_id):
    require(source_sha == CANDIDATE, 'only the reviewed candidate may be packaged')
    require(type(run_id) is int and run_id > 0, 'positive GitHub workflow run ID required')
    binaries, output = Path(binaries), Path(output)
    require(binaries.is_dir() and not binaries.is_symlink(), 'binary directory required')
    contents = {}
    for name in TRIO:
        content = regular_bytes(binaries / name)
        elf_x86_64(content)
        if name == 'aiua':
            require(all(marker in content for marker in MARKERS), 'hotel broker markers missing')
            require(CANDIDATE.encode() in content, 'hotel build source marker missing')
        contents[name] = content
        require(sum(len(b) for b in contents.values()) <= MAX_TOTAL, 'trio exceeds package bound')
    manifest = validate_manifest({
        'schema': 1, 'baseline_source': BASELINE, 'candidate_source': source_sha,
        'platform': 'linux-x86_64', 'workflow_run_id': run_id,
        'components': {name: {'path': 'bin/' + name,
                              'sha256': hashlib.sha256(contents[name]).hexdigest(),
                              'size': len(contents[name]), 'expected_current_sha256': CURRENT[name]}
                       for name in TRIO},
        'preserve_openrouter_sha256': OPENROUTER,
        'activation': {'broker': False, 'issuer': False, 'client_admissions': False, 'lifegraph': False},
        'acceptance': {'mixed_version': 'pending', 'state_restore': 'pending', 'systemd_isolation': 'pending'},
    })
    manifest_bytes = canonical(manifest)
    output.mkdir(mode=0o700, parents=False, exist_ok=False)
    archive = output / 'muninn-hotel-linux-x86_64.tar.gz'
    with archive.open('xb') as raw:
        with gzip.GzipFile(fileobj=raw, mode='wb', filename='', mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode='w', format=tarfile.USTAR_FORMAT) as tar:
                for name, data, mode in [('manifest.json', manifest_bytes, 0o644)] + [
                        ('bin/' + name, contents[name], 0o755) for name in TRIO]:
                    info = tarfile.TarInfo(name)
                    info.size, info.mode, info.mtime = len(data), mode, 0
                    info.uid, info.gid, info.uname, info.gname = 0, 0, '', ''
                    tar.addfile(info, io.BytesIO(data))
    receipt = {'schema': 1, 'candidate_source': source_sha, 'workflow_run_id': run_id,
               'manifest_sha256': hashlib.sha256(manifest_bytes).hexdigest(),
               'archive_sha256': hashlib.sha256(archive.read_bytes()).hexdigest(),
               'components': {name: manifest['components'][name]['sha256'] for name in TRIO}}
    (output / 'manifest.json').write_bytes(manifest_bytes)
    (output / 'receipt.json').write_bytes(canonical(receipt))
    verify(archive, receipt)
    return receipt


def verify(archive, receipt):
    require(isinstance(receipt, dict) and set(receipt) == {'schema', 'candidate_source', 'workflow_run_id',
            'manifest_sha256', 'archive_sha256', 'components'}, 'receipt fields')
    require(receipt['schema'] == 1 and receipt['candidate_source'] == CANDIDATE, 'receipt source/schema')
    require(type(receipt['workflow_run_id']) is int and receipt['workflow_run_id'] > 0, 'receipt run')
    descriptor = os.open(archive, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as stream:
        info = os.fstat(stream.fileno())
        require(stat.S_ISREG(info.st_mode) and 1 <= info.st_size <= MAX_TOTAL, 'bounded regular archive required')
        archive_bytes = stream.read(MAX_TOTAL + 1)
        require(len(archive_bytes) == info.st_size, 'archive changed while reading')
    require(hashlib.sha256(archive_bytes).hexdigest() == receipt['archive_sha256'], 'archive digest')
    # Parse only four ordinary USTAR members, validating each header BEFORE
    # reading its payload. tarfile's general parser accepts extension records
    # whose declared expansion can be enormous even for a tiny gzip input.
    with gzip.GzipFile(fileobj=io.BytesIO(archive_bytes), mode='rb') as stream:
        total = 0
        manifest = None
        for index, name in enumerate(['manifest.json'] + ['bin/' + n for n in TRIO]):
            header = stream.read(512)
            require(len(header) == 512 and header[257:263] == b'ustar\x00', 'USTAR header required')
            member = tarfile.TarInfo.frombuf(header, 'utf-8', 'strict')
            cap = 16384 if index == 0 else MAX_TOTAL
            require(member.name == name and member.type == tarfile.REGTYPE
                    and not member.linkname and 0 <= member.size <= cap
                    and member.mode == (0o644 if index == 0 else 0o755)
                    and member.uid == member.gid == member.mtime == 0, 'canonical member metadata')
            total += member.size
            require(total <= MAX_TOTAL, 'archive expanded size')
            data = stream.read(member.size)
            require(len(data) == member.size, 'truncated archive member')
            padding = stream.read((-member.size) % 512)
            require(padding == b'\x00' * ((-member.size) % 512), 'member padding')
            if index == 0:
                require(hashlib.sha256(data).hexdigest() == receipt['manifest_sha256'], 'manifest digest')
                manifest = validate_manifest(json.loads(data))
                require(canonical(manifest) == data and manifest['workflow_run_id'] == receipt['workflow_run_id'],
                        'canonical manifest/run binding')
                require(receipt['components'] == {n: manifest['components'][n]['sha256'] for n in TRIO},
                        'receipt/component binding')
                continue
            component = name.removeprefix('bin/')
            require(member.size == manifest['components'][component]['size']
                    and hashlib.sha256(data).hexdigest() == manifest['components'][component]['sha256'],
                    'binary digest/size')
            elf_x86_64(data)
            if component == 'aiua':
                require(all(marker in data for marker in MARKERS) and CANDIDATE.encode() in data,
                        'hotel broker/build markers')
        trailer = stream.read(10241)
        require(1024 <= len(trailer) <= 10240 and not trailer.strip(b'\x00'), 'archive terminator/member count')
    return manifest


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binaries', required=True)
    parser.add_argument('--output', required=True)
    parser.add_argument('--source-sha', required=True)
    parser.add_argument('--workflow-run-id', type=int, required=True)
    args = parser.parse_args()
    print(json.dumps(package(args.binaries, args.output, args.source_sha, args.workflow_run_id), sort_keys=True))
