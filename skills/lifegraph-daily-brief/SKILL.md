# LifeGraph Daily Brief

> **Runtime truth lives in the cron payload**, not this file: no crate reads
> SKILL.md at runtime (the skills/ tree is human documentation — see the
> architect-charter reality-gap notes). The deliverable prompt is
> `BRIEF_PROMPT` in
> `crates/philotic-client/examples/life_graph_brief_cron_register.rs`.
> Keep the two in sync when editing.

## Purpose

The operator-invited standing digest from the LIFE_GRAPH_ACTIVE proposal
(slice S3): every morning the chief-of-staff steward (Beacon) composes one
Telegram message from the LifeGraph and invites reactions. It is a scheduled
digest the operator explicitly requested — not an autonomous interruption —
so the Attention Steward's observe-only gate is respected, and brief
reactions are exactly the SIL evidence that gate waits for (S4).

## Delivery mechanics

- Cron job `lifegraph-daily-brief:vps-jane`, schedule `0 0 11 * * * *`
  (11:00 UTC daily), target `role:agent-beacon:orchestrator`, registered at
  runtime via `IpcRequest::RegisterCronJob` (same path as the
  `cron.register` tool). Session target is `Isolated` (`cron:<job_id>`), so
  brief turns never evict the conversational apartment window; the reply
  reaches Telegram via the payload's independent `chat_id`/`source` fields.
- Payload carries `preapproved_tools: ["life.recall", "life.recall.feedback"]`
  (forwarded because `created_by=operator`) so an unattended brief can never
  park at `WaitingApproval`.

## Brief contract (mirror of BRIEF_PROMPT)

1. Gather: `life.recall` with `commitments_approaching`
   (`due_within_hours=72`), `open_loops_by_context`,
   `goals_and_next_actions`, `re_entry_context`.
2. Compose ONE Telegram message, skipping empty sections:
   **Due soon** (commitments, soonest first) · **Open loops** (≤5, stalest
   first) · **Goals** (NextActions that `ADVANCES` a goal shown under it) ·
   **Picking back up** (1–2 lines).
3. Plain-language `claim_summary` text only — never raw node ids. Under
   1200 characters. No preamble.
4. Close by inviting reactions (`done / stale / noisy / useful <item>`).
5. On a reaction, file `life.recall.feedback` with the surfacing recall's
   `packet_id`, the indicated rating, and the node in the matching refs
   array — this drives the `recall_utility` EWMA and SIL evidence.
6. All sections empty → one line saying the graph has no active agenda
   items, inviting the operator to add one.

## Known limitation

`goals_and_next_actions` vector-dispatches only `Goal` labels; NextActions
surface via the recency fallback or graph expansion over `ADVANCES` edges
(S2). If NextActions are chronically missing from briefs, widen that
strategy's dispatch labels in `data-memorygraphrag/src/provider.rs`.
