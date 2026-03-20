---
name: runtime-rollout-watch
description: Use this skill when source changes must be proven in an installed, supervised, or operator-facing runtime before claiming smoke-green or watched-live-green. Trigger for Homebrew/launchd rollouts, stale-binary suspicion, installed-vs-source drift, or live watch validation after deploy.
---

# Runtime Rollout Watch

Scope: Philotic-specific, but generally applicable to installed runtimes.

Use this skill when the work crosses the boundary from "code is fixed" to "the live stack is actually running that fix."

This skill owns:

- proving installed/runtime truth before watched validation
- distinguishing source-build truth from installed-binary truth
- verifying supervisor restarts and running process paths
- checking that the observed behavior came from the updated runtime

This skill does not own:

- root-causing the underlying code bug
- proposal maintenance
- commit/push closeout

Use [$runtime-debugger](../runtime-debugger/SKILL.md) when the main problem is locating the failing live boundary.
Use [$verification-ladder](../verification-ladder/SKILL.md) when the main question is validation depth rather than rollout proof.

## Workflow

1. Identify the live runtime boundary.
2. Verify the target binary path.
3. Verify the installed artifact actually changed.
4. Verify the supervisor restarted the right process.
5. Verify the running PID is using the updated binary path.
6. Only then run smoke or watched-live validation.
7. Record whether the result is proven live truth or only source truth.

## Checks

- installed path, symlink path, or cellar path
- file timestamp and/or checksum before vs after rollout
- running PID and executable path
- launch agent / supervisor state
- relevant config or env layer if rollout depends on it

## Heuristics

- If the file write failed, nothing else matters.
- If the PID did not change, assume the old process may still be serving.
- If logs do not show fresh startup/registration, assume restart did not take.
- If the user-visible behavior contradicts the rollout story, trust the behavior first.

## Output Expectations

Report:

- what path was updated
- whether the running process changed
- whether live validation is now honest
- what still blocks live truth if rollout is incomplete
