# PR 634 macOS CI failure diagnosis

Published head: `e8d90607845da7db4c9073e0db434876dd856ae7`.
PR Check run `37834850660`, job `113509165812`, step 9 `cargo test --workspace`:
687 aiua tests passed, two failed. Test binaries built successfully. Formatting,
Clippy and Linux compile jobs passed. Percival Linux acceptance `37834851017`
passed, including both affected tests under its serial numeric-UID harness.

Both failures were introduced with this branch's new scoped-vault tests:

- `listener_owned_handlers_close_sockets_on_disable_without_late_secret_delivery`,
  scoped_vault.rs line 759 at published head: available permits 0, expected 1.
- `timed_out_blocking_resolver_retains_worker_until_completion`, line 709:
  available permits 0, expected 1.

Both failing assertions follow a fixed 120 ms sleep that assumes a synthetic
100 ms blocking job has finished and released its permit. The timeout/denial and
socket-closure assertions before them did not fail. Both unmodified tests passed
in targeted cached local runs. Classification: introduced timing-sensitive tests;
blocking-pool scheduling delay under workspace load is the likely trigger, not
an independently proven scheduler trace or evidence of an authorization bypass.

The narrow local correction changes only these two test bodies: explicitly hold
the synthetic resolver behind a release channel, confirm it entered, assert its
permit remains held after timeout/cancellation, release it, then await actual
semaphore acquisition with a five-second bound. Sender drop releases fixture work
on early test failure. Production deadlines, worker ownership and authorization
code are unchanged. The test request deadline is two seconds rather than 20 ms
to allow scheduling before exercising the real timeout path.

Validation: scoped listener test group 14 passed; corrected tests passed five
targeted repetitions each; source-harness Clippy all targets with warnings denied
and Rust formatting passed. No installs or full workspace rerun.

This correction is local, not published. The PR remains at the reviewed published
head with failed PR Check. Recommended next action: review this test-only diff,
then authorize pushing the correction so normal PR CI validates the new exact
commit. Do not merge or rerun the old commit merely to hide the timing failure.
