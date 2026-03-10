# Agent Loop Proposal

## Goal

Close four concrete gaps in the current `agent-core` loop that prevent it from functioning as a real agentic runtime: the one-shot tool loop, hardcoded media routing, placeholder tool definitions, and coarse approval granularity.

## Disposition

`draft — proposed for next implementation slice`

## Linked Work Surface

[docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md) — Agent Logic section.

---

## Gap 1: Multi-Turn Tool Re-Entry Loop

### Current behavior

After `handle_tool_result()`, `interpret_tool_result()` always returns `Respond { "Tool X says: <content>" }` — the tool result string is sent directly to hegemon. The model never sees it. There is no way for the model to reason about the result, chain another tool call, or change its mind.

### Recommendation

After a tool result is received, re-submit to the model with the full turn context: original prompt + tool call record + tool result record. The model then decides whether to respond, call another tool, or request approval.

Introduce an **iteration budget** per turn (suggested cap: 10 model round-trips). If the budget is exhausted, fail the turn with a clear error.

Concrete shape:

- `WorkingTurn` already tracks `iteration`. Use it as the loop counter.
- `SessionState.recent_turns` stores only completed turns. Add a `working_tool_history` to `WorkingTurn` that accumulates `(tool_call, tool_result)` pairs for the current turn.
- When re-submitting to the model, include the working tool history in the prompt under `[Tool call history]`.
- After model response, apply the same `interpret_model_payload` dispatch. Only `Respond` exits the loop; `ToolCall` continues it; `Fail` aborts.

Deferred from first slice:
- Structured tool history (keep as prompt text initially)
- Streaming intermediate tool results to the user
- Interrupting a running tool loop via a new user message

---

## Gap 2: Configurable Media Routing

### Current behavior

`handle_user_message` hardcodes the branch: if any blob-backed attachments of kind `photo/image/voice/audio/document` are present, use `analyze_media` action and strip tools. Otherwise use `generate_text`. This is not overridable per agent or per session.

### Recommendation

Add a `MediaRoutingPolicy` to `AgentProfile`:

```rust
pub struct MediaRoutingPolicy {
    pub forward_media_to_model: bool,          // default: true
    pub voice_action: Option<String>,          // "analyze_media" | "transcribe" | custom
    pub image_action: Option<String>,          // "analyze_media" | "describe" | custom
    pub document_action: Option<String>,       // "analyze_media" | "summarize" | custom
    pub strip_tools_on_media: bool,            // default: true
}
```

- When `forward_media_to_model` is false, treat all messages as text-only (ignore attachments for model routing).
- `voice_action`, `image_action`, `document_action` allow per-kind action overrides.
- `strip_tools_on_media` controls whether tools are suppressed for media turns (current default behavior).

This is the slot where voice transcription-first routing (ElevenLabs or Whisper) will plug in when the voice machine is ready.

Deferred from first slice:
- Per-session media policy override via `/media` slash command
- Multi-provider routing (e.g., send voice to ElevenLabs, images to Gemini)

---

## Gap 3: Real Tool Definitions

### Current behavior

`default_tool_assembly_for_bindings` generates every tool as:
```rust
ToolDefinition {
    tool_name: tool_name.clone(),
    description: format!("Execute the {} tool.", tool_name),
    input_schema: json!({ "type": "object" }),
}
```

The model has no real schema or description to reason about. Tool selection is heuristic string matching in `project_tools_for_turn`, not model-guided.

### Recommendation

Define a **static tool catalog** in `agent-core` with proper definitions for all currently known tools:

| Tool | Description | Schema |
|---|---|---|
| `session.status` | Returns the current session state summary. | No args |
| `echo` | Echoes a string back. Use for testing. | `{ text: string }` |
| `workspace.list` | Lists files and directories in the workspace. | `{ path?: string }` |
| `workspace.read` | Reads a file from the workspace. | `{ path: string, offset?: u64, limit?: u64 }` |

The catalog should be a `fn tool_catalog() -> HashMap<&str, ToolDefinition>` function that `default_tool_assembly_for_bindings` calls instead of generating stubs.

Add a `class` field to `ToolDefinition` for approval and projection purposes (e.g., `"session"`, `"workspace"`, `"utility"`, `"capability"`).

Deferred from first slice:
- Hotel-provided tool catalog (remote definitions via IPC snapshot)
- Dynamic tool registration by tool-runner guests
- Per-tool usage examples in the model prompt

---

## Gap 4: Approval Granularity

### Current behavior

`approval_policy_allows()` only checks `auto_approve_all`. The `preapproved_tools` and `preapproved_classes` fields in `ApprovalPolicy` are declared but never evaluated.

`/approve` and `/deny` with steering notes record the note but never inject it back into the model prompt.

### Recommendation

**4a — Implement `preapproved_tools` and `preapproved_classes`:**

```rust
pub fn approval_policy_allows(&self, approval: &ApprovalRequest, tool: Option<&ToolCall>) -> bool {
    if self.approval_policy.auto_approve_all { return true; }

    if let Some(tool) = tool {
        if self.approval_policy.preapproved_tools.contains(&tool.tool_name) {
            return true;
        }
        if let Some(class) = tool_class(&tool.tool_name) {
            if self.approval_policy.preapproved_classes.contains(class) {
                return true;
            }
        }
    }
    false
}
```

`tool_class` derives the class from the tool catalog's `class` field.

**4b — Inject steering note on `/approve` and `/deny`:**

When the approval command carries a note (e.g., `/approve use the staging environment`), re-submit to the model with:
- The original approved_response text as the starting point
- A `[Operator steering]` section appended: `Operator approved with note: use the staging environment.`

For `/deny` with a note, re-submit as a new model turn with the deny note as operator guidance, allowing the model to produce an alternative response.

For `/deny` without a note, fail the turn as today.

**4c — Surface approval request IDs:**

When `request_approval` action includes an `approval_id`, display it to the user so they can reference it in `/approve <id>` or `/deny <id>` for multi-pending-approval scenarios.

Deferred from first slice:
- `/approve <id>` and `/deny <id>` syntax for selecting among multiple pending approvals
- Approval expiry / timeout
- Approval escalation to a supervisor agent

---

## Implementation Order

1. **Gap 3** (tool catalog) first — required for Gap 4 (class-based approval) and for tool projection quality.
2. **Gap 4** (approval) — unblocked after Gap 3.
3. **Gap 1** (re-entry loop) — can land independently; requires `working_tool_history` added to `WorkingTurn`.
4. **Gap 2** (media routing) — can land independently by adding `MediaRoutingPolicy` to `AgentProfile`.

## Open Questions

- Should the iteration cap be per-session configurable, or a hard compile-time constant?
- Should working tool history be preserved across approval interrupts (i.e., if approval is requested mid-loop, does the tool history survive)?
- Should the tool catalog live entirely in `agent-core` or be provided partly by the hotel IPC snapshot (so hotel can inject custom tool definitions from tool-runner guests)?
