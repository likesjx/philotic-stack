#!/usr/bin/python
"""Fixed approval/checkpoint boundary; all service actions stay in the role."""
import hashlib
import importlib
import inspect
from ansible.module_utils import percival_memory_contract as c
from ansible.module_utils.basic import AnsibleModule
from ansible.module_utils.percival_memory_host import Host


def main():
    module=AnsibleModule(argument_spec={
        'action':{'type':'str','choices':['prepare','checkpoint','recover','mutate'],'required':True},
        'manifest':{'type':'dict','required':True},'qualification':{'type':'dict','required':True},
        'approved_sha256':{'type':'str','required':True},
        'mode':{'type':'str','choices':['activate','recover'],'default':'activate'},
        'step':{'type':'str','default':''},'status':{'type':'str','choices':['pending','done'],'default':'pending'},
        'operator_sha256':{'type':'dict','required':True},
        'fixture_root':{'type':'str','default':''}},supports_check_mode=False)
    p=module.params
    try:
        c.require(p['operator_sha256']==p['manifest']['operator_sha256'])
        for name in c.OPERATOR_FILES:
            if name.startswith('ansible/module_utils/'):
                mod=importlib.import_module('ansible.module_utils.'+name.rsplit('/',1)[1][:-3])
                c.require(hashlib.sha256(inspect.getsource(mod).encode()).hexdigest()==p['operator_sha256'][name])
            elif name=='ansible/library/percival_memory_checkpoint.py':
                source=__loader__.get_source('ansible.modules.percival_memory_checkpoint')
                c.require(hashlib.sha256(source.encode()).hexdigest()==p['operator_sha256'][name])
        host=Host(p['fixture_root'])
        args=(p['manifest'],p['qualification'],p['approved_sha256'])
        if p['action']=='prepare': result=host.prepare(*args)
        elif p['action']=='mutate': result=host.mutation(*args,p['mode'],p['step'])
        else:
            with host.locked():
                if p['action']=='recover':result=host.recover(*args)
                else:result=host.checkpoint(*args,p['mode'],p['step'],p['status'])
    except Exception:
        module.fail_json(msg='Memory checkpoint refused. Preserve public transaction records; no automatic retry or rollback is authorized.')
    module.exit_json(changed=True,**result)


if __name__=='__main__': main()
