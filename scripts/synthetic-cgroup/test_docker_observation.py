import contextlib
import importlib.util
import io
import os
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

if __name__=='__main__': unittest.main()
