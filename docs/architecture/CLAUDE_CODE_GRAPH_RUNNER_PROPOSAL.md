---
title: Claude Code Graph Task Runner Proposal
doc_type: proposal
domain: agent-coordination
status: proposed
last_updated: 2026-11-21
tags:
- intel-graph
- claude-code
- agent-coordination
- runner
- evidence-gated
related_docs:
- ../../skills/graph-task-runner/SKILL.md
- ../../skills/graph-intelligence/SKILL.md
- ../../skills/session-hygiene/SKILL.md
- RUNTIME_AUTHORITY_LEASES_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: claude-code-graph-runner
implements: []
implemented_by: []
active_seams:
- claude-code-graph-runner
source_of_truth_targets:
- ../../skills/graph-task-runner/SKILL.md
---

# Claude Code Graph Task Runner

## Problem

Claude Code on the Mac already has the IntelliGraph MCP (`.mcp.json` →
`intel-graph`) and a managed harness (`claude-local`), but it only works the
ledger when someone opens a session and asks. Tasks that Priti or Codex assign
to `claude-local` sit until then. The operator wants Claude Code to pick up its
own graph engineering tasks automatically.

Three facts from the code shape the design:

1. `graph_next_task` (`egress.rs::next_task`) scores proposals and seams only.
   It ignores `task` nodes and has no owner filter, so it cannot answer "what
   is mine to do?".
2. On loopback the graph has no auth, and with `PHILOTIC_GRAPH_TOKEN` set
   there is one shared token. There is no per-agent write scope on the server.
   Least privilege has to be enforced on the client side.
3. MCP `graph_update_node` records no agent; REST `/api/nodes/:id/update`
   does. Most ledger mutations today carry no agent name.

## Design

A deterministic runner owns the loop and every ledger write. Claude Code does
the code in between and only reads the graph.

```
launchd (every 30 min, off until enable switch)
  └─ scripts/claude_graph_runner.py once
       1. select   GET task nodes; keep owner=claude-local, status open,
                   autorun=true, acceptance_cmd in allowlist, no active
                   session, no open `blocks` edge. Rank by priority.
       2. claim    POST update {status: in_progress, claim_session}
                   + read-back; POST /api/session/start (agent=claude-local)
       3. isolate  git fetch origin develop; worktree ../philotic-stack-claude-<slug>
                   on branch claude/ig-<slug>
       4. work     claude -p … --permission-mode dontAsk
                     --settings .claude/runner/settings.json
                     --mcp-config .claude/runner/mcp.json --strict-mcp-config
                     --setting-sources project
       5. verify   runner re-runs acceptance_cmd itself (does not trust the
                   model's claim)
       6. report   POST /api/test-run, update task {status, receipt, blocker},
                   POST /api/decide (with evidence), POST /api/session/close
       7. stop     status in_review (commits + all green) or blocked (reason)
operator
  └─ review the worktree → just claude-runner-publish <task> → draft PR → merge
```

### Identity

The existing harness id `claude-local` is the agent. Every runner write goes
through REST with `agent: claude-local` and the runner session id, so the
mutation log finally shows who did what. Tasks are assigned the existing way:
an `owner` property on the task (the graph rejects `assigned_to` edges).

### What makes a task runnable (the evidence gate)

| Property | Required | Why |
|---|---|---|
| `owner: claude-local` | yes | ownership convention already in use |
| `status: open` | yes | `ready`/`todo` also accepted |
| `autorun: true` | yes | per-task operator opt-in; nothing runs by default |
| `acceptance_cmd` | yes | no tests, no autonomous run; the result is gated on it |
| `seam_id`, `proposal_id` | no | linked into the session and context |
| `base_ref` | no | defaults to `develop` |

`acceptance_cmd` is executed by the runner, and several agents can write the
graph, so it is a parse-checked allowlist rather than a shell string:
`just check|test|build|format|test-suite`, `cargo test|check|clippy|build -p
<crate> [args]`, `python3 [-B] -m unittest [args]`, with arguments limited to
`[A-Za-z0-9_./:=-]`. Anything else makes the task ineligible (and the dry run
says why).

