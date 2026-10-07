# Handoff — 2026-10-07 — Atlas Follow-Through (for Claude Code on mac-jane)

> Written by a cloud session that could not download crates
> (`static.crates.io` blocked), could not push `.github/workflows/*`
> (GitHub App lacks `workflows` permission), and cannot reach the hotels.
> Everything below needs at least one of those. The operator approved
> implementing all six Atlas plans with the **recommended option at every
> decision point**.
>
> Branch with the finished non-Rust work: `claude/stoic-goldberg-f8w99b`
> (based on `origin/develop` @ `f170cb88`). Index: `docs/task.md` →
> "Atlas Follow-Through".

## 0. Start

```bash
git fetch origin && git switch claude/stoic-goldberg-f8w99b
just session-start            # Muninn triad + graph claim
just check && just test       # baseline must be green before editing
python3 -m unittest discover -s scripts/tests -p 'test_*.py'   # 40 tests, 1 skip
```

Follow the repo branch model: one `codex/<slug>` worktree per slice
(`just workstream-start <slug>`), PR into `develop`, `just
workstream-overlap <slug>` before opening. Merge
`claude/stoic-goldberg-f8w99b` into `develop` first (it is docs, scripts,
ansible only — no Rust), or base slices on it.

## 1. Apply the parked workflow patches (S, needs your git credentials)

```bash
for p in docs/process/workflow-patches/000{1,2,3}-*.patch; do git apply --check "$p" && git apply "$p"; done
git add .github/workflows && git rm docs/process/workflow-patches/000*.patch
git commit -m "ci: apply parked workflow patches (docs lint, package manifest, release.yml)"
```

