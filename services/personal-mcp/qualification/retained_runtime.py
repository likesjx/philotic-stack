"""Verify the exact reviewed public artifact without executing or extracting it."""
import hashlib
import io
import json
from pathlib import Path
import stat
import zipfile

RUN = 39671418572
ARTIFACT = 12379818356
WORKFLOW = 'f7f1d8484df19c9e39bbc50c0435ae89b6bb55bf'
SOURCE = '230d5aaa1ccdf9fe2b18fd14c7d66c91cff95219'
ARCHIVE = '0e747ae7c5e0653c782becb1f916de114353fc866d76bd357678ed765309abf3'
BINARY = '7f789203dc87790740399885f093b940d22f66bff66dd6541c5fdf6f3345b9dd'


def require(value):
    if not value:
        raise ValueError('retained public artifact contract refused')


def unique(pairs):
    value = {}
    for key, item in pairs:
        require(key not in value)
        value[key] = item
    return value


def verify(archive, run, artifact):
    require(run['id'] == RUN and run['head_sha'] == WORKFLOW and run['conclusion'] == 'success' and run['event'] == 'push')
    require(artifact['id'] == ARTIFACT and artifact['workflow_run']['id'] == RUN and not artifact['expired'])
    require(artifact['digest'] == 'sha256:' + ARCHIVE)
    path = Path(archive)
    require(path.is_file() and not path.is_symlink() and path.stat().st_size <= 512 * 1024**2)
    raw = path.read_bytes()
    require(len(raw) <= 512 * 1024**2 and hashlib.sha256(raw).hexdigest() == ARCHIVE)
    with zipfile.ZipFile(io.BytesIO(raw)) as z:
        entries = z.infolist()
        names = [entry.filename for entry in entries]
        require(len(entries) <= 100 and len(names) == len(set(names)))
        for entry in entries:
            require('/' not in entry.filename and '\\' not in entry.filename and entry.filename not in ('', '.', '..'))
            require(stat.S_IFMT(entry.external_attr >> 16) in (0, stat.S_IFREG))
            require(entry.file_size <= 512 * 1024**2 and not entry.flag_bits & 1)
        require(sum(entry.file_size for entry in entries) <= 600 * 1024**2)
        require({'aiua', 'candidate-receipt.json', 'combined-results.json'} <= set(names))
        for name in ('candidate-receipt.json', 'combined-results.json'):
            require(z.getinfo(name).file_size <= 65536)
        receipt = json.loads(z.read('candidate-receipt.json'), object_pairs_hook=unique)
        report = json.loads(z.read('combined-results.json'), object_pairs_hook=unique)
        require(receipt['source_commit'] == SOURCE and receipt['aiua_sha256'] == BINARY and receipt['production_selection'] is False)
        require(report['migration_units_passed'] is True and report['state_suite_returncode'] == 0 and report['broker_suite_returncode'] == 0 and report['installed_equivalence'] is False)
        binary = z.read('aiua')
        require(hashlib.sha256(binary).hexdigest() == BINARY)
        require(len(binary) > 20 and binary[:6] == b'\x7fELF\x02\x01' and binary[18:20] == b'\x3e\x00')
    return binary, {'runtime_source': SOURCE, 'producer_workflow_revision': WORKFLOW,
        'producer_run': RUN, 'producer_artifact': ARTIFACT, 'producer_archive_sha256': ARCHIVE,
        'native_aiua_sha256': BINARY, 'installed_equivalence': False, 'activation_authorized': False}


if __name__ == '__main__':
    import argparse
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('archive', 'run', 'artifact', 'output', 'receipt'):
        p.add_argument('--' + name, type=Path, required=True)
    a = p.parse_args()
    binary, receipt = verify(a.archive, json.loads(a.run.read_text()), json.loads(a.artifact.read_text()))
    with a.output.open('xb') as output:
        output.write(binary)
    a.output.chmod(0o555)
    with a.receipt.open('x') as output:
        json.dump(receipt, output, sort_keys=True, indent=2)
        output.write('\n')
