"""Controller-only artifact and fail-closed release/rollback plans. Never installs."""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import struct
import subprocess
import tarfile

BASE = '57b13ba0087722a2de062ff86710ae64a510d445'
COMPONENTS = {'model-controller-openrouter': ['model.openrouter'],
              'model-controller-gemini': ['model'],
              'model-controller-ollama': ['model.ollama'], 'model-router': ['model']}
PRESERVED = {'aiua', 'philote', 'heal-dispatcher'}
HEX = r'[0-9a-f]{64}'
MAX_BINARY = 256 * 1024 * 1024
SYSTEM_LIBRARIES = {'libc.so.6', 'libm.so.6', 'libgcc_s.so.1', 'libstdc++.so.6',
                    'libdl.so.2', 'libpthread.so.0', 'librt.so.1', 'libssl.so.3', 'libcrypto.so.3'}


def require(value):
    if not value: raise ValueError('controller release refused')


def canonical(value):
    return (json.dumps(value, sort_keys=True, separators=(',', ':')) + '\n').encode()


def digest(raw): return hashlib.sha256(raw).hexdigest()


def pairs(items):
    result = {}
    for key, value in items:
        require(key not in result); result[key] = value
    return result


def json_file(path):
    path = Path(path)
    require(path.is_file() and not path.is_symlink() and path.stat().st_size <= 65536)
    return json.loads(path.read_bytes(), object_pairs_hook=pairs)


def elf(raw):
    require(len(raw) >= 64 and raw[:6] == b'\x7fELF\x02\x01')
    require(struct.unpack_from('<HH', raw, 16)[0] in (2, 3))
    require(struct.unpack_from('<HH', raw, 16)[1] == 62)


def binary(path):
    require(path.is_file() and not path.is_symlink() and path.stat().st_nlink == 1)
    require(64 <= path.stat().st_size <= MAX_BINARY)
    raw = path.read_bytes(); elf(raw)
    return raw


def dependencies(path):
    result = subprocess.run(['readelf', '--dynamic', '--version-info', '--wide', str(path)],
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=True)
    require(len(result.stdout) <= 65536)
    text = result.stdout.decode('ascii')
    needed = sorted(set(re.findall(r'\(NEEDED\).*Shared library: \[([A-Za-z0-9_.+-]+)\]', text)))
    # This package contains executables only. A non-system dependency requires
    # a separately reviewed package instead of silently shipping a broken guest.
    require(set(needed) <= SYSTEM_LIBRARIES)
    versions = [tuple(map(int, value.split('.'))) for value in re.findall(r'GLIBC_([0-9]+\.[0-9]+)', text)]
    maximum = max(versions, default=(0, 0)); require(maximum <= (2, 39))
    return {'needed_libraries': needed, 'glibc_minimum': '.'.join(map(str, maximum))}


def assemble(binary_dir, output, source):
    require(re.fullmatch('[0-9a-f]{40}', source) is not None)
    checkout = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
    require(checkout == source)
    subprocess.run(['git', 'merge-base', '--is-ancestor', BASE, source], check=True)
    entries = {}; records = {}
    for name, roles in COMPONENTS.items():
        path = Path(binary_dir) / name; raw = binary(path)
        require(source.encode() in raw)  # Existing PHILOTIC_BUILD_SHA provenance.
        entries['bin/' + name] = raw
        records[name] = {'sha256': digest(raw), 'bytes': len(raw), 'roles': roles, **dependencies(path)}
    manifest = {'schema': 1, 'bundle': 'beacon-controller-diagnostics-v1', 'source_sha': source,
                'installed_source_baseline': BASE, 'target': 'x86_64-unknown-linux-gnu',
                'components': records, 'activation_authorized': False}
    entries['manifest.json'] = canonical(manifest)
    entries['release-contract.json'] = canonical(contract())
    # Include the reviewed planner in the package, with no executable installer.
    entries['package_release.py'] = Path(__file__).read_bytes()
    archive = io.BytesIO()
    with gzip.GzipFile(fileobj=archive, mode='wb', mtime=0, filename='') as zipped:
        with tarfile.open(fileobj=zipped, mode='w') as tar:
            for name, raw in sorted(entries.items()):
                info = tarfile.TarInfo(name); info.size = len(raw)
                info.mode = 0o755 if name.startswith('bin/') else 0o644
                info.mtime = 0; info.uid = info.gid = 0
                tar.addfile(info, io.BytesIO(raw))
    output = Path(output); output.mkdir(parents=True, exist_ok=True)
    raw = archive.getvalue(); filename = 'beacon-controller-diagnostics-x86_64.tar.gz'
    (output / filename).write_bytes(raw)
    (output / 'manifest.json').write_bytes(entries['manifest.json'])
    (output / 'release-contract.json').write_bytes(entries['release-contract.json'])
    receipt = {'source_sha': source, 'archive_sha256': digest(raw),
               'manifest_sha256': digest(entries['manifest.json']), 'components': records}
    (output / 'package-receipt.json').write_bytes(canonical(receipt))
    print(canonical(receipt).decode(), end='')


