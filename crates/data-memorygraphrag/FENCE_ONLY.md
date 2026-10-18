# Supervised fence-only profile v2

`SupervisorConfig.version = 2` requires `profile = "fence_only"` and no graph connection file. It retains the existing gateway/issuer worker roles, artifact pins, kernel peer authentication, five-second lease, shared durable uncertainty journal and whole-generation shutdown on required worker loss. It does not connect to a graph; graph reads fail. Version 1 retains its required graph connection and existing behavior. Mixed versions/profiles reject before launch.

This is a bounded prerequisite for Muninn response-release ordering, not a production enrollment or complete response writer. The enrolled gateway acquires `begin_release`, validates the canonical token and trusted record policy, and holds the lease through exact-byte local transport completion before `end_release`. Issuer mutations use `begin_issuer_change`/`end_issuer_change` on that same owner. A new mutex, process-local lock or second owner is not equivalent.

The owner trusts the enrolled gateway to enforce record policy and exact HTTP bytes; it does not inspect the HTTP payload. Physical network receipt and retracting transmitted bytes are not guaranteed. Metadata writers outside this owner require their existing read/write exclusion or immutable revisions through handoff. Unknown completion must not acknowledge a clean release.

The versioned integration proposal is `MUNINN_FENCE_CONTRACT.md` on the issuer's `codex/muninn-issuer-fence` branch. Gateway integration belongs to the Muninn owner. Actual worker enrollment, identities, collection, non-private privacy label and credentials require separate approval. No production configuration is supplied here.

Qualification runs the existing native owner and real child/supervisor suites, including new graph-absence, mixed-profile, serialization and unknown-mutation/restart cases. Existing graph-backed and disposable Memgraph tests remain. The hosted artifact is a test candidate, not a deployment acceptance receipt.