### Least privilege for the session

| Surface | Session gets | Enforced by |
|---|---|---|
| Graph | read tools + `session_activity` on its own session only | runner settings allow/deny + `guard-graph-writes.sh` (fails closed) |
| Ledger writes | none; the runner writes from verifiable facts | deny list + hook |
| MCP servers | `intel-graph` only (no Muninn, no public `philotic` endpoint) | `--strict-mcp-config` |
| Settings | project settings only (not the 872-grant local file or user file) | `--setting-sources project` |
| Git | add/commit/diff/log in its worktree | allow list; push/merge/rebase/checkout/worktree/remote denied, plus existing `guard-destructive-git.sh` |
| Network, deploy, secrets | none: `curl`, `ssh`, `gh`, `ansible*`, `phil`, WebFetch denied; `~/.ssh`, `~/.config`, `~/.philotic`, vault and keys unreadable | deny list |

`cargo test` and `just test` still execute repository code, which is inherent
to doing engineering work; the worktree, the deny list and the no-push rule
bound what that can reach.

### Approval boundaries

- Nothing leaves the Mac during a run. Pushing is `publish`, an operator-run
  command that refuses to run inside a runner session.
- Merges, deploys, secrets and production stay with the operator; the runner
  has no path to any of them.
- On/off: launchd job installed with `RunAtLoad=false`, and each run exits
  unless `~/.config/philotic/claude-graph-runner.enabled` exists. Deleting the
  file is the kill switch. One run at a time (lock dir), 45 min session
  timeout, 80 turns.

### Receipt

Written to the task as `receipt` and to `~/.philotic/claude-graph-runner/<stamp>/`:
session, branch, base/head sha, commit count, diffstat, uncommitted files,
each acceptance command with exit code and duration, the model's own
outcome/summary, turns, cost, and local log paths. A `TestRun` node links the
acceptance result to the task; a decision records the status change with the
head sha and branch as evidence.

## How this fits existing work

- **graph-intelligence workflow** (`skills/graph-intelligence`): same session
  lifecycle (start → activity → close) and decision recording, done by the
  runner instead of trusting each session to remember.
- **Priti's evidence-gated factory and graph-engineering proposals** (graph
  nodes, not in the repo): `autorun` + `acceptance_cmd` are this runner's gate.
  The property names are a placeholder until they are mapped to the factory's
  own gate fields; see open questions.
- **Hotel mailbox** (`codex/hotel-project-mailbox`, not on GitHub at the time
  of writing): out of scope here. Notification today is the task status
  change; routing `in_review`/`blocked` through the mailbox is a follow-up.
- **local_work.rs leases/fences**: the runner's claim is a status property plus
  read-back, sufficient for one runner per agent. If more runners appear, the
  claim should move to the lease/fence protocol.
- **Munin key translation**: not used. The runner adds no credentials, no key
  vault and no remote access; it talks to the loopback graph only.

## Verification

- `scripts/tests/test_claude_graph_runner.py` (hermetic, runs in CI): stub
  graph REST server, throwaway git origin, fake `claude` binary. Covers gate
  logic, acceptance allowlist, dry run claims nothing, disabled switch claims
  nothing, happy path → `in_review` with receipt/test-run/decision/close all
  stamped `claude-local` and nothing pushed, failing acceptance → `blocked`.
- Not verified here: the real graph server, the real `claude` CLI flags on the
  operator's installed version (`dontAsk`, `--setting-sources`), launchd.
  `just claude-runner-doctor` and `just claude-runner-list` are the first
  checks on the Mac.

## Open questions

1. Gate field names: adopt Priti's factory fields instead of `autorun` /
   `acceptance_cmd`?
2. Should `graph_next_task` learn `task` nodes and an `agent` filter so every
   harness gets the same selection logic server-side? (Small Rust change; left
   out to keep this PR client-only.)
3. Branch prefix `claude/ig-<slug>` vs the repo's `codex/<slug>` convention.
4. Muninn in runner sessions: excluded for now while memory moves to the new
   graph-native store.
5. Who sets `autorun: true` on a task: operator only, or Priti/Codex too?
