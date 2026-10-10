# Bounded Muninn candidate evidence

This workflow builds only `aiua`, `philote`, and `heal-dispatcher` from candidate
`3144e42930d575532249e0133dc15e10b45afb58`. It packages them before adding
synthetic examples. It does not deploy, acquire credentials, create grants,
access private memory, change services, or enable LifeGraph. The existing
OpenRouter binary and every other installed binary remain outside this package.

The workflow uses public fixtures to compare candidate and hotfix
`57b13ba0087722a2de062ff86710ae64a510d445` session checkpoints and the hotfix
provider's context parser. Passing results establish source compatibility for
these fixtures, not compatibility of the actual installed OpenRouter binary.
The candidate intentionally omits unattested recalled memory and agent graph
context. The old provider duplicates the current message and does not enforce
the candidate's context budgets. These gaps must remain visible; changing the
provider route or restoring unverified recall is not an acceptance workaround.

## Acceptance gates, in order

1. Review the exact packager commit and successful workflow run. Validate the
   run, artifact metadata, canonical manifest, receipt, source pins, exact three
   components, current binary pins, and preserved OpenRouter pin with the site
   evidence checker. Metadata validation alone does not verify artifact bytes.
2. Obtain the artifact through an authorized route and verify the actual archive
   and all component bytes with `package.py`'s verifier. A blocked download stays
   blocked; do not substitute a different candidate or bypass the restriction.
3. Run synthetic acceptance against the frozen installed guest binaries and the
   candidate, including correlated refresh, context/route preservation, and the
   budget gap above. No private records or external provider calls are needed.
4. Complete real Linux systemd FD and isolation acceptance. Review separate
   memory and operations authorization. UID/GID availability must be measured
   at execution time; no account allocation is prescribed here.
5. Before a production transaction, implement and exercise a quiesced, opaque
   state backup and trio rollback under the shared release lock. The temporary
   sealed SQLite fixtures in `synthetic_restore.py` are tests, not a production
   backup backend. Preserve credentials, configuration, unit environment, other
   binaries, and OpenRouter; verify state and executable rollback together.
6. Obtain exact action-time approval for any credentials, persistent grants,
   accounts, units, or network/security changes. Begin with the synthetic
   `percival_connection_test` vault, read-only and no-learning. Real dot/Claude
   callback allowlists and independently revocable consent are onboarding
   handoffs, never inferred from synthetic callbacks.

Source/test success does not authorize activation. Broker/issuer onboarding,
LifeGraph adapter enablement, and operational release wiring have separate
owners and acceptance gates.
