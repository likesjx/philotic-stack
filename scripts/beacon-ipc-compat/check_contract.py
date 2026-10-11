"""Fail closed on the approved workflow's privilege and source boundaries."""
from pathlib import Path
import re
import runpy

HERE = Path(__file__).resolve().parent
workflow = (HERE.parents[1] / ".github/workflows/beacon-hotfix-ipc-compat.yml").read_text()
runner = runpy.run_path(str(HERE / "run.py"))
assert "timeout-minutes: 45" in workflow
assert "contents: read" in workflow
assert workflow.count("persist-credentials: false") == 3
assert "ref: " + runner["OLD"] in workflow
assert "ref: " + runner["NEW"] in workflow
assert "toolchain: '1.94.0'" in workflow
assert len(re.findall(r"^  [\w-]+:\n    runs-on:", workflow, re.M)) == 1
assert "  pull_request:" in workflow
for forbidden in ["workflow_dispatch", "pull_request_target", "secrets.", "ssh", "deploy", "upload-artifact", "download-artifact"]:
    assert forbidden not in workflow, forbidden
old = (HERE / "old-aiua-test.patch").read_text()
new = (HERE / "new-controller-test.patch").read_text()
assert "TestGraphAdapter" in old and "IpcServer::new" in old and "receiver.recv_task()" in old
assert "PUBLIC_SYNTHETIC_ONLY" in old
assert "CrossVersionProbe" in new and "PUBLIC_SYNTHETIC_PROBE_REACHED" in new
assert "invoke_ordinary_provider_attempt" in new
verify = runner['verify_test_output']
verify('test result: ok. 1 passed; 0 failed; 0 ignored; 238 filtered out;')
for summary in [
    'test result: ok. 0 passed; 0 failed; 0 ignored;',
    'test result: ok. 0 passed; 0 failed; 1 ignored;',
    'test result: ok. 2 passed; 0 failed; 0 ignored;',
    'test result: FAILED. 0 passed; 1 failed; 0 ignored;',
    '',
]:
    try:
        verify(summary)
    except RuntimeError:
        pass
    else:
        raise AssertionError('execution-count negative control was accepted')
print("PASS: pinned sources, single bounded PR-only job, read-only permissions, synthetic harness")
print("PASS: executed-test count positive control and five zero/skipped/failed/missing-output negative controls")
