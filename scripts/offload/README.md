# Portable fallback integration

This increment adds ordinary fallback tools only. It changes no hosted workflow,
publication guard, production configuration or CI permission. Start with
FALLBACK.md and COVERAGE.md. Explicitly select native-portable-verification for
the dependency-free subset. A selected-profile pass is never full-CI approval.

The integration's first patch contains 25 dependency-free harness/exporter
tests. The optional second patch adds the non-executing dependency planner and
7 tests, for 32 total. The historical 92-test count in coverage belongs to the
larger original offload drafts, not this narrowed integration.

dependency_plan.py is available only after the optional preparation patch.
It prints a plan and never installs packages. Existing npm approval does not
remove the unresolved network restriction.

Legacy run.py, shell build/qualification stages, orchestration adoption and
workflow edits remain separate unfinished work. Preserve the existing dirty
worktrees and original evidence until they are intentionally integrated.
