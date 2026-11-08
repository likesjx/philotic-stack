"""Synthetic Docker fixture helper; caller must use the reviewed hosted runner.

This checks one already-owned runtime and the retained Mongo before and after a
stop. It does not create containers, prove cross-container isolation, or resume
an application. Unknown Docker results fail closed.
"""
import json
import re

from runtime_cgroup import capture

ID = re.compile(r'[0-9a-f]{64}\Z')
NONCE = re.compile(r'[0-9a-f]{32}\Z')
LABEL = 'com.philotic.synthetic-cold-start'


def _inspect(docker, ident, nonce, running):
    result = docker(['inspect', ident])
    if result.returncode != 0:
        raise RuntimeError('synthetic container inspection uncertain')
    try:
        value = json.loads(result.stdout)
        if type(value) is not list or len(value) != 1 or type(value[0]) is not dict:
            raise ValueError()
        item = value[0]
        state = item['State']
        labels = item['Config']['Labels']
        if (item['Id'] != ident or labels.get(LABEL) != nonce
                or item['HostConfig']['RestartPolicy'] != {'Name':'no','MaximumRetryCount':0}
                or state['Running'] is not running or state['Dead'] is not False
                or item['RestartCount'] != 0):
            raise ValueError()
        if running and (type(state['Pid']) is not int or state['Pid'] <= 1):
            raise ValueError()
        if not running and (type(state['Pid']) is not int or state['Pid'] != 0):
            raise ValueError()
        return state['Pid']
    except (TypeError, ValueError, KeyError, json.JSONDecodeError) as exc:
        raise RuntimeError('synthetic container identity or state uncertain') from exc


def retire_owned_runtime(docker, runtime, mongo, nonce, capture_cgroup=capture):
    if (not isinstance(runtime,str) or not ID.fullmatch(runtime)
            or not isinstance(mongo,str) or not ID.fullmatch(mongo) or mongo == runtime
            or not isinstance(nonce,str) or not NONCE.fullmatch(nonce)):
        raise ValueError('invalid synthetic resource identity')
    mongo_pid = _inspect(docker,mongo,nonce,True)
    runtime_pid = _inspect(docker,runtime,nonce,True)
    if runtime_pid == mongo_pid:
        raise RuntimeError('synthetic container process identity uncertain')
    group = capture_cgroup(runtime, runtime_pid)
    try:
        if _inspect(docker,runtime,nonce,True) != runtime_pid:
            raise RuntimeError('synthetic runtime incarnation changed')
        result = docker(['stop','--time','10',runtime])
        if result.returncode != 0 or result.stdout.strip() != runtime:
            raise RuntimeError('synthetic runtime stop uncertain')
        _inspect(docker,runtime,nonce,False)
        if group.populated() != 0:
            raise RuntimeError('synthetic runtime cgroup remains populated')
        if _inspect(docker,mongo,nonce,True) != mongo_pid:
            raise RuntimeError('synthetic Mongo incarnation changed')
    finally:
        group.close()
