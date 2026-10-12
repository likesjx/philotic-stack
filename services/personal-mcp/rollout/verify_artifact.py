"""Verify an official Actions ZIP and its bounded package; never install or deploy."""
import hashlib
import io
import json
from pathlib import Path
import stat
import zipfile

import package

RUN = 38077303896
ARTIFACT = 11679148793
HEAD = '3bf64966be1cb75a25b669d960f6112242faacf0'
ZIP_SHA = '0ccc3a0a4f516bde9b49dec7a656a5e5aba81b50f30002a721f16550e42993a5'
ARCHIVE_SHA = 'dcc070c6afc9a27014d9e0d72d6e258f8d3c3f8933a7f41293badd0e09dd55b1'
MANIFEST_SHA = '1c2c742d35ea20b3691c45d93be0dcbe6cdae353c0532dd5bb4c8df8ce027355'
MEMBERS = {'manifest.json', 'receipt.json', 'muninn-hotel-linux-x86_64.tar.gz'}


def require(value):
    if not value:
        raise ValueError('official artifact verification refused')


def provenance(run, artifact):
    require(run['id'] == RUN and run['repository']['full_name'] == 'likesjx/philotic-stack'
            and run['head_repository']['full_name'] == 'likesjx/philotic-stack'
            and run['head_sha'] == HEAD and run['path'] == '.github/workflows/muninn-bounded-package.yml'
            and run['event'] == 'pull_request' and run['status'] == 'completed' and run['conclusion'] == 'success')
    require(artifact['id'] == ARTIFACT and artifact['name'] == 'muninn-bounded-hotel-linux-x86_64'
            and artifact['expired'] is False and artifact['digest'] == 'sha256:' + ZIP_SHA
            and artifact['workflow_run']['id'] == RUN and artifact['workflow_run']['head_sha'] == HEAD)


def verify_zip(content, output, *, expected_zip_sha=ZIP_SHA):
    require(1 <= len(content) <= package.MAX_TOTAL and hashlib.sha256(content).hexdigest() == expected_zip_sha)
    output = Path(output)
    require(output.absolute() == output.resolve())
    # Inspect the entire central directory before decompressing or writing.
    with zipfile.ZipFile(io.BytesIO(content)) as archive:
        members = archive.infolist()
        require(len(members) == 3 and {m.filename for m in members} == MEMBERS)
        require(sum(m.file_size for m in members) <= package.MAX_TOTAL)
        for member in members:
            mode = member.external_attr >> 16
            require(not member.is_dir() and stat.S_IFMT(mode) in (0, stat.S_IFREG)
                    and not member.flag_bits & 1 and member.file_size >= 0
                    and member.file_size <= (package.MAX_TOTAL if member.filename.endswith('.tar.gz') else 16384))
        values = {m.filename: archive.read(m) for m in members}
    receipt = json.loads(values['receipt.json'])
    manifest = json.loads(values['manifest.json'])
    require(receipt['archive_sha256'] == ARCHIVE_SHA and receipt['manifest_sha256'] == MANIFEST_SHA
            and receipt['workflow_run_id'] == RUN and package.canonical(receipt) == values['receipt.json']
            and package.canonical(manifest) == values['manifest.json'])
    require(hashlib.sha256(values['manifest.json']).hexdigest() == MANIFEST_SHA)
    output.mkdir(mode=0o700, parents=False, exist_ok=False)
    path = output / 'muninn-hotel-linux-x86_64.tar.gz'
    path.write_bytes(values[path.name])
    verified = package.verify(path, receipt)
    require(verified == manifest)
    return {'schema': 1, 'artifact_id': ARTIFACT, 'workflow_run_id': RUN, 'producing_head': HEAD,
            'candidate_source': package.CANDIDATE, 'official_zip_sha256': expected_zip_sha,
            'archive_sha256': ARCHIVE_SHA, 'manifest_sha256': MANIFEST_SHA,
            'components': receipt['components'], 'artifact_bytes_verified': True,
            'installed_guest_acceptance': False, 'systemd_acceptance': False, 'deployment_authorized': False}


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--zip', required=True)
    parser.add_argument('--run', required=True)
    parser.add_argument('--artifact', required=True)
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    provenance(json.loads(Path(args.run).read_bytes()), json.loads(Path(args.artifact).read_bytes()))
    path = Path(args.zip)
    require(path.absolute() == path.resolve() and path.is_file() and path.stat().st_size <= package.MAX_TOTAL)
    print(json.dumps(verify_zip(path.read_bytes(), args.output), sort_keys=True))
