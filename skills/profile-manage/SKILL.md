---
name: profile-manage
description: How an agent understands its own toolset profile, grows its capability set over time, and requests or registers new skills for its roles. The self-directed capability growth loop.
catalog:
  skill_name: profile.manage
  implied_tools:
    - role.list
    - skill.list
    - skill.register
    - skill.assign
    - role.configure
    - session.status
  validation_state: validated
  skill_markers:
    - governed
    - self_directed
  field_sources:
    required_fields:
      - role_name
    optional_fields:
      - skill_name
      - proposed_expansion
    repo_skill_path: skills/profile-manage/SKILL.md
    workflow: "role.list → skill.list → identify gap → register or request → assign"
---

# Profile Management

Use this skill to understand, audit, and grow your own capability profile. An agent's capability is determined by its toolset profile (which tools are allowed) and its skill assignments (which delegation patterns it has learned). Both can be expanded within the bounds the operator has set.

## Understanding your profile

Every role has a `toolset_profile` that determines:
- **`allowed_tools`**: explicit tool IDs the role may call
- **`allowed_classes`**: tool class names allowed (e.g. `graph`, `table`, `workspace`)
- **`allowed_skills`**: named skills assigned to the role

Run `role.list` to see your current roles and their profiles. Run `skill.list` to see which skills are currently assigned.

## The capability growth loop

```
Identify a recurring pattern you've delegated 3+ times
  → Does a skill already exist? (skill.list)
    → Yes: assign it to your role with skill.assign
    → No: author it with skill.register + skill.assign (uses skill.authoring)

Identify a tool your role needs but doesn't have
  → Is it in your allowed_tools? (role.list)
    → Yes: you can call it — no action needed
    → No: use capability.request to ask the orchestrator to expand your profile
```

## Authoring a new skill for your role

When you've identified a pattern worth registering as a skill:

1. Call `skill.list` to confirm the skill doesn't already exist under a different name
2. Call `skill.register` with:
   - `skill_name`: short slug, lowercase, hyphens — e.g. `"data-summarizer"`
   - `description`: what the skill does and when to use it (1-3 sentences)
   - `subagent_kind`: almost always `"philote-worker"`
   - `goal`: the goal template the subagent receives; use `{{placeholder}}` for variables
   - `allowed_tools` and/or `allowed_classes`: narrow the subagent's tool surface to exactly what it needs
3. Call `skill.assign` with your `role_name` and the new `skill_name`

Skills persist in the hotel database across sessions. They accumulate as your learned delegation vocabulary.

## Requesting a profile expansion

If you need a tool that isn't in your `allowed_tools` and isn't in your profile's `allowed_classes`, you cannot add it yourself — toolset profiles are operator-controlled. Use the `capability.request` skill to request it:

1. Document what you need and why
2. Call `handoff.back` to the orchestrator with the request formatted as:
   ```
   CAPABILITY REQUEST: I need [tool or skill name] to [reason].
   Suggested expansion: { tool_names: [...] or skill_name: "..." }
   Please add to my role '[role_name]' and return me to this session.
   ```
3. The orchestrator will update your profile and hand you back

## Auditing your profile

To verify your profile is correct for the work you're doing:

1. Call `role.list` and inspect `toolset_profile`, `allowed_tools`, `allowed_skills`
2. Call `skill.list` and verify all assigned skills are relevant to your current responsibilities
3. If a skill is stale or no longer relevant, surface it to the operator — skills cannot be self-revoked (only admin roles have `skill.revoke`)

## When to update the role manifest

Your role's `role_identity_addendum` (set via `role.configure`) is your persistent self-description — it tells the model who you are in this role. Update it when:
- Your responsibilities have materially changed
- You've registered several new skills that define a new capability area
- The operator has explicitly asked you to document your current posture

Use `role.configure` with `role_identity_addendum` to update. This does NOT change the toolset profile — it only updates the identity text.

## Guardrails

- Do not register skills for one-off tasks — skills are for patterns you've seen 3+ times
- Do not attempt to modify `allowed_tools` directly — use `capability.request`
- Skill names are permanent once registered — choose deliberately
- Do not assign skills to other agents' roles — `skill.assign` is scoped to your own agent
