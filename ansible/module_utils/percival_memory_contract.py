"""Closed public approval contract for preserving the installed memory broker.

No I/O on import. Owner attestations are not live checks and never contain keys.
"""
import hashlib
import json
import re
import time

HOTEL = 'b7d0c9703b3670efc5b93cfc8cb2561b3f80fe7d0d94be65c400987751eb7a17'
GATEWAY = '709c0d7e3de6e85df6ba45786fbd7537123e4011'
BASE = '/opt/percival-personal-mcp'
POLICY = '/etc/percival-personal-mcp/scoped-vault-policy.json'
DISCOVERY = '/etc/percival-personal-mcp/gateway-discovery.json'
OVERRIDE = '/etc/systemd/system/percival-personal-mcp.service.d/99-muninn-memory.conf'
GATES = {'issuer_pair_verified', 'native_scope_and_expiry_verified', 'admissions_disabled',
         'default_vault_private_future_read_only_approved', 'shared_hotel_restart_approved',
         'gateway_restart_approved', 'policy_adoption_approved', 'operators_quiescent'}


def require(ok):
    if not ok:
        raise ValueError('memory contract refused')


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=True).encode()


def digest(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def exact(value, keys):
    require(type(value) is dict and set(value) == set(keys.split()))


def hex64(value):
    return type(value) is str and re.fullmatch('[a-f0-9]{64}', value) is not None


def policy(value):
    exact(value, 'caller_uid hotel_uid secret_ref introspection_secret_ref')
    require(type(value['caller_uid']) is int and value['caller_uid'] == 1003)
    require(type(value['hotel_uid']) is int and value['hotel_uid'] == 999)
    for key, kind in [('secret_ref', 'percival-muninn-observe'), ('introspection_secret_ref', 'percival-issuer-introspection')]:
        require(type(value[key]) is str and re.fullmatch('secret://hotel/default/' + kind + '/[A-Za-z0-9_-]{1,128}', value[key]))
    return value


def validate(manifest, qualification, approved_sha, now=None, recovery=False):
    now = int(time.time()) if now is None else now
    exact(manifest, 'schema kind transaction approved_at expires_at config_sha256 hotel_sha256 gateway_source qualification_sha256 baseline policy issuer_attestation native_key_attestation owner_attestations operator_sha256 recovery')
    require(type(manifest['schema']) is int and manifest['schema'] == 1 and manifest['kind'] == 'existing-hotel-memory-activation')
    require(hex64(approved_sha) and digest(manifest) == approved_sha)
    require(type(manifest['transaction']) is str and re.fullmatch('[a-f0-9]{32}', manifest['transaction']))
    require(type(manifest['approved_at']) is int and type(manifest['expires_at']) is int and
            manifest['approved_at'] <= now < manifest['expires_at'] <= manifest['approved_at'] + 1800)
    require(hex64(manifest['config_sha256']) and manifest['hotel_sha256'] == HOTEL and manifest['gateway_source'] == GATEWAY)
    exact(qualification, 'schema kind binary_sha256 source_revision artifact_sha256 source_artifact_zip_sha256 source_run_id source_repository fixture_sha256 passed production_selection')
    require(type(qualification['schema']) is int and qualification['schema'] == 1 and qualification['kind'] == 'same-binary-scoped-broker-qualification')
    require(qualification['binary_sha256'] == HOTEL and hex64(qualification['artifact_sha256']) and
            qualification['artifact_sha256'] == HOTEL and hex64(qualification['fixture_sha256']))
    require(type(qualification['source_revision']) is str and re.fullmatch('[a-f0-9]{40}', qualification['source_revision']))
    require(hex64(qualification['source_artifact_zip_sha256']) and type(qualification['source_run_id']) is int and qualification['source_run_id'] > 0 and qualification['source_repository'] == 'likesjx/philotic-stack')
    require(qualification['passed'] is True and qualification['production_selection'] is False)
    require(digest(qualification) == manifest['qualification_sha256'])
    exact(manifest['baseline'], 'gateway_unit_sha256 discovery_sha256 policy_sha256 supervisor_sha256 discovery_node_sha256 application_override_sha256')
    require(all(hex64(v) for v in manifest['baseline'].values()))
    require(manifest['baseline']['discovery_node_sha256']=='81925c0995b5c1427b5d538e6a90ca2fdc4daffb786b09af749beaf7369d4e90')
    policy(manifest['policy'])
    exact(manifest['issuer_attestation'], 'revision image authority_mode coordination_mode')
    require(manifest['issuer_attestation']=={'revision':'2031a9cff6609ed77e961868b976ba8c7bcf87c6','image':'sha256:9c218f8fd65bb2a888976658edf3aded3ab0978f5435fe93e08740e1bb1dcf1f','authority_mode':'absent','coordination_mode':'absent'})
    key=manifest['native_key_attestation']
    exact(key, 'vault scope expires_at verified_at revoked policy_reference_match')
    require(key['vault']=='default' and key['scope']=='observe' and key['revoked'] is False and key['policy_reference_match'] is True)
    require(type(key['expires_at']) is int and key['expires_at']>0 and type(key['verified_at']) is int and key['verified_at']>=0)
    if not recovery:
        require(key['verified_at']<=manifest['approved_at'] and manifest['approved_at']-key['verified_at']<1800 and manifest['expires_at']<=key['expires_at'])
    exact(manifest['owner_attestations'], ' '.join(GATES))
    require(all(v is True for v in manifest['owner_attestations'].values()))
    exact(manifest['operator_sha256'], ' '.join(OPERATOR_FILES))
    require(all(hex64(v) for v in manifest['operator_sha256'].values()))
    if recovery:
        exact(manifest['recovery'], 'activation_sha256 commands_quiescent previous_record_sha256 previous_approval_sha256')
        require(hex64(manifest['recovery']['previous_record_sha256']) and (manifest['recovery']['previous_approval_sha256'] is None or hex64(manifest['recovery']['previous_approval_sha256'])))
        require(hex64(manifest['recovery']['activation_sha256']) and manifest['recovery']['commands_quiescent'] is True)
    else:
        require(manifest['recovery'] is None)
    stage = BASE + '/staged/memory-v1-' + manifest['config_sha256']
    override = ('# Owned Muninn transaction ' + manifest['transaction'] + '\n[Unit]\nRequires=philotic-hotel.service percival-personal-vault.socket\nAfter=philotic-hotel.service percival-personal-vault.socket\n[Service]\nWorkingDirectory=' + stage + '/gateway\nExecStart=\nExecStart=' + stage + '/bin/node ' + stage + '/gateway/server.mjs ' + stage + '/gateway.json\n')
    return {'stage': stage, 'override': override, 'policy': canonical(manifest['policy']).decode() + '\n',
            'manifest_sha256': approved_sha, 'qualification_sha256': manifest['qualification_sha256']}

FILES = {"bin/node": "7fde7b8afa198da66257f42ee2001d874c7355631e6d1579a5fb5ef1f246df4c", "gateway/gateway.mjs": "307fc9ac422d575579f9b9e66f221e29d40592c7dbf9b4d0fed5544d0eb86259", "gateway/lifegraph-catalog.mjs": "579b3e7f9dcbbda4cec2b212821830811799786c615be67823850a616a3da0ad", "gateway/lifegraph-gateway.mjs": "d3ba769bee254bb0ff4f5088d1a3a465897af20a3bb646ee36c35a7791fa53b6", "gateway/lifegraph-identity.mjs": "0ffce4263fffa14a9d3354e11517ad5a8671c41e215160652ff61088bbdd3bb0", "gateway/lifegraph-memgraph.mjs": "bb8e4362279150ce83424655f0bcb51c608120cb32b84e1edbebf71ab5223e8c", "gateway/lifegraph-owned-storage.mjs": "ce79d24e9f5c5867eb4213224a3c76d2f94b0c5b28a5f2dd0c5747bf9b2719a5", "gateway/lifegraph-owner-transport.mjs": "630a98be90b8991f11eea60b0cc6db16542edb4c4634bebaf6556b00dd569e35", "gateway/lifegraph-policy-owner.mjs": "1e345e04d5fc2949b379875e459809b4a302ed0c2a1547f168d2e60f129f57b8", "gateway/lifegraph-production.mjs": "c8db6973ba654429ea344181a7a6e1ee8e1b7798907778526ccd1318be51e1d2", "gateway/lifegraph-release.mjs": "1b98d0d9a7da8304a236693130ed65ddb0f8d5e7172de6a62a95cd64f8f68c2f", "gateway/lifegraph-storage.mjs": "fec79d7604eab753178183fbc28d3840493338a2a69d67725499fe95892b0928", "gateway/lifegraph.mjs": "01dbd596f4d2bce41b62dbd6e34ee9a471bbc5edaecbf2cd40f5ad17a770c577", "gateway/package.json": "9866d92cb6f04ba3ba4e6c22993fc469ab432151b5ed889f43708ab05a792193", "gateway/server.mjs": "d6fb41b6c4626da12ac4ff46e6bb0a02e4b383d294ba2f6356ff3cbc8ae32d79", "gateway/vault.mjs": "13469581477a55729ce34273ad60e07ab9f2c2549f7379ea91bbc1a6e749df27", "gateway/whole-vault.mjs": "ed44b97ea73999c6bfce013444fdb50675c0ce5ef91741082243c54dc2a0427e"}


def memory_configuration(value):
    subject='15393f50-045b-4227-90b0-0cf4264f0b3d'
    clients=['claude_muninn_recall_v1','chatgpt_muninn_recall_v1']
    socket='/run/percival-personal-vault-broker.sock'
    expected={'resource':'https://mcp.jaredlikes.com/personal/mcp','port':8913,'muninnVault':'default',
        'enabledTools':['muninn_recall'],'allowedSubjects':[subject],'allowedClients':clients,
        'wholeVaultAccess':{'version':1,'subject':subject,'vault':'default','includeFutureRecords':True},
        'clientPolicies':[{'clientId':c,'subjects':[subject],'vault':'default','scopes':['memory:recall']} for c in clients],
        'issuer':{'issuer':'https://www.jaredlikes.com','discovery':'https://www.jaredlikes.com/.well-known/oauth-authorization-server',
            'introspection':'https://www.jaredlikes.com/oauth/introspect','clientId':'percival_gateway_persistent_recall_v1','credentialBrokerSocketPath':socket},
        'upstreams':{'muninn_recall':{'url':'http://127.0.0.1:8750/mcp','backendProtocol':'muninn-unary-json','credentialBrokerSocketPath':socket}}}
    require(canonical(value)==canonical(expected))
    return value

OPERATOR_FILES = ('justfile', 'ansible/deploy_personal_mcp.yml', 'ansible/module_utils/percival_memory_contract.py', 'ansible/module_utils/percival_memory_journal.py', 'ansible/module_utils/percival_memory_host.py', 'ansible/module_utils/percival_provenance.py', 'ansible/library/percival_memory_checkpoint.py', 'ansible/roles/percival_personal_mcp/tasks/main.yml', 'ansible/roles/percival_personal_mcp/tasks/memory_activate.yml', 'ansible/roles/percival_personal_mcp/tasks/memory_step.yml', 'ansible/roles/percival_personal_mcp/defaults/main.yml')


def validate_vars(value, mode, root, now=None, approval_sha=None):
    """Validate public outer variables before any Ansible/Jinja evaluation."""
    from pathlib import Path
    if mode=='stage':
        exact(value, 'percival_memory_package_dir percival_memory_config_file percival_memory_config_sha256')
        for name in ('percival_memory_package_dir','percival_memory_config_file'):
            path=value[name]
            require(isinstance(path,str) and len(path)<=4096 and re.fullmatch(r'/[A-Za-z0-9_./-]+',path) is not None)
            require(all(part not in ('','.', '..') for part in path.split('/')[1:]))
        require(hex64(value['percival_memory_config_sha256']))
        require(approval_sha is None)
        return {'valid':True}
    exact(value, 'percival_memory_manifest percival_memory_qualification percival_memory_approval_sha256')
    if approval_sha is not None:
        require(hex64(approval_sha) and approval_sha==value['percival_memory_approval_sha256'])
    require(mode in ('activate', 'recover'))
    manifest=value['percival_memory_manifest']
    result=validate(manifest, value['percival_memory_qualification'], value['percival_memory_approval_sha256'], now, recovery=mode=='recover')
    root=Path(root)
    actual={name: hashlib.sha256((root/name).read_bytes()).hexdigest() for name in OPERATOR_FILES}
    require(actual==manifest['operator_sha256'])
    return result


def _vars_cli():
    import argparse
    import os
    from pathlib import Path
    import stat
    parser=argparse.ArgumentParser(description='Validate public memory approval variables locally; no remote operation.')
    parser.add_argument('--validate-vars', required=True)
    parser.add_argument('--mode', choices=('activate','recover','stage'), required=True)
    parser.add_argument('--approval-sha')
    parser.add_argument('--emit-vars', action='store_true', help='Emit only the validated canonical public variable snapshot.')
    args=parser.parse_args()
    def closed_pairs(pairs):
        result={}
        for key,value in pairs:
            require(key not in result)
            result[key]=value
        return result
    try:
        fd=os.open(args.validate_vars, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            info=os.fstat(fd)
            require(stat.S_ISREG(info.st_mode) and info.st_size<=131072)
            raw=os.read(fd,131073)
            require(len(raw)<=131072)
        finally:
            os.close(fd)
        value=json.loads(raw, object_pairs_hook=closed_pairs)
        require(args.mode=='stage' or hex64(args.approval_sha))
        validate_vars(value,args.mode,Path(__file__).resolve().parents[2],approval_sha=args.approval_sha)
        print(canonical(value).decode() if args.emit_vars else '{"valid":true}')
        return 0
    except (ValueError,TypeError,KeyError,OSError,UnicodeError):
        print('{"valid":false,"reason":"public_approval_refused"}')
        return 2


if __name__ == '__main__':
    raise SystemExit(_vars_cli())
