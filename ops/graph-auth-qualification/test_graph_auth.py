import contextlib
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import launch_graph as launch
import probe_graph as probe
import hosted_graph_auth as hosted

PASSWORD = "a" * 64
OTHER = "b" * 64

class AuthError(Exception):
    code = probe.UNAUTHORIZED

class Record:
    def __init__(self, values): self._values = values
    def values(self): return self._values

class Result(list):
    def consume(self): pass

class Fake:
    def __init__(self, accept_all=False, failure=None):
        self.accept_all, self.failure = accept_all, failure
        self.auth = None
        self.calls = []
    def __call__(self, uri, **kwargs):
        self.auth = kwargs["auth"]
        self.calls.append((uri, kwargs))
        return self
    def __enter__(self): return self
    def __exit__(self, *args): pass
    def session(self): return self
    def run(self, statement):
        if self.failure: raise self.failure
        if not self.accept_all and self.auth != (probe.USER, PASSWORD): raise AuthError("do not print")
        return Result([Record([1] if statement.startswith("RETURN") else [probe.USER])])

class LaunchTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "passfile"
        self.path.write_text("beta_fixture:" + PASSWORD + "\n")
        self.path.chmod(0o600)
    def test_valid_launch_passes_path_not_secret(self):
        seen = []
        launch.launch(str(self.path), {}, lambda *args: seen.append(args))
        binary, argv, env = seen[0]
        self.assertEqual(binary, launch.BINARY)
        self.assertNotIn(PASSWORD, json.dumps(argv))
        self.assertEqual(env["MEMGRAPH_PASSFILE"], str(self.path))
        self.assertNotIn("MEMGRAPH_PASSWORD", env)
        self.assertIn("--auth-password-permit-null=false", argv)
    def test_bad_formats_never_exec(self):
        for data in (b"", b"beta_fixture:\n", b":bad\n", b"user:p:a\n",
                     b"beta_fixture:" + b"a"*64, b"beta_fixture:" + b"a"*64 + b"\nx:y\n"):
            with self.subTest(data=data):
                self.path.write_bytes(data)
                with self.assertRaises(ValueError):
                    launch.launch(str(self.path), {}, lambda *args: self.fail("executed"))
    def test_missing_file_never_exec(self):
        with self.assertRaises(FileNotFoundError):
            launch.launch(str(self.path)+"missing", {}, lambda *args: self.fail("executed"))
    def test_readable_by_others_refused(self):
        self.path.chmod(0o644)
        with self.assertRaises(ValueError): launch.read_private(self.path)
    def test_symlink_refused(self):
        link = self.path.with_name("link")
        link.symlink_to(self.path)
        with self.assertRaises(OSError): launch.read_private(link)
    def test_hardlink_refused(self):
        os.link(self.path, self.path.with_name("link"))
        with self.assertRaises(ValueError): launch.read_private(self.path)
    def test_conflicting_environment_never_exec(self):
        for key in ("MEMGRAPH_USER", "MEMGRAPH_PASSWORD", "MEMGRAPH_CONFIG"):
            with self.assertRaises(ValueError):
                launch.launch(str(self.path), {key:""}, lambda *args: self.fail("executed"))
    def test_wrong_owner_refused(self):
        with patch("launch_graph.os.getuid", return_value=os.getuid()+1):
            with self.assertRaises(ValueError): launch.read_private(self.path)

class ProbeTests(unittest.TestCase):
    def test_all_three_phases(self):
        for phase in ("fresh", "restart", "changed_passfile"):
            fake = Fake()
            probe.check(fake, AuthError, {"password":PASSWORD,"replacement":OTHER}, phase)
            self.assertEqual(len(fake.calls), 9)
            self.assertTrue(any(kw["auth"] is None for _,kw in fake.calls))
    def test_unauthenticated_server_fails(self):
        with self.assertRaisesRegex(probe.ProbeFailure, "unauthorized_connection_accepted"):
            probe.check(Fake(accept_all=True), AuthError, {"password":PASSWORD,"replacement":OTHER}, "fresh")
    def test_correct_credentials_auth_failure_is_explicit(self):
        with self.assertRaisesRegex(probe.ProbeFailure, "positive_authentication_denied"):
            probe.check(Fake(failure=AuthError()), AuthError, {"password":PASSWORD,"replacement":OTHER}, "fresh")
    def test_transport_failure_is_not_denial(self):
        with self.assertRaisesRegex(probe.ProbeFailure, "denial_not_authentication"):
            probe.denied(Fake(failure=ConnectionError("private")), AuthError, probe.USER, OTHER)
    def test_wrong_error_code_is_not_denial(self):
        err = AuthError("private"); err.code = "Neo.ClientError.Security.Forbidden"
        with self.assertRaisesRegex(probe.ProbeFailure, "denial_not_authentication"):
            probe.denied(Fake(failure=err), AuthError, probe.USER, OTHER)
    def test_bad_credentials_refused(self):
        with self.assertRaises(probe.ProbeFailure):
            probe.check(Fake(), AuthError, {"password":PASSWORD,"replacement":PASSWORD}, "fresh")
    def test_no_managed_retry(self):
        fake = Fake()
        probe.accepted(fake, PASSWORD)
        for _, kw in fake.calls: self.assertEqual(kw["max_transaction_retry_time"], 0)

