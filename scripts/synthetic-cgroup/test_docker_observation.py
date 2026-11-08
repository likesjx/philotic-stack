import contextlib
import importlib.util
import io
import os
import subprocess
from pathlib import Path
import unittest
from unittest.mock import patch
from types import SimpleNamespace

spec=importlib.util.spec_from_file_location('observation',Path(__file__).with_name('docker-cgroup-observation.py'))
observation=importlib.util.module_from_spec(spec);spec.loader.exec_module(observation)

class ObservationTest(unittest.TestCase):
    def test_uncertain_create_never_reports_cleanup_confirmed(self):
        def fake(args):
            if args[0]=='pull': return SimpleNamespace(returncode=0,stdout='')
            if args[0]=='create': return SimpleNamespace(returncode=1,stdout='')
            if args[0]=='ps': return SimpleNamespace(returncode=0,stdout='')
            self.fail('unexpected Docker command')
        output=io.StringIO()
        with patch.object(observation.sys,'platform','linux'), patch.dict(os.environ,{'GITHUB_ACTIONS':'true','RUNNER_ENVIRONMENT':'github-hosted'}), patch.object(Path,'is_file',return_value=True), patch.object(observation,'docker',side_effect=fake), contextlib.redirect_stdout(output):
            with self.assertRaises(SystemExit): observation.main()
        self.assertIn('synthetic_container_cleanup=uncertain',output.getvalue())
        self.assertNotIn('synthetic_container_cleanup=confirmed',output.getvalue())
        self.assertIn('synthetic_failure_stage=mongo_create',output.getvalue())


    def test_pull_failure_reports_fixed_stage_and_no_daemon_output(self):
        output=io.StringIO()
        result=SimpleNamespace(returncode=1,stdout='SENTINEL_SECRET')
        with patch.object(observation.sys,'platform','linux'), patch.dict(os.environ,{'GITHUB_ACTIONS':'true','RUNNER_ENVIRONMENT':'github-hosted'}), patch.object(Path,'is_file',return_value=True), patch.object(observation,'docker',return_value=result), contextlib.redirect_stdout(output):
            with self.assertRaises(SystemExit): observation.main()
        self.assertIn('synthetic_failure_stage=pull',output.getvalue())
        self.assertIn('synthetic_container_cleanup=uncertain',output.getvalue())
        self.assertNotIn('SENTINEL_SECRET',output.getvalue())

    def test_pull_timeout_is_uncertain_and_reports_fixed_stage(self):
        output=io.StringIO()
        with patch.object(observation.sys,'platform','linux'), patch.dict(os.environ,{'GITHUB_ACTIONS':'true','RUNNER_ENVIRONMENT':'github-hosted'}), patch.object(Path,'is_file',return_value=True), patch.object(observation,'docker',side_effect=subprocess.TimeoutExpired('synthetic',30)), contextlib.redirect_stdout(output):
            with self.assertRaises(SystemExit): observation.main()
        self.assertIn('synthetic_failure_stage=pull',output.getvalue())
        self.assertIn('synthetic_container_cleanup=uncertain',output.getvalue())

    def test_cleanup_failure_is_reported_without_raw_output(self):
        def fake(args):
            if args[0]=='pull': return SimpleNamespace(returncode=0,stdout='')
            if args[0]=='create': return SimpleNamespace(returncode=1,stdout='')
            if args[0]=='ps': return SimpleNamespace(returncode=1,stdout='SENTINEL_SECRET')
            self.fail('unexpected Docker command')
        output=io.StringIO()
        with patch.object(observation.sys,'platform','linux'), patch.dict(os.environ,{'GITHUB_ACTIONS':'true','RUNNER_ENVIRONMENT':'github-hosted'}), patch.object(Path,'is_file',return_value=True), patch.object(observation,'docker',side_effect=fake), contextlib.redirect_stdout(output):
            with self.assertRaises(SystemExit): observation.main()
        self.assertIn('synthetic_cleanup_failure=owned_container',output.getvalue())
        self.assertIn('synthetic_container_cleanup=uncertain',output.getvalue())
        self.assertNotIn('SENTINEL_SECRET',output.getvalue())

    def test_retire_exception_stage_is_bounded(self):
        fake_id='a'*64
        def fake(args):
            if args[0] in ('pull','start','rm'): return SimpleNamespace(returncode=0,stdout='')
            if args[0]=='create': return SimpleNamespace(returncode=0,stdout=fake_id)
            if args[0]=='inspect':
                import json
                # Name is checked against the dynamic recorded name.
                return SimpleNamespace(returncode=0,stdout=json.dumps([{'Id':fake_id,'Name':'/'+fake.name,'Config':{'Labels':{observation.LABEL:fake.nonce}}}]))
            if args[0]=='ps': return SimpleNamespace(returncode=0,stdout='')
            self.fail('unexpected Docker command')
        def tracked(args):
            if args[0]=='create':
                fake.name=args[args.index('--name')+1];fake.nonce=args[args.index('--label')+1].split('=',1)[1]
            return fake(args)
        output=io.StringIO()
        with patch.object(observation.sys,'platform','linux'), patch.dict(os.environ,{'GITHUB_ACTIONS':'true','RUNNER_ENVIRONMENT':'github-hosted'}), patch.object(Path,'is_file',return_value=True), patch.object(observation,'docker',side_effect=tracked), patch.object(observation,'retire_owned_runtime',side_effect=RuntimeError('SENTINEL_SECRET')), contextlib.redirect_stdout(output):
            with self.assertRaises(SystemExit): observation.main()
        self.assertIn('synthetic_failure_stage=retire',output.getvalue())
        self.assertNotIn('SENTINEL_SECRET',output.getvalue())

if __name__=='__main__': unittest.main()
