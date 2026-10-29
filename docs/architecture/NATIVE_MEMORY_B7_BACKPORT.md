---
title: B7 native memory route compatibility
doc_type: reference
domain: deployment-distribution
status: active
last_updated: 2026-10-29
tags: [native-memory, b7, backport]
related_docs: [../../task.md]
---

# B7 compatibility boundary

This branch is based on exact B7 source 5a08d74d944ece80940513a197c5ffa9f6f5c255,
not develop. Its only runtime change is the five-line NativeMemory route enum
addition. It supports the current native guests from PR #683 without importing
unrelated hotel changes. The draft PR targets a baseline branch pinned to B7;
merging it into develop or main is not the intended delivery operation.

Unmodified B7 rejects the native endpoint configuration as an unknown enum variant.
The backport retains existing variant wire shapes. The dedicated workflow builds
Linux aiua, provisions native and unrelated legacy endpoints through the actual
typed hotel operation, rejects a false owner, checks persistence, then uses the
current membrane startup replay and native runner for signed capture/recall/retry.
There is no endpoint table injection. The synthetic bearer resolver and manually
started runtime remain test scaffolding. Smoke mode disables materialization;
successful provisioning does not claim production guest spawning or vault setup.

The workflow pins guest/test source to 7399da86b061c3c01fd278fa631d030725720ae3.
It also builds original B7 as a separate source-built rollback candidate, runs
legacy route tests, and packages both with the existing candidate release-manifest
tool. Full SHAs, hashes, compiler and run provenance accompany the archives.
No release tag is created. Artifact retention is 14 days. This does not reproduce
or attest the installed Linux hash, nor identify the executable of a running process.

Run 40902121996 / artifact 12910732094 is unqualified: independent download
verification found identical backport and rollback binaries. Logs showed stale
Cargo output reuse across workspaces despite distinct source checkouts. Do not
deploy either file from that artifact. The corrected workflow uses separate fresh
target directories for backport, baseline and guests, prohibits identical hotel
bytes, and executes original B7 as a negative control: legacy provisioning must
succeed, native provisioning must return MALFORMED_PAYLOAD with the explicit
unknown-native_memory variant message, and no native config may persist.

# Rollout and rollback effects

VPS-only pilot: new membrane and native runner guests, existing central Memgraph.
Port 8913 is occupied; 8914 is merely a candidate requiring a fresh bind check.
No Memgraph upgrade is included. The installed hotel must be backed up and its
hash compared before replacement. Existing disk aiua hash observed:
b7d0c9703b3670efc5b93cfc8cb2561b3f80fe7d0d94be65c400987751eb7a17.
Read access to running /proc executable identity was denied; do not bypass it.

Deploying this binary requires restarting philotic-hotel.service, which can
interrupt connected guests. The user approved necessary scoped restarts today;
that does not authorize new persistent keys/grants/network/schema effects.
No production action is performed by this branch or workflow.

Before rollback, disable/revoke the new native endpoint and stop the two new
guests; restore the pre-pilot endpoint/component configuration and original
verified disk binary, then restart the hotel. Original B7 cannot parse native
route records, so leaving them configured is not a qualified rollback. Retain
Memgraph observations/receipts for reconciliation, rather than deleting data.
The source-built rollback archive is a fallback candidate, not a substitute for
the exact installed backup. Production rollback and interruption recovery remain
unverified; source contract and isolated test evidence must not imply otherwise.

Outstanding operator facts: actual node ID via existing authenticated mesh target
status; owner ID via existing desktop session status; database identity, privileges
and effective durability through authorized metadata access. No new login, token
extraction or private graph queries are part of this work. Existing personal
discovery returns 502 and is not repaired here; actual dot exposure remains open.
