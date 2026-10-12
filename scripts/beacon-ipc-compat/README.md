# Synthetic cross-version envelope compatibility

This harness uses public fixtures and pinned sources in the existing, read-only
45-minute PR job. It does not contact installed services or model providers.

The old Rust hotel uses `TestGraphAdapter`, manually registered receivers and
explicit `target_guest_id` addressing. It delivers three ordinary plain-text
payloads carrying `model.openrouter`, `model`, and `model.ollama` labels. The
candidate parses each exported `InboundTask`, then invokes the same in-process
synthetic Ollama probe through `invoke_ordinary_provider_attempt`. Assertions
check model/token preparation, removal of observation metadata, and preservation
of the original task. The probe deliberately returns a fixed error marker;
there is no successful model response.

This proves old-hotel envelope transport and candidate parser/helper compatibility.
It does **not** prove the controller event loop, role-specific provider selection,
reply delivery, normal Beacon cognitive success, materialization, installed
binaries, systemd or rollout. A broken provider registry or reply path could pass
this bounded harness. Full controller/old-hotel round-trip acceptance remains a
separate integration gate before rollout; no new workflow access or privileges
are requested by this revision.

Each Rust filter must list exactly one matching test and actually execute one
passing test with zero failures and zero ignored tests. The runner rejects
zero-test, skipped, failed, multiple-test and missing-output summaries. Its
existing contract check exercises one accepted and five rejected synthetic
summary controls. This closes the candidate-ignore false-positive path without
expanding the harness's proof claim.
