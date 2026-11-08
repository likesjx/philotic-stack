#!/usr/bin/env python3
"""Disposable hosted Linux only; containers are network-isolated; pull uses registry."""
import json
import os
import pathlib
import re
import subprocess
import sys
import uuid
from retire_runtime import LABEL, retire_owned_runtime

IMAGE='ubuntu@sha256:2e863c44b718727c860746568e1d54afd13b2fa71b160f5cd9058fc436217b30'
ID=re.compile(r'[a-f0-9]{64}\Z')


def docker(args):
    return subprocess.run(['docker','--host=unix:///var/run/docker.sock',*args],
        stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,
        encoding='utf-8',errors='strict',timeout=30,check=False,
        env={'PATH':'/usr/bin:/bin','HOME':'/nonexistent'})


def call(args):
    result=docker(args)
    if result.returncode: raise RuntimeError('synthetic Docker action uncertain')
    return result.stdout.strip()


def inspect(ident, nonce):
    info=json.loads(call(['inspect',ident]))
    if type(info) is not list or len(info)!=1 or info[0].get('Id')!=ident or info[0].get('Config',{}).get('Labels',{}).get(LABEL)!=nonce:
        raise RuntimeError('synthetic container identity uncertain')
    return info[0]


def main():
    if not (sys.platform=='linux' and os.environ.get('GITHUB_ACTIONS')=='true'
            and os.environ.get('RUNNER_ENVIRONMENT')=='github-hosted'
            and pathlib.Path('/sys/fs/cgroup/cgroup.controllers').is_file()):
        raise RuntimeError('exclusive disposable Linux runner required')
    nonce=uuid.uuid4().hex
    owned={}; uncertain=False; outcome='inconclusive'; failures=[]; stage='pull'
    try:
        call(['pull',IMAGE])
        for role in ('mongo','runtime'):
            name='philotic-cgroup-observe-'+nonce+'-'+role
            # Record the name before creation; resolve uncertain create during cleanup.
            owned[role]={'name':name,'id':None}
            stage=role+'_create'
            ident=call(['create','--name',name,'--label',LABEL+'='+nonce,'--network','none',
                        '--restart=no','--cap-drop','ALL','--security-opt','no-new-privileges',
                        '--memory','64m','--pids-limit','16',IMAGE,'sleep','300'])
            if not ID.fullmatch(ident): raise RuntimeError('synthetic creation uncertain')
            owned[role]['id']=ident
            stage=role+'_inspect'
            item=inspect(ident,nonce)
            if item.get('Name')!='/'+name: raise RuntimeError('synthetic name uncertain')
            stage=role+'_start'
            call(['start',ident])
        stage='retire'
        try:
            retire_owned_runtime(docker,owned['runtime']['id'],owned['mongo']['id'],nonce)
            outcome='stopped_and_cgroup_empty'
        except RuntimeError as exc:
            if str(exc)=='synthetic cgroup state uncertain': outcome='cgroup_state_unavailable'
            elif str(exc)=='synthetic runtime cgroup remains populated': outcome='cgroup_still_populated'
            else: raise
    except BaseException:
        uncertain=True
        print('synthetic_failure_stage='+stage)
    finally:
        for item in reversed(list(owned.values())):
            try:
                # Reconcile only exact unique name and label, including unknown creation.
                ids=call(['ps','-aq','--no-trunc','--filter','name=^/'+item['name']+'$', '--filter','label='+LABEL+'='+nonce]).splitlines()
                if len(ids)>1 or any(not ID.fullmatch(i) for i in ids): raise RuntimeError('inventory uncertain')
                if item['id'] is not None and ids != [item['id']]: raise RuntimeError('owned identity missing')
                if ids:
                    value=inspect(ids[0],nonce)
                    if value.get('Name')!='/'+item['name']: raise RuntimeError('name mismatch')
                    call(['rm','--force',ids[0]])
                after=call(['ps','-aq','--no-trunc','--filter','name=^/'+item['name']+'$']).splitlines()
                if after: raise RuntimeError('removal uncertain')
            except BaseException:
                failures.append('cleanup_uncertain')
    if failures:
        print('synthetic_cleanup_failure=owned_container')
    print('synthetic_cgroup_observation='+outcome)
    print('synthetic_container_cleanup='+('uncertain' if failures or uncertain else 'confirmed'))
    if uncertain or failures or outcome!='stopped_and_cgroup_empty': raise SystemExit(1)

if __name__=='__main__': main()
