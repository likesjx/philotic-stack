"""Disabled, read-only preflight for a disposable CI fixture.

No standalone execution route. The caller must independently pin and review
the contract; a contract made from the observations it checks is not proof.
The trusted, exclusive runner and host-client assumptions remain external.
"""
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat

import native_container_boundary_candidate as boundary


def _json_one(runner, args):
    result = runner(args)
    if result.returncode != 0:
        raise RuntimeError('preflight inspection unavailable')
    try:
        value = json.loads(result.stdout)
        if not isinstance(value, list) or len(value) != 1 or not isinstance(value[0], dict):
            raise ValueError('invalid inspection')
        return value[0]
    except (TypeError, ValueError):
        raise RuntimeError('preflight inspection invalid') from None


def _artifact_hash(root, relative):
    if not isinstance(relative, str) or not relative or PurePosixPath(relative).is_absolute() or '..' in PurePosixPath(relative).parts:
        raise ValueError('invalid artifact path')
    path = root.joinpath(relative)
    if path.is_symlink() or not path.resolve(strict=True).is_relative_to(root):
        raise RuntimeError('artifact path unverified')
    # CI fixtures are exclusively owned. This detects ordinary replacement,
    # not a hostile runner administrator or in-place concurrent modification.
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), 'rb') as stream:
        first = os.fstat(stream.fileno())
        if not stat.S_ISREG(first.st_mode) or first.st_size > 536870912:
            raise RuntimeError('fixture artifact is not a bounded regular file')
        hasher = hashlib.sha256()
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            hasher.update(chunk)
        digest = hasher.hexdigest()
        last = os.fstat(stream.fileno())
    current = path.stat(follow_symlinks=False)
    if ((first.st_dev, first.st_ino, first.st_size, first.st_mtime_ns) != (last.st_dev, last.st_ino, last.st_size, last.st_mtime_ns)
            or (first.st_dev, first.st_ino) != (current.st_dev, current.st_ino)):
        raise RuntimeError('artifact changed while reading')
    return digest


