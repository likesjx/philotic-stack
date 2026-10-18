import importlib.util
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch,Mock
spec=importlib.util.spec_from_file_location('launcher',Path(__file__).with_name('ci-launch.py'))
launcher=importlib.util.module_from_spec(spec);spec.loader.exec_module(launcher)

class Tests(unittest.TestCase):
    def test_daemon_failure_is_not_absence(self):
        with patch.object(launcher,'call',side_effect=subprocess.CalledProcessError(1,['docker'])):
            with self.assertRaises(subprocess.CalledProcessError):launcher.present('container','fixture')

    def test_cleanup_requires_matching_owner(self):
        with patch.object(launcher,'present',return_value=True),patch.object(launcher,'call',return_value=Mock(stdout=b'foreign')) as call:
            with self.assertRaises(RuntimeError):launcher.remove_owned('container','fixture','expected')
            self.assertEqual(call.call_count,1)

    def test_cleanup_verifies_absence(self):
        with patch.object(launcher,'present',side_effect=[True,False]),patch.object(launcher,'call',return_value=Mock(stdout=b'expected')) as call:
            self.assertTrue(launcher.remove_owned('container','fixture','expected'))
            self.assertEqual(call.call_args.args[0],['docker','container','rm','--force','fixture'])

if __name__=='__main__':unittest.main()
