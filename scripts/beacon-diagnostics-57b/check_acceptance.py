"""Fail closed on zero/ignored acceptance; public synthetic CI only."""
import re
import subprocess

command = ["cargo", "test", "--locked", "-p", "aiua", "--bin", "aiua",
           "service::ipc::tests::beacon_diagnostics_acceptance::beacon_diagnostics_real_controller_dispatch_reply",
           "--", "--exact", "--nocapture"]
result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                        timeout=1800, check=False, text=True)
summaries = re.findall(r"test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;", result.stdout)
if result.returncode or summaries != [("1", "0", "0")]:
    # This runner has no private credential/state sources. Compiler/test failure
    # output is source-only diagnostic evidence, never a production log fetch.
    print(result.stdout)
    print(result.stderr)
    raise SystemExit("exact synthetic acceptance failed or did not execute")
print("real baseline hotel/controller dispatch and reply: 1 passed, 0 failed, 0 ignored")
