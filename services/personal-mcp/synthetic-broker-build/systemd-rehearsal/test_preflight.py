"""Public mocked preflight regression; never invokes systemd."""
import contextlib,io,json,pathlib,tempfile,unittest
from unittest.mock import patch
import rehearse as r

class PreflightEvidence(unittest.TestCase):
    def test_loaded_slice_is_still_refused_with_exact_metadata(self):
        report={};data={'LoadState':'loaded','ActiveState':'inactive','SubState':'dead',
            'FragmentPath':'','SourcePath':'','DropInPaths':'','ControlGroup':'','Transient':'no'}
        with patch.object(r,'show',return_value=data),contextlib.redirect_stdout(io.StringIO()) as output:
            with self.assertRaisesRegex(AssertionError,'LoadState'):r.inspect_fresh_unit(report,r.SLICE)
        self.assertEqual(report['unit_metadata'][r.SLICE],data)
        self.assertEqual(json.loads(output.getvalue().splitlines()[-1])['actual'],'loaded')
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
