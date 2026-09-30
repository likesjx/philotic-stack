---
title: "Philote Memory Watched UAT Runbook"
doc_type: workflow
domain: memory-context
status: active
last_updated: 2026-09-17
tags:
  - muninn
  - memory
  - uat
  - watched-live
  - operator
related_docs:
  - ../architecture/MUNINN_MEMORY_CORE_PROPOSAL.md
  - ../process/WORKFLOW.md
task_refs:
  - ../task.md
---

# Philote Memory Watched UAT Runbook

Acceptance for the philote memory loop: recall, remembering, dispersal to the
observers, the nightly sleep cycle, and credential hygiene. Two halves — an
automated pass over the live fleet, and a short operator conversation that only a
human can judge.

**Do not use `just uat` for this.** That recipe boots an ephemeral hotel and has
killed the live mac-jane hotel before. The script below inspects the fleet as it
is actually running.

## 1. Automated pass

```bash
bash scripts/uat-philote-memory.sh            # read-only
bash scripts/uat-philote-memory.sh --write    # adds one end-to-end write, then forgets it
```

Read-only by default. It reads Muninn health, reads memories back by id, and
reads each hotel's ledger and log. Every line is PASS / WARN / FAIL and the exit
status is non-zero if anything FAILed. It needs ssh to mbp-jane and jane-vps.

What each check means, and what to do when it fails:

| Check | Passing looks like | If it fails |
|---|---|---|
| Muninn health | all three nodes `ok`, observers on the rc1 build | start the daemon (`~/.muninn/resync-{mbp,mac}.sh --start`) |
| Replication | both observers hold the 25 newest Cortex memories per vault | an observer is behind; see the repair below |
| Recall health | failures under 10% of recalls, median well under a second, bands present | read `Auto recall failed` in that hotel's log. `token_rejected` means the hotel's vault tokens are stale — run the token sync below |
| Rejected writes | zero HTTP 421 in 24h | writes are hitting a local observer instead of the Cortex: check `muninn_write_route` and that the philote build carries the forward path |
| Tool grants | every toolset profile grants `memory.recall` + `memory.remember` | reseed by restarting the hotel after a deploy; `companion` is deliberately tool-free and is excluded |
| Credential | vault-held, no plaintext in `config:muninn` | see "credential" below |
| Sleep | a run within 48h, no failed vaults | check `MemorySleep` lines in the Cortex hotel log |

An idle hotel reports WARN for "no recalls in 24h" — that is not a defect.

## 2. Operator conversation (the half a script cannot judge)

Do this over Telegram with a persona that has recent memories (Beacon on
vps-jane, Björk on mac-jane). Five minutes:

1. **Recall is relevant.** Ask about something the agent should know
   ("what's my organ practice schedule?"). The answer should use real stored
   detail, not a generic reply, and should not drag in unrelated memories.
2. **Recall is honest about staleness.** Ask about something that changed. The
   agent should prefer what you just said over the older memory, and say so.
3. **Remembering works on request.** Say "remember that <durable fact>". The
   reply must claim a save only if it actually called the tool — verify with
   `scripts/tool-usage.sh` or by asking about the fact in a NEW conversation
   later.
4. **Automatic capture works.** State a durable fact plainly ("my dentist is
   Dr X"), without asking for it to be saved. Within a day, a new conversation
   should be able to recall it.
5. **Nothing embarrassing surfaces.** Watch for test probes, self-heal noise, or
   months-old events presented as current.

Record the outcome in `docs/task.md` and, if something is wrong, file it with the
exact turn so it can be traced in the hotel log.

## 3. Repairs this UAT points at

- **Observer missing memories** — restore it from a fresh Cortex checkpoint:
  `bash ~/.muninn/resync-mbp.sh` or `~/.muninn/resync-mac.sh` (each keeps a
  rollback copy and has `--rollback` / `--start`). Rescue observer-only memories
  to the Cortex first; a restore replaces the observer's store.
- **`token_rejected` recall failures** — a restore replaces the observer's auth
  store with the Cortex's, so locally minted vault tokens stop existing and an
  observer cannot mint replacements (minting is a write; observers answer 421).
  Run on that Mac, then restart the hotel:

  ```bash
  python3 scripts/sync-muninn-vault-tokens.py --socket ~/.philotic/bjork/aiua-mac-jane.sock
  ```

- **Credential** — the Muninn admin credential belongs in the hotel vault
  (`muninn_admin_secret_ref`), never as plaintext in `config:muninn` or
  `mesh-config.json`. `aiua load` prefers the vault copy.

## 4. Known-good baseline (2026-09-17)

Numbers to compare against, from the run that closed Phase 2:

- recall median 34-157 ms, `degraded_vaults=[]`, mostly `moderate` bands with
  the weak matches dropped by the gate
- zero rejected writes in 24h on both Macs
- both observers hold every Cortex memory (checked by id, all vaults)
- sleep: 3 vaults, 232 memories, 0 duplicate groups, 9 diagnostic probes
  forgotten, 0 contradictions