def preflight_fixture(manifest, contract, scratch_root, runner_temp, runner=boundary._docker):
    """Inspect created, never-started synthetic resources without changing them.

    Image/command/mount expectations are separately supplied by reviewed CI
    policy, not derived from Docker. No output includes environment values.
    """
    if (not isinstance(manifest, dict) or set(manifest) != {'nonce', 'containers', 'network_id'}
            or not isinstance(contract, dict) or set(contract) != {'images', 'roles', 'artifacts'}):
        raise ValueError('invalid preflight contract')
    nonce, containers, network_id = manifest['nonce'], manifest['containers'], manifest['network_id']
    if (not isinstance(nonce, str) or not boundary.NONCE.fullmatch(nonce)
            or not isinstance(containers, dict) or set(containers) != boundary.ROLES
            or any(not isinstance(x, str) or not boundary.ID.fullmatch(x) for x in containers.values())
            or len(set(containers.values())) != len(boundary.ROLES)
            or not isinstance(network_id, str) or not boundary.ID.fullmatch(network_id)
            or not isinstance(contract['images'], dict) or set(contract['images']) != boundary.ROLES
            or not isinstance(contract['roles'], dict) or set(contract['roles']) != boundary.ROLES
            or not isinstance(contract['artifacts'], dict) or not contract['artifacts']):
        raise ValueError('invalid preflight identities')
    root, parent = Path(scratch_root).resolve(strict=True), Path(runner_temp).resolve(strict=True)
    if not root.is_dir() or root.parent != parent or root.name != f'native-qualification-{nonce}':
        raise ValueError('fixture scratch unavailable')
    if runner is boundary._docker:
        if not os.environ.get('RUNNER_TEMP') or Path(os.environ['RUNNER_TEMP']).resolve(strict=True) != parent:
            raise ValueError('runner scratch does not match environment')
    for name, digest in contract['artifacts'].items():
        if not isinstance(digest, str) or not re.fullmatch(r'[0-9a-f]{64}', digest) or _artifact_hash(root, name) != digest:
            raise RuntimeError('fixture artifact identity mismatch')

    listed = runner(['ps', '-aq', '--no-trunc'])
    if listed.returncode != 0 or set(listed.stdout.split()) != set(containers.values()):
        raise RuntimeError('container inventory unverified')
    network = _json_one(runner, ['network', 'inspect', network_id])
    network_name = f'native-qualification-{nonce}-network'
    if (network.get('Id') != network_id or network.get('Name') != network_name
            or network.get('Internal') is not True or network.get('Driver') != 'bridge'
            or network.get('Scope') != 'local' or network.get('Labels', {}).get('com.philotic.fixture') != nonce
            or network.get('Containers') != {}):
        raise RuntimeError('network not isolated and empty')

    observed_volumes = set()
    for role in sorted(boundary.ROLES):
        image, expected = contract['images'][role], contract['roles'][role]
        if (not isinstance(image, str) or not re.fullmatch(r'sha256:[0-9a-f]{64}', image)
                or not isinstance(expected, dict)
                or set(expected) != {'declared_volumes', 'mounts', 'tmpfs', 'entrypoint', 'command', 'env_sha256'}):
            raise ValueError('invalid role contract')
        image_info = _json_one(runner, ['image', 'inspect', image])
        declared = (image_info.get('Config') or {}).get('Volumes') or {}
        if image_info.get('Id') != image or not isinstance(declared, dict) or sorted(declared) != sorted(expected['declared_volumes']):
            raise RuntimeError('image or declared volume mismatch')
        info = boundary._inspect(runner, containers[role])
        settings = info.get('NetworkSettings')
        attachments = settings.get('Networks') if isinstance(settings, dict) else None
        expected_attachments = {network_name} if role in ('anchor', 'proxy') else set()
        if (not isinstance(attachments, dict) or set(attachments) != expected_attachments
                or any(not isinstance(value, dict) or value.get('NetworkID') != network_id
                       for value in attachments.values())):
            raise RuntimeError('fixture network attachments unverified')
        mounts = info.get('Mounts')
        if not isinstance(mounts, list) or not isinstance(expected['mounts'], list) or not isinstance(expected['tmpfs'], dict):
            raise ValueError('invalid mount contract')
        # Preserve existing isolation checks while separately verifying exact
        # named volumes below. No unapproved volume is silently accepted.
        no_volumes = dict(info)
        no_volumes['Mounts'] = [m for m in mounts if isinstance(m, dict) and m.get('Type') != 'volume']
        expected_net = {network_name, network_id} if role in ('anchor', 'proxy') else {f'container:{containers["anchor"]}'}
        if boundary._validate(no_volumes, role, containers[role], image, nonce, root, expected_net):
            raise RuntimeError('fixture started before preflight')
        state, host, config = info['State'], info['HostConfig'], info['Config']
        if (state.get('Status') != 'created' or state.get('StartedAt') != '0001-01-01T00:00:00Z'
                or host.get('PublishAllPorts') is not False
                or info.get('RestartCount') != 0 or (host.get('Tmpfs') or {}) != expected['tmpfs']
                or config.get('Entrypoint') != expected['entrypoint'] or config.get('Cmd') != expected['command']
                or not isinstance(config.get('Env'), list)):
            raise RuntimeError('fixture creation contract mismatch')
        env_hash = hashlib.sha256(json.dumps(config['Env'], ensure_ascii=False, separators=(',', ':')).encode()).hexdigest()
        if not isinstance(expected['env_sha256'], str) or not re.fullmatch(r'[0-9a-f]{64}', expected['env_sha256']) or env_hash != expected['env_sha256']:
            raise RuntimeError('fixture environment mismatch')
        actual_mounts = []
        for mount in mounts:
            if not isinstance(mount, dict) or mount.get('Type') not in ('bind', 'volume', 'tmpfs'):
                raise RuntimeError('unknown fixture mount')
            kind = mount['Type']
            if kind == 'bind' and Path(mount['Source']).is_socket():
                raise RuntimeError('socket bind is forbidden')
            source = (str(Path(mount['Source']).resolve(strict=True)) if kind == 'bind'
                      else mount.get('Name') if kind == 'volume' else '')
            item = {'type': kind, 'source': source, 'destination': mount.get('Destination'),
                    'rw': mount.get('RW'), 'propagation': mount.get('Propagation')}
            if not isinstance(item['source'], str) or not isinstance(item['destination'], str) or not isinstance(item['rw'], bool):
                raise RuntimeError('fixture mount metadata invalid')
            actual_mounts.append(item)
            if kind == 'volume':
                if not re.fullmatch(rf'native-qualification-{nonce}-[a-z0-9-]+', source) or source in observed_volumes:
                    raise RuntimeError('unknown or reused fixture volume')
                volume = _json_one(runner, ['volume', 'inspect', source])
                if (volume.get('Name') != source or volume.get('Driver') != 'local' or volume.get('Scope') != 'local'
                        or volume.get('Labels', {}).get('com.philotic.fixture') != nonce or volume.get('Options') not in ({}, None)):
                    raise RuntimeError('fixture volume unverified')
                observed_volumes.add(source)
        key = lambda item: json.dumps(item, sort_keys=True, separators=(',', ':'))
        if sorted(map(key, actual_mounts)) != sorted(map(key, expected['mounts'])):
            raise RuntimeError('fixture mount contract mismatch')
        destinations = {m['destination'] for m in actual_mounts} | set(expected['tmpfs'])
        if not set(declared).issubset(destinations):
            raise RuntimeError('image volume destination not explicitly contained')
    volume_list = runner(['volume', 'ls', '-q'])
    if volume_list.returncode != 0 or set(volume_list.stdout.split()) != observed_volumes:
        raise RuntimeError('volume inventory unverified')
    # The separately reviewed stop candidate rejects every volume. Do not
    # declare this composition eligible until owned-volume cleanup is proven.
    if observed_volumes:
        raise RuntimeError('fixture volume teardown not qualified')
    return {'containers': len(containers), 'volumes': len(observed_volumes), 'state': 'created-not-started'}


if __name__ == '__main__':
    raise SystemExit('Candidate disabled: no standalone execution route')
