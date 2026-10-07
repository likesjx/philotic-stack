# Pending workflow patches

Changes to `.github/workflows/` that a cloud session could not push: the
Claude GitHub App used by cloud sessions lacks the `workflows` permission, so
GitHub rejects any push that touches workflow files.

Apply from a checkout with normal credentials (a Mac session or the operator):

```bash
git apply --check docs/process/workflow-patches/<file>.patch
git apply docs/process/workflow-patches/<file>.patch
git add .github/workflows && git commit -m "ci: apply <file>"
git rm docs/process/workflow-patches/<file>.patch   # once applied
```

| Patch | What | Proposal |
|---|---|---|
| `0001-pr-check-docs-lint.patch` | Non-blocking `fmt`-job step: script unit tests + `docs-metadata-check.py --warn-only` (status vocabularies) | WATCH_LIVE_BURNDOWN W0 |

Alternative: grant the GitHub App the `workflows` permission, after which a
cloud session can push these directly.
