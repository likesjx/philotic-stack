---
title: UAT Scenarios — Whisper Protocol Phase 1+2
doc_type: uat-scenarios
domain: cognitive-plane
status: active
last_updated: 2026-04-02
tags:
- whisper
- paracrine
- uat
- lookaside
- membrane
related_docs:
- WHISPER_PROTOCOL_PROPOSAL.md
proposal_id: whisper-protocol
---

# UAT Scenarios — Whisper Protocol Phase 1+2

## Setup

```bash
# Run against the whisper-protocol worktree directly (no Homebrew push needed)
cd /Users/jaredlikes/code/philotic-stack-whisper-protocol
just uat worktree=.

# OR build and push to the live bjork hotel
just local-push
launchctl kickstart -k gui/$(id -u)/com.philotic.aiua.local-telegram
```

### Log tailing
```bash
tail -f ~/.philotic/bjork/aiua.log | grep -E "paracrine|exosome|whisper|@agent|lookaside"
```

### Philote prep
No manual config needed — `delegate.whisper` is in the default `orchestrator`
and `admin` toolset profiles. A specialist role needs `PHILOTIC_ROLE_NAME` set
in its guest environment (set via `role.configure` or the context graph).

---

## Scenario 1 — Tool call dispatches and returns immediately

**Goal:** Confirm `delegate.whisper` fires as a local agent tool and returns
without blocking the turn.

**Steps:**
1. Start a conversation with the orchestrator
2. Send: `"Use delegate.whisper to ask the theoretician role to explain the CAP theorem. reply_to self."`

**Expected:**
- [ ] Tool result in model re-entry contains `"paracrine dispatched (id: <uuid>)"`
- [ ] Orchestrator's turn completes — no blocking
- [ ] Log shows `paracrine_request` delivered to theoretician inbox
- [ ] `paracrine_id` is a valid UUID

---

## Scenario 2 — paracrine_id logged on the turn

**Goal:** Confirm `associated_paracrine_ids` is populated.

**Steps:**
1. After Scenario 1, ask: `"What paracrine IDs are associated with this turn?"`

**Expected:**
- [ ] Agent can report the UUID from the `delegate.whisper` dispatch
- [ ] ID matches what was in the tool result from Scenario 1

---

## Scenario 3 — Specialist receives and processes the exosome

**Goal:** Confirm `paracrine_request` routes correctly in the specialist philote.

**Steps:**
1. Materialize a specialist with `PHILOTIC_ROLE_NAME=theoretician`
2. Run Scenario 1
3. Watch specialist philote logs

**Expected:**
- [ ] Specialist receives task with `action: "paracrine_request"`
- [ ] Exosome contains `prompt`, `paracrine_id`, and `response_routing`
- [ ] Specialist runs a full model turn against the prompt
- [ ] Specialist's `deliver_text_reply` detects `paracrine_origin` and emits `action: "paracrine_response"`

---

## Scenario 4 — paracrine_response routes through the lookaside reflex

**Goal:** Confirm `handle_paracrine_response` is invoked (not `handle_user_message`).

**Steps:**
1. Run Scenario 3 end-to-end with `reply_to: "self"`

**Expected:**
- [ ] Log shows `handle_paracrine_response` invoked (not default user message path)
- [ ] `paracrine_id` in response matches the dispatched ID
- [ ] Routing variant `CognitiveReEntry` (default) → specialist answer fed into orchestrator's model
- [ ] Orchestrator produces a synthesized reply incorporating the specialist's answer

---

## Scenario 5 — Attribution tag stripped, inline button appears

**Goal:** Confirm the membrane interceptor works end-to-end.

**Steps:**
1. Ask orchestrator: `"Use delegate.whisper to ask the theoretician role for a one-sentence explanation of entropy. reply_to membrane."`

**Expected:**
- [ ] Message appears in Telegram from the theoretician
- [ ] `@agent:theoretician` tag is **not visible** in the message text
- [ ] Inline button `🎭 theoretician` appears below the message
- [ ] Partial/draft messages do NOT show the button (only the final send)

---

## Scenario 6 — Role switch via inline button

**Goal:** Confirm button tap triggers modal role handoff.

**Steps:**
1. Tap the `🎭 theoretician` button from Scenario 5

**Expected:**
- [ ] Telegram sends `callback_data: "/role theoretician"` to membrane
- [ ] Philote receives the command and executes the existing role handoff path
- [ ] Subsequent messages are handled by the theoretician persona
- [ ] Orchestrator can still be reached via `/role orchestrator`

---

## Scenario 7 — RawForward routing

**Goal:** Confirm `routing: "raw_forward"` bypasses the model entirely.

**Steps:**
1. Send: `"Use delegate.whisper with role=theoretician, reply_to=membrane, routing=raw_forward"`

**Expected:**
- [ ] Specialist response appears directly in Telegram
- [ ] No orchestrator model re-entry triggered
- [ ] Attribution tag stripped, button appears

---

## Scenario 8 — Cross-turn paracrine_id provenance

**Goal:** Confirm the paracrine thought graph threads correctly across philotes.

**Steps:**
1. From orchestrator, dispatch two whispers in the same turn — one to theoretician, one to a datasource role
2. Observe both `paracrine_id`s in `associated_paracrine_ids`
3. Observe both responses arrive and route independently

**Expected:**
- [ ] Two distinct UUIDs in `associated_paracrine_ids`
- [ ] Responses arrive independently and route correctly
- [ ] Each paracrine_response carries its originating `paracrine_id`
- [ ] Orchestrator can synthesize from both results

---

## Known gaps / not yet implemented

- `answerCallbackQuery` not called after button tap — Telegram will show loading
  spinner briefly (no functional impact, cosmetic only)
- `EnrichedToolResult` routing path wired but not exercise-tested end-to-end
- Multi-lookaside correlation via `lookaside_id` depth tracking (Phase 3)
- Heartbeat loop pattern (Phase 3)
