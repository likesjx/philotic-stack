"""Pure public approval/journal tests. No host inspection, sockets or services."""
import copy
import importlib.util
import json
from pathlib import Path
import sys
import types
import unittest

ROOT=Path(__file__).resolve().parents[3]
ansible=types.ModuleType('ansible');utils=types.ModuleType('ansible.module_utils')
utils.__path__=[str(ROOT/'ansible/module_utils')]
sys.modules.setdefault('ansible',ansible);sys.modules.setdefault('ansible.module_utils',utils)
from ansible.module_utils import percival_memory_contract as c
from ansible.module_utils import percival_memory_journal as j

NOW=1800000000

def inputs():
    receipt={'schema':1,'kind':'same-binary-scoped-broker-qualification','binary_sha256':c.HOTEL,
             'source_revision':'a'*40,'artifact_sha256':c.HOTEL,'source_artifact_zip_sha256':'b'*64,
             'source_run_id':123,'source_repository':'likesjx/philotic-stack','fixture_sha256':'c'*64,
             'passed':True,'production_selection':False}
    policy={'caller_uid':1003,'hotel_uid':999,'secret_ref':'secret://hotel/default/percival-muninn-observe/public-fixture',
            'introspection_secret_ref':'secret://hotel/default/percival-issuer-introspection/public-fixture'}
    before=c.canonical({**policy,'introspection_secret_ref':policy['introspection_secret_ref']+'-old'})+b'\n'
    m={'schema':1,'kind':'existing-hotel-memory-activation','transaction':'a'*32,'approved_at':NOW,'expires_at':NOW+1800,
       'config_sha256':'d'*64,'hotel_sha256':c.HOTEL,'gateway_source':c.GATEWAY,'qualification_sha256':c.digest(receipt),
       'baseline':{'gateway_unit_sha256':'e'*64,'discovery_sha256':'f'*64,'policy_sha256':j.bytes_sha(before),'supervisor_sha256':'b'*64,'application_override_sha256':'e'*64,'discovery_node_sha256':'81925c0995b5c1427b5d538e6a90ca2fdc4daffb786b09af749beaf7369d4e90'},
       'policy':policy,'issuer_attestation':{'revision':'2031a9cff6609ed77e961868b976ba8c7bcf87c6','image':'sha256:9c218f8fd65bb2a888976658edf3aded3ab0978f5435fe93e08740e1bb1dcf1f','authority_mode':'absent','coordination_mode':'absent'},
       'owner_attestations':dict.fromkeys(c.GATES,True),'operator_sha256':{name:j.bytes_sha((ROOT/name).read_bytes()) for name in c.OPERATOR_FILES},'native_key_attestation':{'vault':'default','scope':'observe','expires_at':NOW+7200,'verified_at':NOW,'revoked':False,'policy_reference_match':True},'recovery':None}
    return m,receipt,before