class HostedTests(unittest.TestCase):
    def test_no_local_execution(self):
        with patch.dict(os.environ, {}, clear=True), patch.object(hosted,"call") as call:
            with self.assertRaisesRegex(hosted.Failure,"approved_disposable_job_required"):
                hosted.main("/does-not-exist")
            call.assert_not_called()
    def test_container_isolation(self):
        row = {"HostConfig":{"NetworkMode":"none","Privileged":False,
               "RestartPolicy":{"Name":"no"},"PidMode":"","IpcMode":"private",
               "CapDrop":["ALL"],"SecurityOpt":["no-new-privileges"]},
               "Mounts":[{"Type":"bind","Destination":p} for p in
               ("/var/lib/memgraph","/fixture","/qualification","/deps")]}
        hosted.isolated(row)
        for k, value in (("NetworkMode","host"),("Privileged",True),("PidMode","host"),
                         ("CapDrop",[]),("SecurityOpt",[]),("PortBindings",{"7687/tcp":[{}]})):
            changed=json.loads(json.dumps(row));changed["HostConfig"][k]=value
            with self.subTest(key=k), self.assertRaises(hosted.Failure): hosted.isolated(changed)
    def test_cleanup_only_verified_id(self):
        ident="a"*64
        with patch.object(hosted,"owned",side_effect=hosted.Failure("identity")), patch.object(hosted,"call") as call:
            with self.assertRaises(hosted.Failure): hosted.remove(ident,"b"*32,"sha256:"+"c"*64)
            call.assert_not_called()
    def test_scratch_removal_error_propagates(self):
        with patch.object(hosted.shutil, "rmtree", side_effect=OSError("busy")):
            with self.assertRaises(OSError): hosted.remove_scratch("/only-test-placeholder")
    def test_cleanup_failure_prevents_success_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            deps=Path(directory)/"deps"; (deps/"neo4j").mkdir(parents=True)
            (deps/"neo4j/__init__.py").write_text("")
            ident="a"*64
            def calls(args, *extra):
                if args[:2] == ["image","inspect"]: return '[{"Id":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}]'
                if args[0] == "create": return ident
                return ""
            env={"GITHUB_ACTIONS":"true","CI":"true","RUNNER_OS":"Linux",
                 "GITHUB_REPOSITORY":"likesjx/philotic-stack","RUNNER_TEMP":directory}
            output=io.StringIO()
            with patch.dict(os.environ,env), patch.object(hosted,"call",side_effect=calls), patch.object(hosted,"owned",return_value={}), patch.object(hosted,"isolated"), patch.object(hosted,"ready"), patch.object(hosted,"remove"), patch.object(hosted,"stop_for_restart"), patch.object(hosted,"remove_scratch",side_effect=OSError("busy")), contextlib.redirect_stdout(output):
                with self.assertRaisesRegex(hosted.Failure,"cleanup_inconclusive"):
                    hosted.main(str(deps))
            self.assertIn("cleanup:inconclusive",output.getvalue())
            self.assertNotIn("qualification:passed",output.getvalue())
    def test_nonzero_wait_is_not_graceful_restart(self):
        with patch.object(hosted,"call",side_effect=["","137"]):
            with self.assertRaisesRegex(hosted.Failure,"restart_stop_not_graceful"):
                hosted.stop_for_restart("a"*64)
    def test_cleanup_stop_wait_remove(self):
        ident="a"*64
        with patch.object(hosted,"owned",side_effect=[{"State":{"Running":True}},{"State":{"Running":False}}]), patch.object(hosted,"call",return_value="") as call:
            hosted.remove(ident,"b"*32,"sha256:"+"c"*64)
            self.assertEqual(call.call_args_list[0].args[0],["stop","--time","20",ident])
            self.assertEqual(call.call_args_list[1].args[0],["wait",ident])
            self.assertEqual(call.call_args_list[2].args[0],["rm",ident])
    def test_semantic_probe_failure_not_retried(self):
        result=type("R",(),{"returncode":1,"stdout":"graph_auth:failed:unauthorized_connection_accepted"})()
        with patch.object(hosted,"owned",return_value={"State":{"Running":True}}), patch.object(hosted,"probe",return_value=result) as call, patch.object(hosted.time,"sleep") as sleep:
            with self.assertRaises(hosted.Failure): hosted.ready("a"*64,"b"*32,"sha256:"+"c"*64,"fresh")
            self.assertEqual(call.call_count,1);sleep.assert_not_called()

if __name__ == "__main__": unittest.main()