def contract():
    return {'schema': 1, 'bundle': 'beacon-controller-diagnostics-v1',
            'allowed_components': list(COMPONENTS), 'preserve_components': sorted(PRESERVED),
            'memory_activation': False, 'configuration_changes': False,
            'required_existing_gates': ['trusted_install_ancestry', 'existing_shared_release_fence',
                'historical_worker_terminal_proof', 'selected_binary_mutation_workers_fenced'],
            'transaction': ['acquire_existing_release_fence_and_historical_fence',
                'verify_selected_installed_hashes_and_private_guest_bindings',
                'drain_only_selected_guests', 'save_and_hash_selected_binary_preimages',
                'atomically_replace_selected_binaries', 'restart_only_selected_guests',
                'verify_running_executable_hashes_and_registration',
                'bounded_approved_synthetic_acceptance', 'commit_receipt_then_release_fences'],
            'rollback': ['retain_fences', 'stop_only_selected_guests',
                'verify_preimage_hashes', 'atomically_restore_all_selected_preimages',
                'restart_original_guests', 'verify_original_running_hashes_and_registration',
                'record_rollback_then_release_fences'],
            'failure_policy': 'rollback entire selected set; retain fence if rollback cannot be proven',
            'executor': 'release owner must bind this contract to reviewed guest drain/restart and fenced swap operations; planner never installs'}


def validate_manifest(manifest):
    require(set(manifest) == {'schema', 'bundle', 'source_sha', 'installed_source_baseline', 'target',
                              'components', 'activation_authorized'})
    require(type(manifest['schema']) is int and manifest['schema'] == 1 and manifest['bundle'] == 'beacon-controller-diagnostics-v1')
    require(manifest['installed_source_baseline'] == BASE and manifest['target'] == 'x86_64-unknown-linux-gnu')
    require(re.fullmatch('[0-9a-f]{40}', manifest['source_sha']) is not None)
    require(manifest['activation_authorized'] is False and set(manifest['components']) == set(COMPONENTS))
    for name, candidate in manifest['components'].items():
        require(type(candidate) is dict and set(candidate) == {'sha256', 'bytes', 'roles', 'needed_libraries', 'glibc_minimum'})
        require(type(candidate['sha256']) is str and re.fullmatch(HEX, candidate['sha256']) is not None)
        require(type(candidate['bytes']) is int and 64 <= candidate['bytes'] <= MAX_BINARY)
        require(candidate['roles'] == COMPONENTS[name])
        require(type(candidate['needed_libraries']) is list and len(candidate['needed_libraries']) <= 8)
        require(all(type(value) is str and re.fullmatch('[A-Za-z0-9_.+-]{1,64}', value) for value in candidate['needed_libraries']))
        require(candidate['needed_libraries'] == sorted(set(candidate['needed_libraries'])) and set(candidate['needed_libraries']) <= SYSTEM_LIBRARIES)
        require(type(candidate['glibc_minimum']) is str and re.fullmatch('[0-9]{1,2}\\.[0-9]{1,2}', candidate['glibc_minimum']))
        require(tuple(map(int, candidate['glibc_minimum'].split('.'))) <= (2, 39))
    return manifest


def verify(archive, archive_sha, manifest_sha, source):
    require(re.fullmatch(HEX, archive_sha) is not None and re.fullmatch(HEX, manifest_sha) is not None)
    path = Path(archive); require(path.is_file() and not path.is_symlink() and path.stat().st_size <= 4 * MAX_BINARY + 65536)
    raw = path.read_bytes(); require(digest(raw) == archive_sha)
    expected = {'bin/' + name for name in COMPONENTS} | {'manifest.json', 'release-contract.json', 'package_release.py'}
    entries = {}
    with tarfile.open(fileobj=io.BytesIO(raw), mode='r:gz') as tar:
        for member in tar:
            require(member.name in expected and member.name not in entries and member.isfile())
            require(0 < member.size <= (MAX_BINARY if member.name.startswith('bin/') else 65536))
            entries[member.name] = tar.extractfile(member).read(member.size + 1)
            require(len(entries[member.name]) == member.size)
    require(set(entries) == expected and digest(entries['manifest.json']) == manifest_sha)
    manifest = validate_manifest(json.loads(entries['manifest.json'], object_pairs_hook=pairs))
    require(manifest['source_sha'] == source)
    require(entries['release-contract.json'] == canonical(contract()))
    require(entries['package_release.py'] == Path(__file__).read_bytes())
    for name, candidate in manifest['components'].items():
        binary_raw = entries['bin/' + name]; elf(binary_raw)
        require(len(binary_raw) == candidate['bytes'] and digest(binary_raw) == candidate['sha256'])
        require(source.encode() in binary_raw)
    return manifest