Then: PR → develop; confirm `pr-check` and `build-linux` stay green (the
manifest-driven package list was verified identical to today's 16 packages).
Release rehearsal (`v0.2.0-rc.1` on develop) waits for R2 below — see
`docs/process/RELEASE.md`.

## 2. Rust slices, in this order

Each slice: tests named in its proposal, `cargo test -p <crates>`, `just
check`, PR with test counts. Proposals hold file:line refs (re-check — develop
has moved since 10-05).

| # | Slice | Proposal | Notes |
|---|---|---|---|
| 1 | **IPC split S0–S2** (rename to `ipc/mod.rs`, tests file, pure fns, small leaf families) | IPC_DISPATCH_SPLIT | Verbatim moves only; equal `cargo test -p aiua` count per PR. Do these first — later slices edit the same handlers. |
| 2 | **Mesh L1** loud inbound, **L2** gossip budget, **L3** doctor checks | MESH_DELIVERY_GUARANTEES | No wire change; safe rolling deploy. |
| 3 | **Mesh L6 option (a)**: delete dead cron broadcast emitters + handlers + duplicate builders | MESH_DELIVERY_GUARANTEES | Decision already made: delete. |
| 4 | **Perimeter P1 (Rust parts)**: socket 0600/dir 0700, drop `/tmp` fallback, `peer_cred` uid check, `mcp_owner_identity_ok(None)=false`, `BeaconMessage.seq` u64, exec-plane accept allowlist | PERIMETER_ENFORCEMENT | Scripts already migrated to `operator` (commit 42a30638). `sync-muninn-vault-tokens.py` still registers as `hotel` — leave until P3. |
| 5 | **IPC split S3** (mid-size families incl. config_vault, mcp_endpoint) | IPC_DISPATCH_SPLIT | Before P2 touches those families. |
| 6 | **Perimeter P2** reserved roles, config-key ACL, vault AAD + `phil vault reseal` | PERIMETER_ENFORCEMENT | |
| 7 | **Mesh L4** ack-what-you-delivered + `mesh_dead_letters` (7-day retention) + `phil mesh dead-letters` (replay operator-only) | MESH_DELIVERY_GUARANTEES | Extract `apply_ledger_command` first. |
| 8 | **IPC split S4–S5** (desktop/placement, tasks/emit/delivery) | IPC_DISPATCH_SPLIT | Not concurrently with step 9. |
| 9 | **Mesh L5 + frontdoor F1**: `NodeCapabilities.features`, `created_at`/`expires_at` ms (25 s for `execute_tool`), dispatcher expiry + attempts | MESH_DELIVERY_GUARANTEES, AGENT_FRONTDOOR | Closes DEF-211. |
| 10 | **IPC split S6** park-path fixes (DEF-223 ParacrineEmit env gap first), cron `fire` acts on undelivered | IPC_DISPATCH_SPLIT | Smoke: `just smoke-paracrine`. |
| 11 | **Perimeter P3** guest capability tokens + `operator.token`, **soft mode** (log-only) first | PERIMETER_ENFORCEMENT | Enforce in the following release. Migrate `sync-muninn-vault-tokens.py` to the operator token. |
| 12 | **Perimeter P4** MAC v2 phase A (dual-verify) → B → C latch → D min-version | PERIMETER_ENFORCEMENT | One phase per fleet deploy; never skip. |
| 13 | **Perimeter P5** signed placement + route `set_home` through the home | PERIMETER_ENFORCEMENT | After P4. Add operator break-glass for a dead home hotel. |
| 14 | **Perimeter P6** egress policy load + runner deny overlay, Public tier **AllowWithAudit** | PERIMETER_ENFORCEMENT | Flip to Deny only after 2 weeks of audit data. |
| 15 | **Mesh L7** `life.observe` write confirmation + one idempotent retry; runner SIGTERM drain | MESH_DELIVERY_GUARANTEES | Independent; can go anywhere. |
| 16 | **Release R2** workspace version 0.2.0, `build_info()`, `--version`, status fields | RELEASE_TRAIN | Unblocks rc rehearsal + `verify-release` build_sha. |
| 17 | **Watch-live W7** doctor readiness checks; **W3 step 5** widen `reports_an_outcome` verbs; **W6** `phil decisions report` | WATCH_LIVE_BURNDOWN | |
| 18 | **Frontdoor F3** caller tags on Muninn writes; **F6** `mcp.grant-expiry` doctor check | AGENT_FRONTDOOR | F6 before 2026-10-23 (grants expire ~10-30). |
| 19 | **Perimeter P7 Rust**: `aiua load` accepts `secret://` refs + `--scrub`; DEF-089 token redaction; blob upload quota | PERIMETER_ENFORCEMENT | |

## 3. Deploy and live proofs (operator-attended)

1. **vps-jane deploy** (`just vps-deploy-ci`): brings the nftables hotel-port
   filter and memory hygiene. Check `sudo nft list table inet philotic`, an
   off-tailnet probe of 16466–16468 (dropped), mesh still healthy (`phil
   doctor`), and the next 03:00 UTC `memory.hygiene: sweep complete` with
   `vaults_scanned > 0`.
2. **Frontdoor before 2026-10-30**: on mac-jane `EXPORT_SCHEMAS=…
   OWNER_AGENT_ID=agent-bjork-01 python3 scripts/provision-agent-frontdoor.py`;
   on vps-jane re-provision with `SCHEMA_FILE=…` (rotates tokens = the F6
   drill); update cloud env + Keychain; `mcp-client-uat.sh agent-frontdoor`;
   old token refused.
3. **Release**: after R2 + patches, tag `v0.2.0-rc.1` on develop, then
   `just vps-deploy-release v0.2.0-rc.1`, `scripts/install-release-mac.sh`
   on each Mac, `just verify-release <host> v0.2.0-rc.1`, one rollback drill
   per platform; then R1 main sync (PR, merge commit) and `v0.2.0`.
4. **Watch-live backlog**: work `docs/process/WATCH_LIVE_BACKLOG.md` weekly
   (W1 journal-grep closures, W3 procedure run, W5 relocation after fixing
   mac-jane peer ports in `ansible/host_vars/jane-vps.yml`, A9 snapshots).

## 4. Close out

`graph_decide` per slice, Muninn memory delta (decisions, reality gaps),
tick `docs/task.md` "Atlas Follow-Through", move proposal statuses as slices
land (the status lint now enforces the vocabulary).
