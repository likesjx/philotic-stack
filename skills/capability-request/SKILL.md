---
name: capability-request
description: Use this skill when a specialist role needs a tool or capability it does not currently have. Hands back to the orchestrator with a structured request so the orchestrator can register and assign the skill, then return the role to continue its work.
catalog:
  skill_name: capability.request
  implied_tools:
    - skill.list
    - handoff.back
  validation_state: validated
  skill_markers:
    - governed
  field_sources:
    repo_skill_path: skills/capability-request/SKILL.md
    workflow: "skill.list → handoff.back with structured request → orchestrator registers + assigns → handoff.to_role returns"
---

# Capability Request

Use this skill when you are in a specialist role and need a capability that is not currently in your toolset.

## Workflow

1. Call `skill.list` to confirm the capability does not already exist in the catalog
2. Draft a structured capability request (see format below)
3. Call `handoff.back` with the structured request as the `summary`
4. The orchestrator will register the skill, assign it to your role, and return you here via `handoff.to_role`

## Request format

Your `summary` in `handoff.back` must follow this structure:

```
CAPABILITY REQUEST: I need [capability name] to [reason].

Suggested skill:
- skill_name: [lowercase-kebab-name]
- description: [what it does, 1-2 sentences]
- subagent_kind: philote-worker
- goal: [the goal template to inject into the subagent]
- allowed_tools: [optional list]
- allowed_classes: [optional list]

Please register and assign to my role '[your current role_name]', then return me to this session.
```

The more complete your suggested skill definition, the faster the orchestrator can act. If you don't know what tool the subagent needs, describe the task — the orchestrator will decide the tool surface.

## Example

```
CAPABILITY REQUEST: I need iMessage read access to check for forwarded alerts from an automated sender.

Suggested skill:
- skill_name: imessage-read
- description: Query the local iMessage database for recent messages from a specified sender handle.
- subagent_kind: philote-worker
- goal: Check ~/Library/Messages/chat.db for messages from sender {{handle}} in the last {{hours}} hours using bash.exec. Return all found messages with timestamps.
- allowed_classes: shell

Please register and assign to my role 'hermes-comm-specialist', then return me to this session.
```

## Guardrails

- Always check `skill.list` first — don't request something that already exists
- Be specific about what you need and why — vague requests slow the orchestrator down
- If the capability requires shell access (`bash.exec`), say so explicitly in the request
- Trust the orchestrator to scope the tool surface appropriately — you don't have to get it perfect
