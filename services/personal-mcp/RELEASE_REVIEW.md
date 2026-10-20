---
title: Offline whole-vault release review
doc_type: reference
domain: deployment-distribution
status: proposed
last_updated: 2026-10-20
tags: [muninn, release, operator]
---

# Offline whole-vault release review

The paired jaredlikes-com `ops/muninn-release` directory owns the exact
non-executable source manifest, schema, source checks and compatibility report.
This repository contributes thin justfile forwarding recipes. No runtime,
provisioning, gateway policy or credential code changes.

Use absolute canonical paths:

```sh
just muninn-plan /absolute/jaredlikes-com > /absolute/review-plan.json
just muninn-check /absolute/jaredlikes-com /absolute/review-plan.json
just muninn-status /absolute/jaredlikes-com
just muninn-reconcile /absolute/jaredlikes-com /absolute/review-plan.json
```

All operations are local. `muninn-activate`, `muninn-disable`, and
`muninn-status-live` deliberately refuse; publication is not execution approval.
The pinned source is `709c0d7e3de6e85df6ba45786fbd7537123e4011`, not the
whole-vault-incomplete historical packaged pin `6618bcf16b6aeda5fd2fa6b808b1e3319556cc16`.
The proposed two-client gateway mapping and Claude-first admissions still require
paired acceptance. Installed state, native seven-day key validity, private terminal
readiness, restricted writer and artifacts remain unavailable or historical.
No provisioning should be repeated from this review.
