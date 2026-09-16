---
name: skill-authoring
description: Use this skill when an agent wants to codify a recurring delegation pattern as a named, reusable skill and assign it to their own role.
catalog:
  skill_name: skill.authoring
  implied_tools:
    - skill.list
    - skill.register
    - skill.assign
    - skill.revoke
    - skill.set_state
    - skill.audit
  validation_state: validated
  skill_markers:
    - governed
    - self_directed
  field_sources:
    required_fields:
      - skill_name
      - description
      - subagent_kind
      - goal
    optional_fields:
      - allowed_tools
      - allowed_classes
      - allowed_skills
    repo_skill_path: skills/skill-authoring/SKILL.md
    workflow: "skill.list → skill.register → skill.assign"
---

# Skill Authoring

Use this skill when you have identified a recurring delegation pattern in your work and want to register it as a named, reusable skill that you can assign to your roles.

## When to create a skill

- You have delegated the same kind of task 3 or more times
- The task has a clear goal template with predictable inputs
- The subagent needs a specific, bounded tool surface to do the work
- You want the pattern to persist across sessions as part of your delegation vocabulary

Do not create a skill for a one-off task. Skills are for patterns.

## Workflow

1. Call `skill.list` to check if a similar skill already exists
2. Draft the skill payload (see fields below)
3. Call `skill.register` with the full payload
4. Call `skill.assign` with the `role_name` of the role that should have access to the skill

## Required `skill.register` fields

| Field | Required | Notes |
|---|---|---|
| `skill_name` | Yes | Lowercase, dots, hyphens, underscores; max 64 chars. Stable identifier (house style is dotted, e.g. `research.deep-dive`). |
| `description` | Yes | What the skill does and when to use it. 1-3 sentences. |
| `subagent_kind` | Yes | The worker role that executes this skill (almost always `philote-worker`). |
| `goal` | Yes | The goal template injected into the subagent at invocation. May include `{{placeholder}}` variables. |
| `allowed_tools` | No | Explicit tool IDs the subagent may use. If omitted, falls back to class allowances. |
| `allowed_classes` | No | Tool class names allowed (e.g. `workspace`, `utility`, `shell`). |
| `allowed_skills` | No | SkillDAG edges: names of already-registered skills this skill depends on. Activating the skill transitively activates its dependencies. |

## Invoking a registered skill

Spawn a registered delegation skill by name — the hotel resolves the stored
goal template, worker kind, tool bounds, and SkillDAG dependencies:

```json
{
  "tool": "subagent.spawn",
  "skill_name": "research.deep-dive",
  "inputs": { "topic": "hotel supervision" },
  "goal": "optional extra context appended to the template"
}
```

`inputs` fills the template's `{{placeholder}}`s. Suspended/deprecated skills
are refused at spawn. An explicit `goal` is only required when no `skill_name`
is given.

## Lifecycle and audit

Registered skills are living records, administered by the orchestrator:

- Re-running `skill.register` with the same `skill_name` updates the record
  (audited as `update`).
- `skill.set_state` moves a skill between `active`, `suspended`, and
  `deprecated`. Suspended/deprecated skills stop contributing tools and
  guidance to sessions on their next snapshot; `active` reinstates them.
- `skill.audit` reads the append-only administration trail: every register,
  update, assign, revoke, and state change, with who and when.

## Minimal example

```json
{
  "skill_name": "file-summarizer",
  "description": "Summarize a workspace file into bullet points for quick review.",
  "subagent_kind": "philote-worker",
  "goal": "Read the file at {{file_path}} and return a structured bullet-point summary of its key content."
}
```

## Full example

```json
{
  "skill_name": "deep-search",
  "description": "Delegate a multi-source research task to a focused subagent. Use when a topic requires reading multiple workspace files or documents before synthesizing an answer.",
  "subagent_kind": "philote-worker",
  "goal": "Research {{topic}} thoroughly. Read all relevant files in the workspace. Return a structured summary with: key findings, open questions, and recommended next steps.",
  "allowed_tools": ["workspace.read", "workspace.list"],
  "allowed_classes": ["workspace", "utility"]
}
```

## Assigning after registration

After `skill.register` succeeds, assign the skill to your current role:

```json
{
  "role_name": "orchestrator",
  "skill_name": "deep-search"
}
```

`skill.assign` is scoped to your own agent — you cannot assign skills to other agents' roles.

## Guardrails

- Do not create skills that require shell access unless you have operator approval for `bash.exec` in your role
- Prefer narrow `allowed_tools` lists — subagents should not have more authority than the task requires
- Name skills for what they accomplish, not how they work: `code-reviewer` not `reads-files-and-gives-feedback`
- Skills are permanent once registered — choose names deliberately