def plan(manifest, receipt):
    validate_manifest(manifest)
    require(set(receipt) == {'schema', 'manifest_sha256', 'selected', 'preserved', 'journal', 'existing_gates'})
    require(type(receipt['schema']) is int and receipt['schema'] == 1)
    require(receipt['manifest_sha256'] == digest(canonical(manifest)))
    require(type(receipt['selected']) is dict and 0 < len(receipt['selected']) <= len(COMPONENTS))
    require(set(receipt['selected']) <= set(COMPONENTS))
    require(set(receipt['preserved']) == PRESERVED)
    require(all(type(value) is str and re.fullmatch(HEX, value) for value in receipt['preserved'].values()))
    require(type(receipt['existing_gates']) is dict and set(receipt['existing_gates']) == set(contract()['required_existing_gates']))
    require(all(type(value) is str and re.fullmatch(HEX, value) for value in receipt['existing_gates'].values()))
    journal = receipt['journal']
    require(type(journal) is dict and set(journal) == {'policy_manifest_sha256', 'storage_mode',
                'retention_seconds', 'maximum_bytes', 'access_audience_reviewed', 'export_destinations_reviewed'})
    require(re.fullmatch(HEX, journal['policy_manifest_sha256']) is not None)
    require(type(journal['retention_seconds']) is int and 0 <= journal['retention_seconds'] <= 2**63 - 1)
    require(type(journal['maximum_bytes']) is int and 0 <= journal['maximum_bytes'] <= 2**63 - 1)
    require(journal['storage_mode'] in ('volatile', 'persistent', 'disabled', 'auto'))
    require(journal['access_audience_reviewed'] is True and journal['export_destinations_reviewed'] is True)
    selected = {}
    for name, current in receipt['selected'].items():
        candidate = manifest['components'][name]
        require(type(candidate) is dict and set(candidate) == {'sha256', 'bytes', 'roles', 'needed_libraries', 'glibc_minimum'})
        require(re.fullmatch(HEX, candidate['sha256']) is not None and candidate['roles'] == COMPONENTS[name])
        require(type(current) is dict and set(current) == {'installed_sha256', 'running_sha256',
                    'guest_binding_manifest_sha256', 'guest_count', 'roles', 'all_consumers_reviewed'})
        require(all(type(current[key]) is str and re.fullmatch(HEX, current[key]) for key in
                    ('installed_sha256', 'running_sha256', 'guest_binding_manifest_sha256')))
        require(current['installed_sha256'] == current['running_sha256'])
        require(type(current['guest_count']) is int and 1 <= current['guest_count'] <= 1000)
        require(current['roles'] == COMPONENTS[name] and current['all_consumers_reviewed'] is True)
        selected[name] = {'candidate_sha256': candidate['sha256'], 'rollback_sha256': current['installed_sha256'],
                          'guest_binding_manifest_sha256': current['guest_binding_manifest_sha256'],
                          'guest_count': current['guest_count']}
    return {'schema': 1, 'mode': 'reviewed_plan_only', 'activation_authorized': False,
            'source_sha': manifest['source_sha'], 'manifest_sha256': receipt['manifest_sha256'],
            'selected': selected, 'preserved': receipt['preserved'], 'journal': journal, 'contract': contract()}


def main():
    parser = argparse.ArgumentParser(); commands = parser.add_subparsers(dest='command', required=True)
    build = commands.add_parser('assemble'); build.add_argument('--binary-dir', required=True)
    build.add_argument('--output', required=True); build.add_argument('--source', required=True)
    prepare = commands.add_parser('plan')
    for name in ('archive', 'archive-sha256', 'manifest-sha256', 'source'):
        prepare.add_argument('--' + name, required=True)
    prepare.add_argument('--operator-receipt', required=True)
    args = parser.parse_args()
    if args.command == 'assemble': assemble(args.binary_dir, args.output, args.source)
    else:
        manifest = verify(args.archive, args.archive_sha256, args.manifest_sha256, args.source)
        print(canonical(plan(manifest, json_file(args.operator_receipt))).decode(), end='')


if __name__ == '__main__':
    try: main()
    except Exception: raise SystemExit('controller release refused; no private diagnostics disclosed') from None