class Contract(unittest.TestCase):
    def test_outer_variables_and_actual_operator_closure(self):
        m,q,b=inputs()
        value={'percival_memory_manifest':m,'percival_memory_qualification':q,'percival_memory_approval_sha256':c.digest(m)}
        c.validate_vars(value,'activate',ROOT,NOW)
        for key in ('ansible_connection','percival_memory_fixture_root','percival_profile','memory_operator_sha256'):
            with self.subTest(key=key),self.assertRaises(ValueError):
                c.validate_vars({**value,key:'unsafe'},'activate',ROOT,NOW)
        m['operator_sha256']['justfile']='0'*64
        value['percival_memory_approval_sha256']=c.digest(m)
        with self.assertRaises(ValueError):c.validate_vars(value,'activate',ROOT,NOW)

    def test_cli_emits_only_validated_public_snapshot(self):
        import subprocess
        import tempfile
        import time
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'public.json'
            script=ROOT/'ansible/module_utils/percival_memory_contract.py'
            stage={'percival_memory_package_dir':'/tmp/public-package','percival_memory_config_file':'/tmp/public.json','percival_memory_config_sha256':'a'*64}
            path.write_text(json.dumps(stage))
            command=[sys.executable,'-B',str(script),'--validate-vars',str(path),'--mode','stage','--emit-vars']
            result=subprocess.run(command,capture_output=True,text=True,check=True)
            snapshot=result.stdout
            self.assertEqual(snapshot,c.canonical(stage).decode()+'\n')
            path.write_text(json.dumps({**stage,'ansible_connection':'DO_NOT_PRINT_INVALID_VALUE'}))
            self.assertEqual(json.loads(snapshot),stage)
            refused=subprocess.run(command,capture_output=True,text=True)
            self.assertEqual(refused.returncode,2)
            self.assertNotIn('DO_NOT_PRINT_INVALID_VALUE',refused.stdout+refused.stderr)
            m,q,b=inputs();m['approved_at']=int(time.time());m['expires_at']=m['approved_at']+1800
            m['native_key_attestation'].update(verified_at=m['approved_at'],expires_at=m['expires_at']+1800)
            value={'percival_memory_manifest':m,'percival_memory_qualification':q,'percival_memory_approval_sha256':c.digest(m)}
            path.write_text(json.dumps(value))
            result=subprocess.run([sys.executable,'-B',str(script),'--validate-vars',str(path),'--mode','activate','--approval-sha',c.digest(m),'--emit-vars'],capture_output=True,text=True,check=True)
            self.assertEqual(json.loads(result.stdout),value)

    def test_stage_outer_paths_and_explicit_approval(self):
        stage={'percival_memory_package_dir':'/tmp/public-package','percival_memory_config_file':'/tmp/public.json','percival_memory_config_sha256':'a'*64}
        c.validate_vars(stage,'stage',ROOT)
        for path in ('relative','/tmp/../secret','/tmp/{{payload}}','/tmp/a\n','/tmp//a'):
            with self.subTest(path=path),self.assertRaises(ValueError):
                c.validate_vars({**stage,'percival_memory_config_file':path},'stage',ROOT)
        m,q,b=inputs()
        value={'percival_memory_manifest':m,'percival_memory_qualification':q,'percival_memory_approval_sha256':c.digest(m)}
        with self.assertRaises(ValueError):c.validate_vars(value,'activate',ROOT,NOW,approval_sha='0'*64)
        c.validate_vars(value,'activate',ROOT,NOW,approval_sha=c.digest(m))

    def test_native_key_missing_expired_wrong_scope(self):
        m,q,b=inputs()
        for key in ({}, {**m['native_key_attestation'],'scope':'write'}, {**m['native_key_attestation'],'expires_at':NOW}):
            n={**m,'native_key_attestation':key}
            with self.subTest(key=key),self.assertRaises(ValueError):c.validate(n,q,c.digest(n),NOW)

    def test_policy_mutation_rechecks_immediate_foreign_drift(self):
        import contextlib
        import tempfile
        from unittest.mock import patch
        from ansible.module_utils import percival_memory_host as h
        m,q,b=inputs()
        record=j.initial(m,q,c.digest(m),b,NOW)
        for step in ('stop_gateway',):
            record=j.advance(record,'activate',step,'pending');record=j.advance(record,'activate',step,'done')
        record=j.advance(record,'activate','write_policy','pending')
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);policy=root/c.POLICY.lstrip('/');policy.parent.mkdir(parents=True);policy.write_bytes(b)
            class SyntheticHost(h.Host):
                def __init__(self):pass
                def locked(self):return contextlib.nullcontext()
                def load(self,transaction):return root,record,3
                def path(self,path):return str(root/path.lstrip('/'))
                def read(self,path,*args,**kwargs):
                    if path.endswith('/owner.json'):return c.canonical({'transaction':m['transaction'],'activation_sha256':c.digest(m)})
                    return Path(self.path(path)).read_bytes()
                def unchanged(self,*args,**kwargs):pass
                def quiescent(self):pass
            original=tempfile.mkstemp
            def replace_during_write(*args,**kwargs):
                result=original(*args,**kwargs)
                policy.write_bytes(b'foreign change')
                return result
            with patch.object(c.time,'time',return_value=NOW),patch.object(h.tempfile,'mkstemp',side_effect=replace_during_write):
                with self.assertRaises(ValueError):SyntheticHost().mutation(m,q,c.digest(m),'activate','write_policy')
            self.assertEqual(policy.read_bytes(),b'foreign change')
            self.assertEqual(list(policy.parent.glob('.muninn-owned-*')),[])

    def test_atomic_publication_short_write_and_prepublication_failure(self):
        import tempfile
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as directory:
            for name,mode in (('001.json',0o600),('99-muninn-memory.conf',0o644)):
                path=Path(directory)/name
                with patch.object(j.os,'write',return_value=1):
                    with self.assertRaises(ValueError):j.atomic_publish(path,b'complete public content',mode)
                self.assertFalse(path.exists())
                with patch.object(j,'publish_noreplace',side_effect=OSError('interrupted before publication')):
                    with self.assertRaises(OSError):j.atomic_publish(path,b'complete public content',mode)
                self.assertFalse(path.exists())
                self.assertEqual(list(Path(directory).glob('.muninn-publish-*')),[])

    def test_atomic_publication_interruption_after_publish_preserves_complete_single_link(self):
        import tempfile
        from unittest.mock import patch
        m,q,b=inputs();record=j.initial(m,q,c.digest(m),b,NOW)
        pending=j.advance(record,'activate','stop_gateway','pending')
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);j.write_new(root/'000.json',record)
            for name,content,mode in (('001.json',c.canonical(pending)+b'\n',0o600),('99-muninn-memory.conf',b'complete owned override\n',0o644)):
                path=root/name
                original=j.os.fsync;calls=[]
                def interrupt_directory_sync(fd):
                    calls.append(fd)
                    if len(calls)==2:raise OSError('interrupted after atomic publication')
                    return original(fd)
                with patch.object(j.os,'fsync',side_effect=interrupt_directory_sync):
                    with self.assertRaises(OSError):j.atomic_publish(path,content,mode)
                self.assertEqual(path.read_bytes(),content)
                self.assertEqual(path.stat().st_nlink,1)
                with self.assertRaises(OSError):j.atomic_publish(path,b'foreign replacement',mode)
                self.assertEqual(path.read_bytes(),content)
            self.assertEqual(j.inspect(json.loads((root/'001.json').read_bytes()))['pending'],('activate','stop_gateway'))
            self.assertEqual(json.loads((root/'000.json').read_bytes()),record)

    def test_exact_pair_and_closed_nonsecret_rendering(self):
        m,q,b=inputs();p=c.validate(m,q,c.digest(m),NOW)
        self.assertIn('/bin/node ',p['override']);self.assertNotIn('Environment=',p['override'])
        self.assertEqual(json.loads(p['policy']),m['policy'])
        self.assertEqual(p['qualification_sha256'],c.digest(q))

    def test_stale_future_source_binary_and_receipt_drift(self):
        m,q,b=inputs()
        for change in [dict(approved_at=NOW+1),dict(expires_at=NOW),dict(hotel_sha256='f'*64),dict(gateway_source='f'*40),dict(qualification_sha256='f'*64)]:
            n={**m,**change}
            with self.subTest(change=change),self.assertRaises(ValueError):c.validate(n,q,c.digest(n),NOW)
        for key,value in [('passed',False),('production_selection',True),('artifact_sha256','a'*64),('source_run_id',True),('source_repository','other')]:
            r={**q,key:value};n={**m,'qualification_sha256':c.digest(r)}
            with self.subTest(key=key),self.assertRaises(ValueError):c.validate(n,r,c.digest(n),NOW)

    def test_missing_owner_gates_false_values_and_inline_key_refuse(self):
        m,q,b=inputs()
        for gate in c.GATES:
            n=copy.deepcopy(m);n['owner_attestations'][gate]=False
            with self.assertRaises(ValueError):c.validate(n,q,c.digest(n),NOW)
        n=copy.deepcopy(m);n['policy']['token']='PUBLIC-CANARY'
        with self.assertRaises(ValueError):c.validate(n,q,c.digest(n),NOW)
        n=copy.deepcopy(m);n['issuer_attestation']['authority_mode']='required'
        with self.assertRaises(ValueError):c.validate(n,q,c.digest(n),NOW)

    def test_approval_hash_binds_all_source_and_config_bytes(self):
        m,q,b=inputs();approved=c.digest(m);m['config_sha256']='a'*64
        with self.assertRaises(ValueError):c.validate(m,q,approved,NOW)

    def test_pending_is_not_retry_authority(self):
        m,q,b=inputs();r=j.initial(m,q,c.digest(m),b,NOW)
        r=j.advance(r,'activate','stop_gateway','pending')
        with self.assertRaises(ValueError):j.advance(r,'activate','stop_gateway','pending')
        with self.assertRaises(ValueError):j.advance(r,'activate','write_policy','pending')
        r=j.advance(r,'activate','stop_gateway','done')
        self.assertEqual(j.inspect(r)['next_activate'],1)

    def test_recovery_requires_original_hash_new_approval_and_quiescence(self):
        m,q,b=inputs();r=j.initial(m,q,c.digest(m),b,NOW)
        r=j.advance(r,'activate','stop_gateway','pending')
        n=copy.deepcopy(m);n['approved_at']=NOW+1;n['expires_at']=NOW+1801
        n['recovery']={'activation_sha256':c.digest(m),'commands_quiescent':True,'previous_record_sha256':c.digest(r),'previous_approval_sha256':None}
        rr=j.begin_recovery(r,n,c.digest(n),NOW+1)
        for step in j.RECOVERY_STEPS:
            rr=j.advance(rr,'recover',step,'pending');rr=j.advance(rr,'recover',step,'done')
        self.assertEqual(j.inspect(rr)['next_recover'],len(j.RECOVERY_STEPS))
        with self.assertRaises(ValueError):j.advance(rr,'activate','stop_gateway','done')
        for key,value in [('commands_quiescent',False),('activation_sha256','0'*64)]:
            bad=copy.deepcopy(n);bad['recovery'][key]=value
            with self.assertRaises(ValueError):j.begin_recovery(r,bad,c.digest(bad),NOW+1)

    def test_recovery_cannot_change_owned_target_or_baseline(self):
        m,q,b=inputs();r=j.initial(m,q,c.digest(m),b,NOW)
        n=copy.deepcopy(m);n['recovery']={'activation_sha256':c.digest(m),'commands_quiescent':True,'previous_record_sha256':c.digest(r),'previous_approval_sha256':None}
        for key in ('config_sha256','transaction'):
            bad=copy.deepcopy(n);bad[key]='0'*len(bad[key])
            with self.assertRaises(ValueError):j.begin_recovery(r,bad,c.digest(bad),NOW)
        r['baseline_policy']=b.decode()+' '
        with self.assertRaises(ValueError):j.inspect(r)

    def test_expired_key_blocks_activation_but_not_recovery(self):
        m,q,b=inputs();r=j.initial(m,q,c.digest(m),b,NOW)
        expired=copy.deepcopy(m);expired['native_key_attestation']['expires_at']=NOW
        with self.assertRaises(ValueError):c.validate(expired,q,c.digest(expired),NOW)
        n=copy.deepcopy(m);n['approved_at']=NOW+8000;n['expires_at']=NOW+9000
        n['recovery']={'activation_sha256':c.digest(m),'commands_quiescent':True,'previous_record_sha256':c.digest(r),'previous_approval_sha256':None}
        self.assertIsNotNone(j.begin_recovery(r,n,c.digest(n),NOW+8000))

    def test_recovery_of_recovery_requires_last_record_and_prior_approval(self):
        m,q,b=inputs();r=j.initial(m,q,c.digest(m),b,NOW)
        n=copy.deepcopy(m);n['recovery']={'activation_sha256':c.digest(m),'commands_quiescent':True,'previous_record_sha256':c.digest(r),'previous_approval_sha256':None}
        r=j.begin_recovery(r,n,c.digest(n),NOW)
        r=j.advance(r,'recover','stop_gateway','pending')
        n2=copy.deepcopy(n);n2['approved_at']=NOW+1;n2['expires_at']=NOW+1801
        n2['recovery'].update(previous_record_sha256=c.digest(r),previous_approval_sha256=c.digest(n))
        resumed=j.begin_recovery(r,n2,c.digest(n2),NOW+1)
        resumed=j.advance(resumed,'recover','stop_gateway','pending')
        self.assertEqual(j.inspect(resumed)['pending'],('recover','stop_gateway'))
        with self.assertRaises(ValueError):j.begin_recovery(r,n,c.digest(n),NOW+1)

if __name__=='__main__':unittest.main()
