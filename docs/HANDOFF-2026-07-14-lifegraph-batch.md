# Session Handoff — 2026-07-14 (vps-jane / Beacon)

> Read this first. It captures a very long session. The headline: **the three things the operator originally needed are DONE and verified.** One rabbit hole (`life.observe.batch`) remains open and needs a **methodical isolation repro**, not more guess-deploys.

---

## TL;DR

| Item | Status |
|---|---|
| **Memory 401** — Beacon couldn't save operator facts (kids' names) | ✅ **FIXED + live-verified** (write+recall to `user_likesjx`, 0 401s) |
| **Transcribe timeout** — long voice memos failed | ✅ **Deployed** (unverified — needs a long voice memo) |
| **`life.observe.batch`** — flight itineraries wedge Beacon | ⏳ **3 layers fixed, handler now runs, but observations still don't land.** STOP guessing → do the isolation repro. |

All deployed to **vps-jane** (Beacon) via `just vps-deploy-ci`. Branch `codex/beacon-memory-userid-transcribe-scaling` merged to `develop` across PRs #268, #271, #275, #277, #278. **Not yet fanned out to mbp-jane / mac-jane** (same bugs affect them; they're on develop now).

The operator's flight data is **safe in Muninn** regardless (vault `user_likesjx`, engram `01KXE8B5WQ9RNNZS05DYXDVSXY`).

---

## ⏳ OPEN: `life.observe.batch` — flight observations never land

### Do this FIRST (the decisive test I kept skipping)

Drive a **multi-item `life.observe.batch` directly at the runner over IPC**, bypassing philote / the model / the 93s WaitingTool watchdog / cross-hotel noise. Adapt `crates/philotic-client/examples/life_graph_ipc_smoke_driver.rs` (it already sends a single `life.observe` via IPC). Watch **each item's write/reject/error in isolation**. Journald on the live hotel is too noisy — that's why 4 guess-deploys happened. Reproduce, don't grep.

- If items **reject** in isolation → the flight observations hit an observe-side plan/contract gate (`self.runner.plan(LifeObserve)` in `handle_observe`). Find the violation.
- If items **write** in isolation but not live → it's load/contention (Memgraph `bolt_num_workers=2`) or a philote-side issue.

### Key clue

Node count climbed **137 → 143** over the session (family details, a car-oil-change loop, etc. **did** land), but **flight/Mali observations NEVER landed — in batch OR single writes.** So this may not be the batch machinery at all; it may be something about the flight observations themselves. The isolation repro settles it in minutes.

### What's ALREADY fixed (do NOT redo)

