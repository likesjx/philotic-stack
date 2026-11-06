"""Disabled candidate for exact, disposable CI container termination.

This module has no executable entry point. It is not a production tool.
It assumes an exclusive trusted Linux GitHub runner and its local Docker
daemon. It does not establish whether a remote mutation committed, prove
host-native systemd behavior, or remove containers, volumes, or networks.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import time

ROLES = frozenset({'runtime', 'mongo', 'graph', 'anchor', 'website', 'proxy'})
ID = re.compile(r'^[0-9a-f]{64}$')
NONCE = re.compile(r'^[0-9a-f]{32}$')


def _docker(args):
    if os.environ.get('CI') != 'true' or os.environ.get('GITHUB_ACTIONS') != 'true':
        raise RuntimeError('disposable GitHub runner required')
    return subprocess.run(
        ['docker', '--host=unix:///var/run/docker.sock', *args],
        capture_output=True, text=True, timeout=15, check=False,
        env={'PATH': '/usr/bin:/bin', 'HOME': '/nonexistent'},
    )


def _inspect(runner, ident):
    result = runner(['inspect', ident])
    if result.returncode != 0:
        raise RuntimeError('container inspection unavailable')
    try:
        data = json.loads(result.stdout)
        if not isinstance(data, list) or len(data) != 1:
            raise ValueError('unexpected inspect result')
        return data[0]
    except (ValueError, TypeError, KeyError):
        raise RuntimeError('container inspection invalid') from None


def _validate(info, role, ident, image, nonce, scratch, expected_network):
    try:
        config, host, state = info['Config'], info['HostConfig'], info['State']
        if (info['Id'] != ident or info['Image'] != image or info['Name'].lstrip('/') != f'native-qualification-{nonce}-{role}'
                or config['Labels']['com.philotic.fixture'] != nonce
                or host['Privileged'] is not False or host['PidMode'] not in ('', 'private')
                or host.get('PortBindings') not in ({}, None) or host.get('PublishAllPorts') is not False
                or 'ALL' not in (host.get('CapDrop') or [])
                or host.get('CapAdd') not in (None, []) or host.get('Devices') not in (None, [])
                or host.get('RestartPolicy', {}).get('Name') != 'no'
                or host.get('RestartPolicy', {}).get('MaximumRetryCount') != 0
                or not any(x == 'no-new-privileges' or x == 'no-new-privileges:true'
                           for x in (host.get('SecurityOpt') or []))
                or any('unconfined' in x for x in (host.get('SecurityOpt') or []))
                or host.get('NetworkMode') not in expected_network
                or state.get('Restarting') is not False or state.get('Dead') is not False
                or not isinstance(state['Running'], bool) or not isinstance(state['Pid'], int)):
            raise ValueError('unsafe container')
        if state['Running'] != (state['Pid'] > 0):
            raise ValueError('contradictory process state')
        for mount in info['Mounts']:
            if mount['Type'] == 'tmpfs':
                continue
            if mount['Type'] != 'bind':
                raise ValueError('unverified volume')
            source = Path(mount['Source']).resolve(strict=True)
            if not source.is_relative_to(scratch):
                raise ValueError('external bind mount')
    except (AttributeError, KeyError, TypeError, ValueError, OSError):
        raise RuntimeError('container identity or isolation unverified') from None
    return state['Running']


def terminate_fixture(manifest, scratch_root, runner_temp, runner=_docker):
    """Stop and independently inspect the exact complete fixture set.

    Caller must first preserve independent mutation/journal evidence and own
    the disposable VM. A failed or ambiguous stop is recorded, not retried.
    Docker's reported stop is interpreted under the private PID-namespace and
    trusted-daemon assumptions; it is not independent cgroup-empty evidence.
    """
    if not isinstance(manifest, dict) or set(manifest) != {'nonce', 'containers', 'images', 'network_id'}:
        raise ValueError('invalid fixture manifest')
    nonce, containers, images, network_id = manifest['nonce'], manifest['containers'], manifest['images'], manifest['network_id']
    if (not isinstance(nonce, str) or not NONCE.fullmatch(nonce)
            or not isinstance(containers, dict) or set(containers) != ROLES
            or any(not isinstance(v, str) or not ID.fullmatch(v) for v in containers.values())
            or len(set(containers.values())) != len(ROLES)
            or not isinstance(images, dict) or set(images) != ROLES
            or any(not isinstance(v, str) or not re.fullmatch(r'sha256:[0-9a-f]{64}', v) for v in images.values())
            or not isinstance(network_id, str) or not ID.fullmatch(network_id)):
        raise ValueError('invalid fixture identity set')
    scratch = Path(scratch_root).resolve(strict=True)
    runner_dir = Path(runner_temp).resolve(strict=True)
    if runner is _docker:
        actual_runner_temp = os.environ.get('RUNNER_TEMP')
        if not actual_runner_temp or Path(actual_runner_temp).resolve(strict=True) != runner_dir:
            raise ValueError('runner scratch does not match environment')
    if (not scratch.is_dir() or not runner_dir.is_dir()
            or scratch.parent != runner_dir or scratch.name != f'native-qualification-{nonce}'):
        raise ValueError('fixture scratch unavailable')

    network_name = f'native-qualification-{nonce}-network'
    listed = runner(['ps', '-aq', '--no-trunc'])
    if listed.returncode != 0 or set(listed.stdout.split()) != set(containers.values()):
        raise RuntimeError('disposable daemon container inventory unverified')
    network_result = runner(['network', 'inspect', network_id])
    try:
        networks = json.loads(network_result.stdout) if network_result.returncode == 0 else []
        network = networks[0] if len(networks) == 1 else {}
        if (network.get('Id') != network_id or network.get('Name') != network_name
                or network.get('Internal') is not True
                or network.get('Labels', {}).get('com.philotic.fixture') != nonce
                or set(network.get('Containers', {})) != {containers['anchor'], containers['proxy']}):
            raise ValueError('network')
    except (ValueError, TypeError, AttributeError):
        raise RuntimeError('fixture network unverified') from None

    # Validate the complete declared set before the first stop. The caller
    # must independently establish that this manifest covers every client.
    running = {}
    for role, ident in containers.items():
        expected_network = {network_name, network_id} if role in ('anchor', 'proxy') else {f'container:{containers["anchor"]}'}
        running[role] = _validate(_inspect(runner, ident), role, ident, images[role], nonce, scratch, expected_network)

    outcomes = {}
    for role in ('runtime', 'website', 'proxy', 'mongo', 'graph', 'anchor'):
        ident = containers[role]
        stop_ok = True
        if running[role]:
            try:
                stop_ok = runner(['kill', '--signal', 'KILL', ident]).returncode == 0
            except (OSError, subprocess.TimeoutExpired):
                stop_ok = False
        if stop_ok:
            try:
                waited = runner(['wait', ident])
                stop_ok = waited.returncode == 0 and waited.stdout.strip().isdigit()
            except (OSError, subprocess.TimeoutExpired):
                stop_ok = False
        try:
            info = _inspect(runner, ident)
            expected_network = {network_name, network_id} if role in ('anchor', 'proxy') else {f'container:{containers["anchor"]}'}
            stopped = not _validate(info, role, ident, images[role], nonce, scratch, expected_network)
            if stopped:
                time.sleep(0.1)
                stopped = not _validate(_inspect(runner, ident), role, ident, images[role], nonce, scratch, expected_network)
        except (RuntimeError, OSError, subprocess.TimeoutExpired):
            stopped = False
        outcomes[role] = bool(stop_ok and stopped)
    # Recheck the full set after the last stop; a late restart or new daemon
    # container invalidates the whole observation. This is still a bounded
    # observation, not a persistent activation lock.
    try:
        listed = runner(['ps', '-aq', '--no-trunc'])
        if listed.returncode != 0 or set(listed.stdout.split()) != set(containers.values()):
            return {role: False for role in ROLES}
        for role, ident in containers.items():
            expected_network = {network_name, network_id} if role in ('anchor', 'proxy') else {f'container:{containers["anchor"]}'}
            if _validate(_inspect(runner, ident), role, ident, images[role], nonce, scratch, expected_network):
                outcomes[role] = False
    except (RuntimeError, OSError, subprocess.TimeoutExpired):
        return {role: False for role in ROLES}
    return outcomes


def all_observed_stopped(outcomes):
    """A complete success requires all six explicit true observations."""
    return isinstance(outcomes, dict) and set(outcomes) == ROLES and all(v is True for v in outcomes.values())


if __name__ == '__main__':
    raise SystemExit('Candidate disabled: no standalone execution route')
