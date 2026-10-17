"""Public mocked preflight regression; never invokes systemd."""
import contextlib,io,json,pathlib,tempfile,types,unittest
from unittest.mock import patch
import rehearse as r

class PreflightEvidence(unittest.TestCase):
    def full_preflight(self,jobs=''):
        with tempfile.TemporaryDirectory() as tmp:
            root=pathlib.Path(tmp)
            def command(*args):
                if args==('systemctl','--version'):out='systemd 255\n'
                elif args==('systemctl','show','--property=UnitPath','--value'):out=str(root)+'\n'
                else:out=jobs
                return types.SimpleNamespace(stdout=out)
            def metadata(name,*props):return {'LoadState':'loaded' if name.endswith('.slice') else 'not-found',
                'ActiveState':'inactive','SubState':'dead','FragmentPath':'','SourcePath':'','DropInPaths':'','ControlGroup':'','Transient':'no'}
            reads={'/proc/1/comm':'systemd','/proc/sys/fs/suid_dumpable':'2','/proc/sys/kernel/yama/ptrace_scope':'1'}
            with patch.multiple(r,BASE=root/'base',UNITPATH=root/r.UNIT,SLICEPATH=root/r.SLICE,run=command,show=metadata),\
                patch.object(r.os,'geteuid',return_value=0),patch.object(r.sys,'platform','linux'),\
                patch.dict(r.os.environ,{'GITHUB_ACTIONS':'true','RUNNER_ENVIRONMENT':'github-hosted'}),\
                patch.object(r.os,'uname',return_value=types.SimpleNamespace(machine='x86_64')),\
                patch.object(pathlib.Path,'read_text',lambda p:reads[str(p)]),contextlib.redirect_stdout(io.StringIO()):
                report={};r.preflight(report);return report
    def test_full_preflight_synthesized_hierarchy_and_unrelated_job(self):
        report=self.full_preflight('12 unrelated.service start running\n')
        self.assertEqual(len(report['unit_metadata']),4)
    def test_malformed_job_output_fails_closed(self):
        with self.assertRaisesRegex(AssertionError,'valid-job-row'):self.full_preflight('unexpected output')
    def test_empty_synthesized_slice_is_accepted_with_exact_metadata(self):
        report={};data={'LoadState':'loaded','ActiveState':'inactive','SubState':'dead',
            'FragmentPath':'','SourcePath':'','DropInPaths':'','ControlGroup':'','Transient':'no'}
        with patch.object(r,'show',return_value=data),contextlib.redirect_stdout(io.StringIO()) as output:
            r.inspect_fresh_unit(report,r.SLICE,[],set())
        self.assertEqual(report['unit_metadata'][r.SLICE],data)
        self.assertEqual(json.loads(output.getvalue().splitlines()[0])['metadata']['LoadState'],'loaded')
    def test_preexisting_or_unknown_resources_are_refused(self):
        good={'LoadState':'loaded','ActiveState':'inactive','SubState':'dead','FragmentPath':'',
            'SourcePath':'','DropInPaths':'','ControlGroup':'','Transient':'no'}
        self.assertTrue(r.fresh_metadata(r.SLICE,good))
        for key,value in [('LoadState','masked'),('ActiveState','active'),('SubState','running'),
            ('FragmentPath','/etc/systemd/system/a.slice'),('SourcePath','/generator'),
            ('DropInPaths','/a.conf'),('ControlGroup','/occupied'),('Transient','yes')]:
            self.assertFalse(r.fresh_metadata(r.SLICE,{**good,key:value}))
        for key in good:self.assertFalse(r.fresh_metadata(r.SLICE,{k:v for k,v in good.items() if k!=key}))
        self.assertFalse(r.fresh_metadata(r.UNIT,good))
        with tempfile.TemporaryDirectory() as tmp,patch.object(r,'show',return_value=good),contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaises(AssertionError):r.inspect_fresh_unit({},r.SLICE,[tmp],{r.SLICE})
            p=pathlib.Path(tmp)/r.SLICE;p.symlink_to('/nonexistent-public-fixture')
            with self.assertRaises(AssertionError):r.inspect_fresh_unit({},r.SLICE,[tmp],set())
            p.unlink();(pathlib.Path(tmp)/(r.SLICE+'.d')).mkdir()
            with self.assertRaises(AssertionError):r.inspect_fresh_unit({},r.SLICE,[tmp],set())
    def test_preflight_failure_writes_receipt_without_systemd_cleanup(self):
        def fail(report):r.check(report,'fixture-failure','loaded','not-found')
        with tempfile.TemporaryDirectory() as tmp,patch.object(r,'HERE',pathlib.Path(tmp)),\
            patch.object(r.sys,'argv',['rehearse.py','--approved-disposable-linux']),\
            patch.object(r,'preflight',side_effect=fail),patch.object(r,'stage') as stage,\
            patch.object(r,'cleanup') as cleanup,contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaises(SystemExit):r.main()
            report=json.loads((pathlib.Path(tmp)/'rehearsal-result.json').read_text())
            self.assertEqual(report['error']['check'],'fixture-failure')
            self.assertEqual(report['run_count'],0);self.assertFalse(report['runtime_passed'])
            stage.assert_not_called();cleanup.assert_not_called()
    def test_default_remains_inert(self):
        with patch.object(r.sys,'argv',['rehearse.py']),patch.object(r,'preflight') as preflight,\
            contextlib.redirect_stdout(io.StringIO()) as output:
            r.main();preflight.assert_not_called();self.assertTrue(json.loads(output.getvalue())['plan'])
    def test_owned_slice_failure_still_dispatches_cleanup(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=pathlib.Path(tmp)
            with patch.multiple(r,HERE=root,SLICEPATH=root/'fixture.slice'),\
                patch.object(r.sys,'argv',['rehearse.py','--approved-disposable-linux']),\
                patch.object(r,'preflight'),patch.object(r,'stage'),\
                patch.object(r,'digest',side_effect=RuntimeError('injected')),patch.object(r,'cleanup') as cleanup,\
                contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(SystemExit):r.main()
                cleanup.assert_called_once()
                self.assertEqual(cleanup.call_args.args[:3],(False,True,None))

if __name__=='__main__':unittest.main()