1. **Routing** (PR #271): `life.observe.batch` was granted to the model + handled by the runner but **missing from both philote→runner routing lists** — `tools_for_allowed_class("life_graph")` (`crates/aiua/src/service/ipc.rs`) and the seeded runner `supported_tools` (`crates/aiua/src/main.rs`). Added it to both, and made the boot-seed **reconcile** `supported_tools` in place (it used to `continue` on an already-present runner keyed by a stable incarnation id, so new tools never reached existing profiles). Verified: batch now reaches `handle_observe_batch`.
2. **Connection-pool churn** (PR #275 → #277): `LifeGraphProvider::connect()` built a **new 16-conn neo4rs pool per call**, and the datasource runtime rebuilds the provider **per task** (`crates/datasource/src/runtime.rs:108`), so every observation opened its own pool — saturating Memgraph's `bolt_num_workers=2` under batch + cross-hotel load → stall → evict. Fixed with a **process-global `static LIFE_GRAPH_POOL: tokio::sync::OnceCell<Graph>`** (`crates/data-memorygraphrag/src/provider.rs`). Verified live: **1 pool per batch** (was one-per-item). NOTE: a per-instance `OnceCell` does NOT work here (provider is per-task) — it MUST be static.
3. **`life.recall.feedback` retry loop** (PR #278): once the hang cleared, turns reached far enough that the model called advisory `life.recall.feedback` with a contract-invalid payload (rating `missing` without `missing_context_refs`); the handler returned `Err` → `step_failed` → **model retry loop** ate the turn budget. Fixed: `handle_recall_feedback` now **soft-skips** malformed feedback (`Ok(feedback_not_recorded(...))`, `recorded=false`) instead of erroring. Unit test: `recall_feedback_with_contract_invalid_rating_is_soft_rejected_not_errored`.

### Latent lever (not done)

Memgraph on vps has **`bolt_num_workers=2`** (auto-sized to the 2-CPU host). The static pool made this a non-issue for churn, but if the isolation repro shows contention, bumping it is an option — Memgraph is started with **empty container Args** (no compose file found under `~`, `/opt/philotic`, `/home/deploy`); find the launch mechanism first.

### Also embedded (bundled hardening, correct regardless)

`embed_text` (`provider.rs`, the embed-on-write sidecar call) had **no HTTP timeout** — a latent hang. Added an 8s timeout + 60s circuit breaker. NOT the batch cause (the ONNX sidecar is healthy — direct curl returns a 768-d embedding <6s; the `embedding is not available` log was tool-schema text from `crates/philote/src/catalog.rs:3086`, a red herring).

---

## ✅ DONE + verified (don't reopen)

### Memory 401 (the original ask) — FIXED + live-verified

Root cause (`crates/philote/src/memory_integration.rs` `inbound_primary_user_id`): preferred the Telegram **numeric `sender_id`** over `sender_username`, so `shared_user` memory routed to vault `user_<numeric_id>` — never provisioned (`derive_vault_names` + the Telegram allowlist key on the lowercased username, e.g. `likesjx` → `user_likesjx`). Every operator-fact write 401'd; self-scoped (agent_id) writes worked. **Longstanding, not the March regression I first (wrongly) guessed — the `state.source`/`user_telegram` theory was a red herring.** Fix: prefer lowercased `sender_username`; fall back to `sender_id` only when absent. Verified live: `POST /api/engrams` succeeds, recall returns all 5 kids, 0 401s. (PR #268.)

### Transcribe timeout — deployed, UNVERIFIED

`voice.transcribe` had a fixed 55s dispatch / 35s attempt budget; long Gemini transcriptions timed out. Now scales `30 + 1.5×clip_duration` clamped `[55, 240]` using Telegram's `voice.duration`, threaded through the attachment `duration_secs` (membrane-telegram → `controller.rs` `AttachmentInput` → `runtime.rs` `effective_dispatch_timeout`). Unknown duration → 240 cap fallback. **To verify:** operator sends a ~60s voice memo → journald should show `sized transcription dispatch budget budget_secs=120` (proportional), not flat 240. (PR #268.)

---

## 🔑 Deploy mechanism & gotchas (critical — read before deploying)

- **vps-jane deploys via CI, NOT on-box.** `.github/workflows/build-linux.yml` builds the x86_64 binary set on a GH runner (the 2-CPU VPS OOM-kills release links) on every push to `develop` (+ `workflow_dispatch`). `just vps-deploy-ci` finds the latest **successful develop** run, has the VPS pull the artifact, verifies SHA256SUMS, and ansible-swaps the binaries + restarts `philotic-hotel`.
- **`/opt/philotic/src` on the VPS is STALE** (`deploy-role-loop`, pre-refactor; its `origin/develop` ref is old). It is NOT the build source. Do NOT try to build there. There is an **uncommitted `membrane-telegram` hotfix** in that tree (`editMessageText` "message is not modified" handling) that was **never deployed** — PR it to develop if wanted.
- Merging to develop deploys **develop HEAD** to vps (my branch is based on current develop), so a deploy always brings the whole develop edge, not just one fix. Fine per the operator's model but be aware.
- Deploy flow that worked: `gh pr create --base develop` → `gh pr merge <N> --merge` → wait for `build-linux.yml` run (~11 min) → `just vps-deploy-ci`. (Auto-merge is disabled on the repo.)
- Log forensics: use `sudo journalctl -u philotic-hotel --no-pager -o cat` and grep tightly — the model **prompt/tool-catalog text is logged verbatim** and pollutes almost every keyword grep (`embedding is not available`, tool schemas, recalled memory, etc.). Prefer structured module lines (`life_graph_runner::provider`, `neo4rs::`, `agent_core::runtime::turn_loop`).

---

## 🧹 Standing follow-ups (all need the operator or are deferred)

1. **Fan out all fixes to mbp-jane / mac-jane** — same bugs (memory routing, batch routing, pool churn, feedback loop) are fleet-wide. They're on develop now; needs the mac/mbp deploy path (launchd, `push-homebrew-remote.sh` — note the chmod u+w gotcha for new binaries in the Cellar bin dir).
2. **Verify transcribe** — operator sends a long voice memo (see above).
3. **Revoke throwaway key** — a full-access diagnostic key minted during the muninn incident: `ssh -t deploy@jane-vps 'muninn api-key revoke 3jaGmoCs5ek --vault user_likesjx -p'` (needs muninn root pw). Also 2 orphan `muninn_vault_token` secrets prunable (`b8c6147`, `66665303`).
4. **PR the orphaned vps `membrane-telegram` hotfix** if wanted (see gotchas).
5. **Muninn token durability (2026-07-21 incident)** — Beacon's memory 401'd again because the Jul-20 cluster rebuild **wiped Muninn's key store**, invalidating the hotel's stored tokens (the token↔key binding spans two independent stores; the hotel DB was fine, Muninn forgot its half). Fixed with a **DB-centric resync** (mint fresh Muninn keys → re-encrypt with the hotel master key → update the `graph_nodes` secret records in place → restart; verified `WRITE 201`, 0 401s). Two follow-up proposals FILED to the intel-graph:
   - **`proposal:memory-token-self-heal`** — on a token-401 (distinct from unreachable), auto re-mint + re-store from the durable hotel-DB truth, so the system self-heals across the two-store gap (the operator's "DBs are truth" philosophy, made resilient). The 2026-07-21 manual resync is the reference implementation.
   - **`proposal:muninn-vps-reharden`** — the rebuilt vps Muninn is **open (no admin auth)**; restore admin + token auth WITHOUT rotating `auth_secret` (would re-break the resynced tokens), and put the hardened baseline + config block in the deploy source so rebuilds restore it. Couples with the self-heal (which needs an admin credential source to mint).
   - Recurrence root: **rebuilds keep dropping Muninn's auth + keys.** The durable answer is the pair above (hardened baseline that rebuilds restore + hotel auto-recovery regardless).
6. Optional immediate mitigation if the batch keeps wedging Beacon before it's root-caused: **disable `life.observe.batch`** from the `life.steward` skill `implied_tools` (live DB patch) so Beacon falls back to single observe.

---

## 🏗️ Architecture item (operator-flagged): data-driven tool grants — proposal FILED

The operator's principle from this session: **"we should not have any tool hard coded."** Tonight's inability to disable one tool without a code+deploy is the motivating case. Filed as an intel-graph proposal — **`proposal:data-driven-tool-grants-skilldag`** (query it with `phil graph context_for` / `graph_context_for`).

- **Problem:** tool grants are compiled into the binaries (five surfaces: `skill_implied_tools` in `catalog.rs`, `tools_for_allowed_class` in `ipc.rs`, seeded `implied_tools` + runner `supported_tools` in `main.rs`, and `tool_catalog()`), so enable/disable/grant/re-route needs a deploy.
- **Goal:** grants become graph/config data — seeded once, editable at runtime with **no deploy**; hardcoded lists demote to first-boot seed + fallback.
- **SkillDAG decision (discussed):** keep the **authoritative, hot-path grants in the LOCAL hotel context graph** (fast, always-available, per-hotel) — do NOT put runtime tool resolution behind the remote LifeGraph (Memgraph on vps-jane), or every agent bricks when it's down (this session's failure mode). Use the **LifeGraph only as an optional reasoning/design layer** the agent proposes changes against, which then **compile down** to the local toolset (compiler pattern; autopoiesis fit). Full pros/cons in the proposal node.
- **Slices** (in the proposal): (1) grant registry in the context graph — verify by disabling `life.observe.batch` at runtime with no deploy; (2) runner routing as data (the PR #277 reconcile is a precedent); (3) governance/audit; (4) later — SkillDAG reflection in the LifeGraph.

---

## 📍 Key file/line references

- `crates/philote/src/memory_integration.rs` — `inbound_primary_user_id` (memory routing fix)
- `crates/data-memorygraphrag/src/provider.rs` — `LifeGraphProvider`, `connect()`/`build_graph()`, `static LIFE_GRAPH_POOL`, `handle_observe`, `handle_observe_batch`, `handle_recall_feedback`, `feedback_not_recorded`, `embed_text`
- `crates/datasource/src/runtime.rs:108` — provider registry rebuilt **per task** (why the pool must be static)
- `crates/aiua/src/service/ipc.rs` — `tools_for_allowed_class("life_graph")`
- `crates/aiua/src/main.rs` — `life.steward` skill `implied_tools`; seeded life-graph-runner `supported_tools` + reconcile logic
- `crates/model-router/src/runtime.rs` — `effective_dispatch_timeout` / `transcribe_budget_secs`
- `crates/philotic-client/examples/life_graph_ipc_smoke_driver.rs` — **base for the isolation repro**
- Memgraph: vps container `philotic-memgraph` @ `100.64.212.8:7687`, `bolt_num_workers=2`, `query_execution_timeout_sec=600`. Query: `echo '<CYPHER>;' | timeout 10 docker exec -i philotic-memgraph mgconsole --output-format=csv`

---

## Muninn memory pointers (this session, most recent last)

- `01KXH4TGQ67WPK2PHS62YA6XYC` — batch state + thrashing admission + isolation-repro plan (START HERE for the batch)
- `01KXGKTYB2AE6Y0DW720ZB7YNS` — feedback-loop fix
- `01KXGGKC97ZS6JSNEXZ2A3SSVN` — corrected root cause (pool churn, not embed)
- `01KXE8XGJYCH1T606J86R267P6` — batch routing fix
- `01KXDVG50W1H8AFEJZNCR9ECFZ` — memory 401 RESOLVED + verified

**Operator's 5 children (context):** Taysha Telenar; Xanthos Gabriel Wagner Likes; Zerin Maluy Likes; Mali-KJerstine Althoff Likes; Daxton Thomas Wagner Likes.
