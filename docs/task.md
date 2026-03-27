# Ansible Mesh Architecture Separation Task List

- [x] Inspect the `openclaw-plugin-ansible` repository and identify the rust port branch.
- [x] Understand the current architecture of the ansible mesh.
- [x] Design the separation of the stack into independently addressable units:
  - [x] chat/communication (including telegram)
  - [x] agent loop + context + session
  - [x] memory
  - [x] models
  - [x] mcp/tools
  - [x] context graph
- [x] Review code for communication and split points.
- [x] Create an implementation plan for the new services architecture.

## Current Work Item Split

Stable seam refs live in [SEAM_REGISTRY.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SEAM_REGISTRY.md).

### WI 1: Session Management

Seam IDs: `session-leases`

- [x] Decide that session state has one canonical home in the Context Graph; apartment checkpoints are derived recovery projections, not a second source of truth.
- [x] Generalize session as a cross-component coordination envelope rather than an agent-only transcript.
- [x] Add graph-modeled session entities for session lifecycle, participants, and turns.
- [x] Bind transport identities in `membrane` to stable `session_id` values.
- [ ] Add session leases / ownership semantics for active work.
- [x] Persist session timeline/progress events while keeping the IPC plane general.
- [x] Support recovery flows at the session layer.
- [x] Support approval flows at the session layer.
- [ ] Define the canonical compact session envelope so session compaction targets:
  - live status and pending work
  - active role/incarnation
  - bounded local working memory
  - bounded recent-turn/control window
  - compact session-local facts only
- [ ] Define the first `CompactSessionEnvelope` shape:
  - `session_identity`
  - `live_status`
  - `active_role`
  - `working_state`
  - `recent_local_window`
  - `session_facts`
  - `checkpoint_metadata`
- [ ] Explicitly keep durable continuity out of the session envelope by default:
  - autobiographical memory
  - durable user relationship memory
  - general long-term topic memory
  - transcript archives

### WI 2: Agent Logic

Seam IDs: `session-compaction`

- [ ] Implement the bounded ZeroClaw-style loop in `philote`.
- [ ] Build context from session snapshot plus memory apartments.
- [ ] Execute tools with approval-aware flow control.
- [x] Keep local working turn state in the agent during execution.
- [x] Use `SyncApartment` as periodic derived snapshot/checkpoint sync back to the Context Graph, not as canonical session ownership.
- [ ] Add compaction/checkpoint policy so apartment sync stays structured and reasonably small.
- [ ] Make compaction preserve live session actuality rather than transcript bulk:
  - active commitments
  - unresolved tool/approval state
  - role-local working state still in play
  - smallest recent-turn window needed for coherence
  - session-local facts still live in this session
- [ ] Make compaction target `CompactSessionEnvelope` explicitly instead of freeform transcript summaries.
- [x] Add slash-command short-circuiting for deterministic agent/system commands before the normal model loop.
- [x] Add approval interrupts with explicit history and a pre-approval runtime path.
- [x] Extend the shared cross-component task error envelope beyond the current model/TTS path so tool-runner, membrane, and other routed components return structured failures instead of silent fallback strings.

## New Project: Agent Loop Gap Closure

- [x] Review [AGENT_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_LOOP_PROPOSAL.md).
- [x] Build real tool catalog with proper descriptions and schemas (Gap 3 — prerequisite for Gap 4).
  - [x] Add `class: Option<String>` field to `ToolDefinition` in `philote`.
  - [x] Create static `tool_catalog()` in `agent-core/src/catalog.rs` with real descriptions and schemas for `session.status`, `echo`, `workspace.list`, `workspace.read`.
  - [x] Add `AbstractToolRecord` to `ansible-mesh-core/src/graph.rs` as the context graph entity.
  - [x] Add `upsert_abstract_tool` / `get_abstract_tool` / `list_abstract_tools` to `GraphStorage` trait and `SqliteGraphStorage` impl.
  - [x] Seed catalog into context graph at hotel startup via `seed_abstract_tool_catalog`.
  - [x] Update both `default_tool_assembly_for_bindings` and `tool_assembly_from_allowed_incarnations` to look up from catalog before falling back to stubs.
- [x] Implement `preapproved_tools` and `preapproved_classes` evaluation in `approval_policy_allows` (Gap 4a).
  - [x] `tool_class()` and `tool_requires_approval()` added to `catalog.rs`.
  - [x] `approval_policy_allows` evaluates all three conditions in order: `auto_approve_all`, `preapproved_tools`, `preapproved_classes`.
  - [x] `handle_tool_call` enforces `approval_required` from `ToolPolicyAnnotation` before dispatch — agent-level gate independent of model intent.
  - [x] Policy annotations derive `policy_class` and `approval_required` from catalog.
  - [x] `agent.configure` tool: class `"config"`, `approval_required: true`. Handles `approval_policy.*`, `profile.*`, `bindings.*` paths with set/append/remove. Rebuilds tool assembly when bindings change.
- [x] Inject operator steering notes from `/approve`/`/deny` back into the model prompt (Gap 4b).
  - [x] `/approve <note>` re-submits to model with `[User approval steering]` prefix appended to turn context.
  - [x] `/deny <note>` re-submits as a new model turn with `[User denied the proposed action. Do this instead]` prefix.
  - [x] `/deny` without note fails the turn (existing behavior preserved).
  - [x] `resume_turn_with_steering` appends the note to `active_turn.user_content`, increments iteration, rebuilds prompt, re-submits to model.
- [x] Add `working_tool_history` to `WorkingTurn` and implement multi-turn tool re-entry loop with iteration cap (Gap 1).
  - [x] `working_tool_history: Vec<(ToolCall, ToolResult)>` added to `WorkingTurn`; serialized into checkpoint and rehydrated from it.
  - [x] `push_tool_history` and `build_reentry_prompt` added to `SessionState`.
  - [x] `build_reentry_prompt` appends `[Tool call history]` section with numbered call/result pairs; directs model to continue or respond.
  - [x] `handle_tool_result` rewritten: pairs result with `pending_tool_call`, pushes to history, checks `MAX_TOOL_ITERATIONS` (10), increments `iteration`, re-submits `generate_text` to model. Loop exits only on `Respond`, `Fail`, `RequestApproval`, or iteration cap.
  - [x] `interpret_tool_result` (old early-close stub) no longer called.
- [x] Add `MediaRoutingPolicy` to `AgentProfile` and make media action selection configurable per agent (Gap 2).

## New Project: Agent Incarnation Model

Model revised: same-self role incarnations now default to context shifts with shared durable memory, while spawned subagents remain the default path for delegated parallel labor. See [AGENT_INCARNATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_INCARNATION_PROPOSAL.md) and [ROLE_CONTEXT_SHIFT_AND_DELEGATED_SUBAGENTS_WHITEPAPER.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROLE_CONTEXT_SHIFT_AND_DELEGATED_SUBAGENTS_WHITEPAPER.md).

Seam IDs: `role-incarnation-records`, `active-membrane-routing`, `handoff-skill`

### Skill Catalog + Toolset Profiles (prerequisite for role provisioning)
- [x] Add `AbstractSkillRecord` to `ansible-mesh-core/src/graph.rs` (parallel to `AbstractToolRecord`).
- [x] Add `upsert_abstract_skill` / `get_abstract_skill` / `list_abstract_skills` to `GraphStorage` trait and `SqliteGraphStorage`.
- [x] Add `ToolsetProfileRecord` to the context graph (`toolset_profile` node kind).
- [x] Add `upsert_toolset_profile` / `get_toolset_profile` / `list_toolset_profiles` to `GraphStorage` trait and `SqliteGraphStorage` impl.
- [x] Seed the first built-in handoff/governance abstract skills at hotel startup.
- [x] Seed built-in toolset profiles at hotel startup: `orchestrator`, `codex`, `research`, `utility` via `seed_toolset_profiles`.
- [x] Update session binding assembly to expand skill grants into `implied_tools` when building `tools_for_model`.

### Role Incarnation Records
- [x] Add `RoleIncarnationRecord` and `TurnLoopConfig` to the context graph (`role_incarnation` node kind).
- [x] Add `upsert_role_incarnation` / `get_role_incarnation` / `list_role_incarnations` to `GraphStorage`.
- [x] Add `ConfigureRole` IPC action (orchestrator → hotel); hotel enforces orchestrator-only writes for the same agent identity.
- [x] Define the first rigid orchestrator-only role-governance workflow skill for create/update, including required reasoning about purpose, toolset, skillset, handoff posture, and limits.
- [x] Seed session bindings from the role's `toolset_profile` when a role incarnation session is initialized.
- [ ] Define the canonical shared-self role contract:
  - base identity and durable memory remain shared
  - role addendum is additive
  - working memory is role-local
  - effective toolset and skillset are role-scoped overlays
- [x] Define the first compatibility-first `RoleActivation` contract and thread it through hotel snapshot -> `SessionState` -> context projection:
  - activation reason
  - requested_by
  - role addendum
  - toolset profile reference
  - effective skillset
  - working memory policy
  - memory projection policy
- [x] Expand `RoleActivation` beyond the first compatibility slice:
  - base identity reference
  - explicit skillset profile reference
  - richer activation requester semantics
  - tighter role activation policy ownership

### Active Membrane Routing
- [x] Add `active_incarnation_id` to `SessionRecord` in the Context Graph.
- [x] Update IpcServer task routing to read `active_incarnation_id` before routing inbound agent tasks.
- [x] Default to orchestrator incarnation if active ID is unregistered.
- [x] Park inbound agent tasks and request on-demand materialization when a configured active role is missing locally.
- [ ] Buffer inbound during explicit handoff/materialization before switching active route ownership.
- [ ] Define when same-identity handoff should activate a role in-place versus waking or materializing a separate role process.

### Handoff Skill + Membrane Switching
- [x] Implement `HandoffToRole { role_name, handoff_bundle }` and `HandoffBack { summary, return_to? }` IPC actions.
- [x] Define the first generic orchestrator-owned `handoff.to_role` workflow skill: trigger patterns, target-role selection, context bundle assembly, role-local cleanup steps, return conditions, and context-shift semantics.
- [x] Decide what role metadata the generic handoff workflow reads so we do not regress into per-role bespoke skill-pair manifests unless the generic approach proves too weak.
- [x] Define the first compatibility-first `SameIdentityHandoffPacket` through the existing `HandoffBundle` wire path:
  - handoff_reason
  - active_goal
  - active_constraints
  - relevant_session_facts
  - working_summary
  - suggested_memory_refs
  - expected_return_mode
  - cleanup_actions
- [ ] Expand `SameIdentityHandoffPacket` into its fuller contract:
  - from_role / to_role
  - tighter session-fact ownership
  - workflow-owned assembly rules
  - explicit return-mode semantics beyond the current compatibility slice
- [x] Add `/role <name>` and `/back` slash commands for manual membrane switching.
- [x] Add `/roles` or equivalent status surface so operators can inspect configured roles and the active routed incarnation without guessing.

### Governed Workflow Skills
- [x] Write [GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md).
- [x] Define the first `WorkflowSkillRecord` boundary and decide when it should supersede plain `AbstractSkillRecord` for governed flows (added to `ansible-mesh-core` graph/storage traits).
- [x] Specify target-boundary classes and rules for:
  - same-agent role handoff / context shift (`handoff.to_role` updated with required `target_focus_framing`)
  - peer Philotic agent delegation (added `delegate.to_peer`)
  - external cognitive peer handoff (added `delegate.to_external_cognitive_peer`)
- [x] Define bounded context packaging and return contracts for peer/external workflows so they do not quietly inherit same-identity handoff assumptions.

### Inactive TTL + On-Demand Rematerialization
- [x] Add `inactive_ttl_seconds` to `RoleIncarnationRecord`.
- [x] Extend supervisor loop TTL check: reclaim inactive non-membrane-owner role processes after TTL.
- [ ] On rematerialization: hotel sends session snapshot to restore working memory from Tier 2.
- [ ] Keep concurrent role materialization explicitly conditional; do not let TTL/rematerialization policy silently become the default role ontology.

### Workers / Subagents
- [x] Implement `SpawnSubagent` execution and async result routing back to parent incarnation (Block D — lease + hook registries; `SpawnSubagentOk`/`SpawnSubagentProposal` responses).
- [x] Thread the first compatibility-first `SpawnSubagent` request boundary through shared IPC with explicit structured `SUBAGENT_NOT_IMPLEMENTED` hotel rejection.
- [x] Split `philote` into lib + `philote` (persona) + `philote-worker` (subagent worker) binaries; define `AgentDriver` trait (Block E).
- [x] Add `skill.register` IPC handler in hotel (`RegisterSkill` → Layer 1 validate → `AbstractSkillRecord` persist → `SkillRegistered` response) (Block F).
- [x] Add `skill.register` and `subagent.spawn` tools to abstract tool catalog + `is_local_agent_tool()` + `execute_local_agent_tool()` IPC dispatch (Block F).
- [x] Fix subagent spawn path: add `PHILOTIC_AGENT_ID` to worker env in `config_json`; add `set_guest_active` to `GraphStorage`; deactivate guest on `ReleaseSubagent` to stop supervisor respawn loop. (`PHILOTIC_AGENT_MODE=subagent` superseded by the `philote-worker` binary split in Block E.)
- [x] Add `/abandon` slash command; fires `FireSubagentHook(TurnCompleted, completed=false)` when `PHILOTIC_PARENT_GUEST_ID` is set; handles all 4 match sites in runtime.rs.
- [x] Define the first compatibility-first `SubagentDelegation` contract and parent-side builder:
  - parent role
  - goal
  - context packet
  - allowed tools
  - allowed skills
  - memory allowance
  - write-back allowance
  - iteration budget
  - ttl
  - completion contract
- [ ] Expand `SubagentDelegation` beyond the compatibility-first slice:
  - explicit parent turn/task provenance
  - workflow-owned context-packet assembly rules
  - richer completion artifact semantics
  - tighter policy ownership for memory and write-back rights
- [ ] Define delegation policy inputs that determine spawned subagent tool access, skill access, memory allowance, and write-back rights.
- [ ] Make subagents explicitly lightweight by default:
  - bounded mission packet
  - no direct membrane ownership
  - little or no durable memory by default
  - report-back contract to the parent role

### Memory
- [ ] Add `session_facts` apartment type and `UpdateMemory` IPC with hotel-side rate/size enforcement.
- [ ] Add Muninn tool surface (`memory.search`, `memory.store`) as hotel-mediated tools with auto-injection into prompt context.
- [ ] Add `/memory show` and `/memory reset` slash commands.

### Inter-Agent Communication
- [ ] Add `known_peers` (local hotel, role=agent) to session snapshot.
- [ ] Validate same-hotel peer task emission via existing `EmitTask` before designing `DelegateToPeer`.

## New Project: Agent Context Management

- [x] Pin the need for a dedicated management plane for agent-owned and operator-owned context graph mutations instead of continuing to rely on `mesh-config.json` edits plus restart cycles.
- [x] Accept the first implementation target as a hotel-mediated self-update path that writes canonical `AgentIdentityRecord` state rather than only mutating `philote` session-local profile data.
- [ ] Define the first request/response contract for `agent.context.update`, including:
  - self-only scope
  - path/update semantics
  - refreshed canonical profile projection on success
  - structured denial/validation failures
- [ ] Implement hotel-side validation and patching for the first bounded allowlist:
  - `identity_text`
  - `user_context_text`
  - `memory_summary`
  - `voice_response_policy.*`
  - `media_routing_policy.*`
- [ ] Refresh live runtime profile state from the hotel response after successful update instead of continuing to treat local `agent.configure` mutation as canonical.
- [ ] Define the first hotel-mediated self-management tool for bounded agent profile updates:
  - `identity_text`
  - `user_context_text`
  - `memory_summary`
  - `voice_response_policy.*`
  - `media_routing_policy.*`
- [ ] Define the first admin/operator management surface for inspecting and mutating any agent's context graph-backed profile/config.
- [ ] Decide which profile fields are self-editable vs admin-only, especially:
  - `soul_text`
  - transport config
  - model defaults
  - role/incarnation definitions
- [ ] Make the shared-identity boundary explicit in context management:
  - agent identity fields remain canonical at the agent layer
  - role addenda are additive posture only
  - role governance must not replace or fork the base identity layer
- [ ] Make runtime/profile updates flow through hotel-owned validation, authorization, persistence, and audit logging rather than direct file edits.

## New Project: Context And Memory Engines

Seam IDs: `context-engine-contract`, `deterministic-context-assembly`, `memory-engine-contract`, `graph-muninn-memory-dual-path`, `philotic-native-memory-integration`

- [ ] Review [PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md).
- [ ] Review [MEMORY_ENGINE_ABSTRACTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMORY_ENGINE_ABSTRACTION_PROPOSAL.md).
- [x] Define the first context-engine contract for deterministic turn context assembly.
- [x] Lock the canonical vocabulary for context assembly scope:
  - `conversation turn` for the external exchange boundary
  - `cognitive step` for internal reasoning/action within that boundary
- [ ] Define the first memory-engine contract for `search`, `store`, and provenance.
- [x] Define the first five context layers with owner, authority, mutability, and projection budget:
  - identity
  - relationship
  - session
  - working
  - knowledge
- [x] Publish the first compact layer contract table for:
  - canonical owner
  - authority level
  - refresh timing
  - promotion target
- [x] Define the first mutability classes for context layers:
  - `static_for_turn`
  - `refreshable`
  - `live_local`
- [x] Define the first concrete context-engine payload shapes:
  - `ConversationTurnScope`
  - `CognitiveStepScope`
  - `LayerContribution`
  - `ContextProjection`
  - `LayerPayload`
- [x] Prove one current context path and one current memory path behind those abstractions.
- [x] Thread the first structured `ContextProjection` path from `philote` into outbound model requests and through `model-router` prompt composition.
- [x] Carry session `active_incarnation_id` through the canonical snapshot, context projection, and model-router prompt composition so agent identity and active role posture stop being silently conflated.
- [ ] Review [MEMORY_RELATION_LIFECYCLE_WHITEPAPER.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMORY_RELATION_LIFECYCLE_WHITEPAPER.md).
- [ ] Define the first memory-formation contract:
  - candidate capture checkpoints
  - candidate payload shape
  - admission/promotion authority
- [ ] Define the first provisional relation-layer contract:
  - relation sources
  - relation lifetimes
  - confidence/authority semantics
  - promotion vs decay rules
- [ ] Define the first sleep/consolidation cycle contract:
  - reinforce
  - merge
  - weaken
  - archive/forget

## New Project: Admin Role And Surfaces

Seam IDs: `admin-posture-model`, `session-admin-elevation`, `cli-tui-admin-surface`, `action-grant-contract`

- [ ] Review [ROLE_POSTURE_AND_ADMIN_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROLE_POSTURE_AND_ADMIN_PROPOSAL.md).
- [ ] Review [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md).
- [ ] Make the admin role a first-class posture in role/incarnation records and operator UX.
- [ ] Define the first elevation model explicitly:
  - principal requests elevation
  - session carries elevated posture
  - hotel decides eligibility
  - dangerous actions use short-lived action grants
- [ ] Define the first admin action-grant contract:
  - `grant_id`
  - `principal_id`
  - `session_id`
  - `hotel_id`
  - `action_class`
  - optional `action_target`
  - TTL / expiry
  - one-time-use / nonce semantics
- [ ] Keep the conversational role intentionally narrow and membrane-facing by default.
- [ ] Define the first deterministic context-graph manager surface in the main CLI.
- [ ] Define the first TUI-backed admin workflow on top of that control plane.
- [ ] Define the first membrane-brokered admin workflow for secret add/rotate initiation where:
  - `membrane` starts the authenticated operator flow
  - hotel control plane owns authorization and mutation
  - vault owns secret persistence
- [ ] Make channel-agnostic admin ingress explicit so hotels without `membrane` remain fully manageable through CLI/TUI control-plane surfaces.
- [ ] Define eligibility rules for admin elevation:
  - trusted principal
  - approved channel/surface
  - hotel policy allows elevation
  - bounded TTL / expiry / revocation
- [ ] Prove one first grant-backed admin flow for vault secret add/rotate initiation before broadening admin mutations.

## New Project: Agent Plugin Hooks

Seam IDs: `agent-hook-registry`, `transcription-hook-extraction`

- [ ] Review [AGENT_PLUGIN_HOOKS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_PLUGIN_HOOKS_PROPOSAL.md).
- [ ] Define the first bounded hook families for context, memory, transcription, and response post-processing.
- [ ] Align hook timing and scope to the context-engine vocabulary:
  - run per `conversation turn` where appropriate
  - refresh at named `cognitive step` checkpoints where needed
- [ ] Define the first explicit hook checkpoints:
  - `conversation_turn.start`
  - `cognitive_step.context_build`
  - `checkpoint.before_model`
  - `checkpoint.after_model`
  - `checkpoint.after_tool`
  - `checkpoint.before_reply`
  - `conversation_turn.end`
- [x] Define the first concrete hook payload/result shapes:
  - `HookRequest`
  - `HookResult`
  - `RefreshRequest`
  - `PromotionAction`
- [ ] Move one live seam behind the hook model before broadening the design story.

## New Project: Local Admin Fallback Model

Seam IDs: `local-admin-capability-envelope`, `onnx-admin-fallback-path`

- [ ] Review [LOCAL_ADMIN_FALLBACK_MODEL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/LOCAL_ADMIN_FALLBACK_MODEL_PROPOSAL.md).
- [ ] Define the first bounded local-admin capability envelope.
- [ ] Decide how ONNX fits for embeddings, tool-calling support, and local degraded-mode admin workflows.
- [ ] Prove one local fallback path without external models.

## New Project: OpenClaw Parity And Migration

- [ ] Review [OPENCLAW_PARITY_MIGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OPENCLAW_PARITY_MIGRATION_PROPOSAL.md).
- [ ] Build the first explicit parity matrix for OpenClaw capability vs Philotic owner/confidence/gap.
- [ ] Identify the minimum migration-critical seams beyond simple feature demos.

## New Project: Agent Loop Gap Closure

- [x] Review [AGENT_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_LOOP_PROPOSAL.md).
- [x] Build real tool catalog with proper descriptions and schemas (Gap 3 — prerequisite for Gap 4).
  - [x] Add `class: Option<String>` field to `ToolDefinition` in `philote`.
  - [x] Create static `tool_catalog()` in `agent-core/src/catalog.rs` with real descriptions and schemas for `session.status`, `echo`, `workspace.list`, `workspace.read`.
  - [x] Add `AbstractToolRecord` to `ansible-mesh-core/src/graph.rs` as the context graph entity.
  - [x] Add `upsert_abstract_tool` / `get_abstract_tool` / `list_abstract_tools` to `GraphStorage` trait and `SqliteGraphStorage` impl.
  - [x] Seed catalog into context graph at hotel startup via `seed_abstract_tool_catalog`.
  - [x] Update both `default_tool_assembly_for_bindings` and `tool_assembly_from_allowed_incarnations` to look up from catalog before falling back to stubs.
- [x] Implement `preapproved_tools` and `preapproved_classes` evaluation in `approval_policy_allows` (Gap 4a).
  - [x] `tool_class()` and `tool_requires_approval()` added to `catalog.rs`.
  - [x] `approval_policy_allows` evaluates all three conditions in order: `auto_approve_all`, `preapproved_tools`, `preapproved_classes`.
  - [x] `handle_tool_call` enforces `approval_required` from `ToolPolicyAnnotation` before dispatch — agent-level gate independent of model intent.
  - [x] Policy annotations derive `policy_class` and `approval_required` from catalog.
  - [x] `agent.configure` tool: class `"config"`, `approval_required: true`. Handles `approval_policy.*`, `profile.*`, `bindings.*` paths with set/append/remove. Rebuilds tool assembly when bindings change.
- [x] Inject operator steering notes from `/approve`/`/deny` back into the model prompt (Gap 4b).
  - [x] `/approve <note>` re-submits to model with `[User approval steering]` prefix appended to turn context.
  - [x] `/deny <note>` re-submits as a new model turn with `[User denied the proposed action. Do this instead]` prefix.
  - [x] `/deny` without note fails the turn (existing behavior preserved).
  - [x] `resume_turn_with_steering` appends the note to `active_turn.user_content`, increments iteration, rebuilds prompt, re-submits to model.
- [x] Add `working_tool_history` to `WorkingTurn` and implement multi-turn tool re-entry loop with iteration cap (Gap 1).
  - [x] `working_tool_history: Vec<(ToolCall, ToolResult)>` added to `WorkingTurn`; serialized into checkpoint and rehydrated from it.
  - [x] `push_tool_history` and `build_reentry_prompt` added to `SessionState`.
  - [x] `build_reentry_prompt` appends `[Tool call history]` section with numbered call/result pairs; directs model to continue or respond.
  - [x] `handle_tool_result` rewritten: pairs result with `pending_tool_call`, pushes to history, checks `MAX_TOOL_ITERATIONS` (10), increments `iteration`, re-submits `generate_text` to model. Loop exits only on `Respond`, `Fail`, `RequestApproval`, or iteration cap.
  - [x] `interpret_tool_result` (old early-close stub) no longer called.
- [x] Add `MediaRoutingPolicy` to `AgentProfile` and make media action selection configurable per agent (Gap 2).

## New Project: Agent Incarnation Model

Model revised: three-kind taxonomy (conversational/worker/subagent) replaced with role incarnations + workers/subagents. See [AGENT_INCARNATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_INCARNATION_PROPOSAL.md).

### Skill Catalog + Toolset Profiles (prerequisite for role provisioning)
- [x] Add `AbstractSkillRecord` to `ansible-mesh-core/src/graph.rs` (parallel to `AbstractToolRecord`).
- [x] Add `upsert_abstract_skill` / `get_abstract_skill` / `list_abstract_skills` to `GraphStorage` trait and `SqliteGraphStorage`.
- [x] Add `ToolsetProfileRecord` to the context graph (`toolset_profile` node kind).
- [x] Add `upsert_toolset_profile` / `get_toolset_profile` / `list_toolset_profiles` to `GraphStorage` trait and `SqliteGraphStorage` impl.
- [x] Seed built-in toolset profiles at hotel startup (see main section above).
- [x] Update session binding assembly to expand skill grants into `implied_tools` when building `tools_for_model`.

### Role Incarnation Records
- [ ] Add `RoleIncarnationRecord` and `TurnLoopConfig` to the context graph (`role_incarnation` node kind).
- [ ] Add `upsert_role_incarnation` / `get_role_incarnation` / `list_role_incarnations` to `GraphStorage`.
- [x] Add `ConfigureRole` IPC action (orchestrator → hotel); hotel enforces orchestrator-only writes.
- [x] Seed session bindings from the role's `toolset_profile` when a role incarnation session is initialized.

### Active Membrane Routing
- [ ] Add `active_incarnation_id` to `SessionRecord` in the Context Graph.
- [ ] Update IpcServer task routing to read `active_incarnation_id` before routing inbound agent tasks.
- [ ] Default to orchestrator incarnation if active ID is unregistered; buffer inbound during materialization.

### Handoff Skill + Membrane Switching
- [x] Implement `HandoffToRole { role_name, handoff_bundle }` and `HandoffBack { summary, return_to? }` IPC actions.
- [x] Define the first handoff skill shape: trigger patterns, context bundle assembly, cleanup steps.
- [x] Add `/role <name>` and `/back` slash commands for manual membrane switching.
- [x] Fix handoff turn termination: call `complete_local_command` on success to prevent model continuation hallucinations.
- [x] Exempt `handoff.back` from mandatory operator approval for fluid returns.

### Inactive TTL + On-Demand Rematerialization
- [x] Add `inactive_ttl_seconds` to `RoleIncarnationRecord`.
- [x] Extend supervisor loop TTL check: reclaim inactive non-membrane-owner role processes after TTL.
- [ ] On rematerialization: hotel sends session snapshot to restore working memory from Tier 2.

### Workers / Subagents
- [x] Implement `SpawnSubagent` IPC and async result routing back to parent incarnation (see main Workers section above).
- [x] Fix subagent spawn path (see main Workers section above).
- [x] Add `/abandon` slash command (see main Workers section above).

### Memory
- [ ] Add `session_facts` apartment type and `UpdateMemory` IPC with hotel-side rate/size enforcement.
- [ ] Add Muninn tool surface (`memory.search`, `memory.store`) as hotel-mediated tools with auto-injection into prompt context.
- [ ] Add `/memory show` and `/memory reset` slash commands.

### Inter-Agent Communication
- [ ] Add `known_peers` (local hotel, role=agent) to session snapshot.
- [ ] Validate same-hotel peer task emission via existing `EmitTask` before designing `DelegateToPeer`.

## New Project: Philotic Agent Loop

- [x] Write a dedicated proposal for the Philotic loop architecture using Pi as the core turn-engine reference. → Superseded by COGNITIVE_LOOP_PROPOSAL.md
- [x] Write an implementation spec for loop state, events, checkpoints, tools, and approval interrupts. → Covered in COGNITIVE_LOOP_PROPOSAL.md
- [ ] Define the provider boundary (`transformContext`, `convertToLlm`, tool/result records, structured outputs).
- [ ] Define the bounded execution loop and checkpoint boundaries.
- [ ] Define approval interrupt/resume semantics.
- [ ] Define loop event streaming and tracing payloads.

## New Project: Cognitive Loop Architecture

Proposal: [COGNITIVE_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/COGNITIVE_LOOP_PROPOSAL.md)
Seam IDs: `context-envelope-contract`, `memory-local-tools`, `active-plan-streaming`, `rules-tier`

### Slice 1 — Context Envelope Fix (unblocks everything else)
- [x] Add `build_reentry_context_envelope()` to `session.rs` — returns full `(prompt, context, context_projection, tools)`.
- [x] Ensure all cognitive calls (initial + re-entry) send `context: Some(...)`, `context_projection: Some(...)`, `response_contract: Some(...)`.
- [x] Add `tool_history` as a context envelope section in `model_context_from_projection` — always present, empty on initial turn, populated on re-entry.
- [x] Add `ToolHistoryEntry` to `ContextEnvelope` in model-router; parse and render `[Tool call history]` in `composed_prompt_text()`.
- [x] `handle_tool_result` now uses `build_reentry_context_envelope()` — model-router receives full structured envelope on every re-entry.
- [x] Keep `build_reentry_context_envelope()` on the same cognitive tool-projection policy as initial turns, so low-intent re-entry does not bypass affordance suppression.
- [ ] `active_plan` as context section (deferred to Slice 4).

### Slice 2 — Settings Tree
- [x] Add `AgentSettings`, `ContextWindowPolicy`, `MemoryPolicy`, `ExecutionPolicy` structs to `session.rs`.
- [x] Create `docs/architecture/AGENT_SETTINGS_CATALOG.md` — full catalog with types, defaults, valid ranges.
- [x] Expand `agent.configure` config paths to `settings.*` prefix (9 new paths, all clamped).
- [x] Add dialogue window char-budget filtering in `model_context_from_projection`.
- [x] Wire `settings.memory.memory_window_size` into `complete_active_turn` (replaces hardcoded `8`).
- [x] Wire `settings.execution.iteration_cap` into `handle_tool_result` (removes `MAX_TOOL_ITERATIONS` constant).
- [x] Time-based dialogue window filtering: `created_at: u64` on `TurnRecord`; time-first roll-off (minutes), then char-budget pass on what remains.

### Slice 3 — Memory Local Tools ✓
- [x] Add `memory.recall` to `catalog.rs` — class `"memory"`, approval: false, local-agent execution.
- [x] Add `memory.remember` to `catalog.rs` — class `"memory"`, approval: false, local-agent execution.
- [x] Wire `memory.recall` handler in `runtime.rs` → `engine.activate(query, SelfOnly, limit)`.
- [x] Wire `memory.remember` handler in `runtime.rs` → `engine.remember(SelfOnly, ...)`.
- [x] Add `memory` skill entry in `skill_implied_tools` (implies `memory.recall` + `memory.remember`).
- [x] `is_local_agent_tool` recognises `memory.recall` and `memory.remember` — routes as `local_agent`.
- [ ] Add `memory_summary` response channel to Gemini schema (alongside `memory_concept`). (deferred)

### Slice 4 — Active Plan + Streaming ✓
- [x] Add `ActivePlan` + `PlanStep` structs; `active_plan: Option<ActivePlan>` + `consecutive_step_failures: u32` on `WorkingTurn`.
- [x] Capture plan from model response in `handle_model_response`; emit `plan_ready` on first capture.
- [x] Turn events: `step_started` (handle_tool_call), `step_completed` / `step_failed` / `loop_recovering` (handle_tool_result). Gated by `stream_tool_events` setting.
- [x] Stall detection: `consecutive_step_failures >= stall_detection_threshold` → `fail_active_turn` with "Stall detected" message. Failures reset to 0 on any successful step.
- [x] `active_plan` added to context envelope (model sees current plan on re-entry) and Gemini response schema (`active_plan` channel in response_contract).
- [x] `active_plan` threaded through `serialize_text_result` and `ProviderOutput::Text` in model-router.

### Slice 5 — Rules Tier ✓
- [x] Add `RuleRecord` to context graph in `ansible-mesh-core/src/graph.rs` (alongside `AbstractToolRecord`).
- [x] Add `upsert_rule` / `get_rule` / `list_rules` to `GraphStorage` trait and `SqliteGraphStorage` impl.
- [x] Add `rule.propose` tool to `catalog.rs` — class `"config"`, always requires operator approval (bypasses preapproval like `is_admin_role_creation`).
- [x] Inject rules into `instructions` section on session snapshot (fetched at session init via `IpcRequest::ListRules`; stored in `SessionState.rules`; rendered as `[Rules]` block in `project_session_context`).
- [x] Wire `CognitiveOutcome` → Rule elevation pathway via hotel IPC: `ProposeRule`/`ListRules` IpcRequest variants; `RuleProposed`/`RuleList` IpcResponse variants; `execute_local_agent_tool` dispatches `rule.propose` → hotel → `upsert_rule`; operator-confirmed via always_require_human gate.

## New Project: Guest Binary Resolution

- [ ] Review [GUEST_BINARY_RESOLUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/GUEST_BINARY_RESOLUTION_PROPOSAL.md).
- [x] Replace hardcoded `target/debug/<name>` paths in `guest_seed_for_profile` with configurable absolute paths or binary names resolved via `PHILOTIC_BIN_DIR`.
- [x] Align seeded guest binary names with actual compiled binary names (`model-router` instead of `model-controller-gemini`/`model-controller-elevenlabs`).
- [x] Define the dev-mode vs deployed-mode binary resolution contract so the same seed logic works in both environments without shims.
- [x] Remove the `target/debug/` Ansible shim task once the Rust code is fixed.
- [x] Define placeholder policy for unimplemented guests (e.g. `tool-runner`) — skip or warn rather than fail spawn.

## New Project: Red Hat Ansible / VPS Deployment Boundary

Seam IDs: `secret-handling-hardening`, `watched-live-vps-smoke`, `artifact-distribution-rollout`

- [x] Pin the architecture boundary between Red Hat Ansible as the outer deployment orchestrator and Philotic `aiua` as the inner hotel runtime authority.
- [x] Define the first Linux/VPS deployment contract:
  - host prerequisites
  - filesystem layout
  - service manager shape
  - config/secrets inputs
  - binary/artifact placement
- [ ] Remove plaintext secret-file assumptions from the VPS deployment path:
  - no raw secrets in `mesh-config.json`
  - no plaintext `secrets.env`
  - encrypted bootstrap material or platform secret-store handoff only
- [x] Define the first peer inventory/rendering contract for deployed hotels so cross-host mesh no longer depends on loopback assumptions.
- [ ] Prove a first VPS deployment smoke for one hotel.
- [ ] Prove a first multi-host or local-to-VPS two-hotel roundtrip.
- [ ] Render and deploy `beacon-test-hotel` on `vps-jane` with a Beacon agent profile, VPS-local `import_workspace`, and hotel-scoped Telegram credentials so Beacon can be UATed on the VPS test stack.

## New Project: Native Overlay / VPN

- [x] Decide that Philotic should separate UDP control-plane gossip from point-to-point execution transport rather than treating one datagram path as the final carriage for all inter-hotel work.
- [x] Capture the migration constraint that hotel identity must be independent of current host VPN reachability so Tailscale/WireGuard can be replaced without a routing-schema rewrite.
- [ ] Define the first tower execution-transport contract:
  - transport negotiation inputs
  - reachability advertisement shape
  - reliability / streaming expectations
  - blob handoff boundary
- [ ] Define the first application-layer trust contract for a future native overlay:
  - hotel identity keys
  - mutual authentication
  - authorization
  - rotation / revocation
- [x] Add execution-plane reachability advertisement to the hotel capability registry.
- [x] Implement the first point-to-point hotel execution transport for routed tasks.
- [x] Move remote model/tool/task execution off raw UDP Beacon payload bodies.
- [ ] Define NAT traversal / relay requirements explicitly before committing to a self-hosted overlay transport.

## New Project: Hotel Perimeter Trust

- [ ] Review [HOTEL_PERIMETER_TRUST_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HOTEL_PERIMETER_TRUST_PROPOSAL.md).
- [ ] Define hotel membership records so “inside the perimeter” is explicit rather than implied by peer discovery.
- [ ] Define hotel identity/auth material beyond transitional dev PSK assumptions.
- [ ] Define join / invite / revoke lifecycle for hotel membership.
- [ ] Require authenticated control-plane traffic outside explicit dev mode.
- [ ] Define authorization scope for which trusted hotels may receive which routed capability classes.

## New Project: Inter-Hotel Routing And Placement

Seam IDs: `placement-policy-broadening`, `multi-host-watched-validation`

- [x] Decide that inter-hotel routing should extend the same route contract already used for intra-hotel execution rather than creating a second remote-only routing abstraction.
- [x] Decide that hotels must advertise capabilities plus live incarnations and remain authoritative for the incarnations they materialize.
- [x] Decide that incarnation identity is hotel-scoped and deterministic: `<hotel_name>:<guest_id>`.
- [x] Decide that unpinned remote routing should resolve by deterministic placement scoring rather than first-match or broadcast behavior.
- [x] Close the first placement inputs for remote selection:
  - latency
  - available capacity / CPU headroom
  - deterministic tiebreak by canonical incarnation id
- [x] Define the first capability advertisement payload and hotel-side registry shape for hotel-scoped incarnations.
- [x] Add heartbeat emission / refresh / TTL rules for the capability advertisement plane.
- [ ] Extend current session/tool/model/membrane route records so the shared routing schema carries remote-capable incarnation metadata consistently.
- [x] Extend placement-based remote selection into model capability routing for `text.generate` and `media.analyze` while keeping membrane reply delivery session-owned.
- [x] Build the first live capability registry view across hotels.
- [x] Implement the first placement-based remote selection for unpinned capability routes on tool fallback when no local runner is available.
- [ ] Extend placement-based remote selection beyond the first tool/model fallback paths to broader routed component classes without breaking session-owned membrane reply routing.
- [ ] Move mesh ACK emission to a strict post-commit boundary.
- [x] Replace routed execution over raw UDP with the first point-to-point execution channel for routed inter-hotel task traffic.
- [ ] Add execution-plane transport negotiation so routing can choose among multiple point-to-point transports instead of assuming one TCP path.

## New Project: Multi-Hotel Component Distribution

Seam IDs: `multi-hotel-route-consistency`, `cross-host-distributed-validation`, `remote-materialization-ceremony`, `capacity-relief-placement`

- [ ] Review [MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md).
- [ ] Extend remote-capable route metadata consistently across remaining routed component classes beyond the first tool/model paths.
- [ ] Preserve session-owned membrane reply routing while proving broader distributed component placement.
- [ ] Define the first remote materialization ceremony:
  - mesh-visible intent
  - deterministic winning target selection
  - targeted materialization request to the winner
  - readiness publication before parked work is released
  - explicit distinction between routeable-ready and lease-authorized when the component family is singleton-scoped
- [ ] Define the first capacity-relief placement flow:
  - stressed hotel help signal
  - candidate offers
  - winning offer selection
  - drain/retire contract instead of immediate kill-by-panic
- [ ] Move inter-hotel ACK behavior toward strict post-commit truth before claiming multi-hop reliability.
- [ ] Build the first watched local multi-hotel vertical slice:
  - membrane on one hotel
  - agent on another
  - model on another
  - tool runner on another
- [ ] Build the first cross-host version of that distributed slice once perimeter trust is in place.

## New Project: Router-Native Observability

- [ ] Review [ROUTER_NATIVE_OBSERVABILITY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROUTER_NATIVE_OBSERVABILITY_PROPOSAL.md).
- [ ] Define the first structured observability event envelope for routed runtime events.
- [ ] Define attachable listener registration and filter semantics.
- [ ] Keep a minimal bootstrap/fatal emergency sink outside the router so pre-router failures are still visible.
- [ ] Prove one console listener and one persistent event sink.
- [ ] Decide how observability events can later feed eval/reinforcement datasets without becoming a second logging ontology.

## New Project: Proposal Organization

- [ ] Review [PROPOSAL_ORGANIZATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PROPOSAL_ORGANIZATION_PROPOSAL.md).
- [ ] Decide the first folder/tag/backlink strategy for organizing growing proposal volume in `docs/architecture/`.
- [ ] Define which proposals should be grouped by domain versus lifecycle.
- [ ] Add lightweight backlink conventions so active proposals can point to adjacent work without turning into wiki sprawl.

## New Project: Model Controller

Seam IDs: `structured-model-envelope`, `hotel-gemini-oauth-flow`

- [ ] Review [MODEL_CONTROLLER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MODEL_CONTROLLER_PROPOSAL.md).
- [x] Land the first `voice.synthesize` request envelope with `display_text`, `spoken_text`, `voice`, `model`, and `provider_options`.
- [x] Add an upstream producer example that emits the richer `voice.synthesize` envelope through the hotel.
- [x] Move the first task failure contract into the shared protocol layer (`philotic-client`) so `model-router` and `philote` can exchange structured errors without making `philote` the accidental owner of reality.
- [ ] Emit structured task failures consistently from all model-controller capability paths and log provider/code/component fields during watched runs.
- [ ] Define the canonical capability envelope for:
  - `text.generate`
  - `voice.synthesize`
  - `voice.dialogue`
  - `sound.generate`
  - `music.generate`
  - `response.generate`
- [x] Add `request_class` to the canonical model-controller envelope and define the first legal classes:
  - `cognitive`
  - `transform`
  - `synthesis`
  - `embedding`
- [x] Define which envelope fields are expected vs usually avoided for each request class so embeddings/transforms do not inherit cognitive baggage by accident.
- [x] Propose the first structured model request envelope split:
  - `response_contract`
  - `context`
  - `affordances`
  - `routing_hints`
  - `provider_options`
- [x] Implement the first compatibility-first structured model envelope seam in `model-router`.
- [x] Thread `request_class` through the current live model request paths:
  - `text.generate` agent reasoning -> `cognitive`
  - `media.analyze` / `voice.transcribe` -> `transform`
  - `voice.synthesize` -> `synthesis`
- [x] Add and run a startup-driven cognitive smoke that proves the structured cognitive prompt path reaches the fake Gemini provider envelope.
- [x] Propose the first structured model response envelope split:
  - `result`
  - `artifacts`
  - `trace`
  - `provider_output`
- [x] Define optimization-oriented response channels:
  - `display_text`
  - `spoken_text`
  - `working_memory_delta`
  - `follow_up_questions`
  - `intent_summary`
  - `state_updates`
  - `delivery_hints`
- [x] Define canonical context layers for model requests:
  - `instructions`
  - `identity`
  - `memory`
  - `dialogue_window`
  - `active_turn`
  - `attachments`
- [x] Decide that tools and skills should project separately from semantic context under `affordances`.
- [x] Implement the first compatibility-first structured model response seam in `model-router`.
- [x] Preserve a first-class minimal prompt-response path with the structured envelope fields remaining mostly optional.
- [x] Add initial `response_contract.channels` handling for text-generation requests.
- [x] Add first structured response fields for:
  - `display_text`
  - `spoken_text`
  - `working_memory_delta`
  - `follow_up_questions`
- [ ] Add `spoken_text` / expressive speech projection alongside user-visible text.
- [ ] Define ElevenLabs default-voice pinning plus upstream voice override behavior.
- [ ] Add Eleven v3 model selection and expressive-tag support without pretending it is the same as the low-latency conversational path.
- [ ] Define how native-audio multimodal models emit text plus audio without being forced through TTS.
- [ ] Define Gemini auth modes:
  - hotel-managed OAuth
  - API key fallback
  - possible ADC path
- [x] Add the first Gemini guest auth abstraction that prefers OAuth bearer config over API key fallback.
- [x] Add `ansible --test` startup harness support for:
  - `text-roundtrip`
  - `gemini-oauth-roundtrip`
  - `telegram-roundtrip`
  - `voice-sample`
- [x] Add a startup-driven model-controller smoke script for the text round-trip path.
- [x] Add a startup-driven Gemini OAuth smoke through the materialized model-controller guest.
- [x] Add a hotel-startup Telegram controller smoke via `ansible --test telegram-roundtrip` using a local fake Telegram API.
- [x] Extend the startup Telegram smoke so it simulates text, photo, and voice-note ingress and exercises fake-Gemini multimodal requests on top of blob-backed media transport.
- [x] Make the startup Telegram smoke honestly green end-to-end by fixing `PhiloticClient` frame reads to survive `tokio::select!` cancellation during the final voice reply handoff to `membrane`.
- [x] Prove watched-live Telegram text/photo/voice/document delivery through membrane -> agent-core -> Gemini and normalize markdown-ish document MIME for Gemini media analysis.
- [x] Make materialized Telegram/agent guests configurable enough for separate hotel/persona stacks (for example Jane vs Aria) instead of hardcoding one Jane-shaped membrane.
- [x] Remove Jane/Aria-specific built-in hotel/agent profile selection from `aiua` startup so agent identity, persona naming, and guest targeting resolve from hotel config or generic hotel-derived fallback rather than persona-specific Rust tables.
- [x] Make hotel-local node identity explicit across startup/runtime smokes and materialized guests so Jane and Aria can both pass local `/ping` and startup text round-trips without hidden `local-aiua-01` assumptions or legacy `model` role registration.
- [x] Make inter-hotel mesh dispatch node-aware by carrying `target_node_id`, discovering peer hotels from the Context Graph, and returning real mesh ACK packets for local multi-hotel development.
- [x] Prove a first local two-hotel remote model smoke over the new TCP execution plane after remote model placement resolves through the live registry.
- [ ] Seed `hotels.default.agents.aria.telegram.bot_token` (and tokens for beacon, hermes, astrid) in local `mesh-config.json` and run the first multi-agent Telegram smoke with all five agents in the default hotel.
- [ ] Tighten inter-hotel mesh reality gaps: preserve target guest specificity across hotels, move ACK emission to a true post-commit boundary, and replace loopback-only peer addressing with explicit host authority.
- [x] Support `hotels.<hotel>.agents.<agent>.import_workspace` so startup can seed the selected agent identity bundle from a declared workspace path.
- [x] Make agent-level media routing policy configurable so text/media/voice decisions are owned by the agent/session profile instead of one hardcoded runtime branch.
- [x] Investigate splitting voice-note transcription/understanding toward ElevenLabs or another speech-specialized provider while keeping richer text reasoning in the agent/model loop.
- [x] Add `VoiceResponsePolicy` to `AgentProfile` so the agent has its own voice identity and TTS is policy-driven, not tool-driven.
- [ ] Make the default local Jane/Aria voice UX honest: `mode=auto` should mirror voice-input turns with voice-only replies, while `/tts on` should escalate to voice+text delivery.
- [x] Route `voice.transcribe` results back into the normal agent reasoning loop before `voice.synthesize`, so voice turns stop parroting the transcript and instead speak the post-reasoning answer.
- [x] Carry an explicit staged `TurnRoutingPlan` on active voice turns so ingress transcription, cognitive generation, and voice egress are visible as one session-owned execution contract instead of an ad hoc branch.
- [x] Make staged routing operational in model request assembly: trim ingress context envelopes and forward stage-derived routing hints into model-controller requests.
- [x] Make stage policy influence affordances and output contracts: suppress tools on non-cognitive stages and request slimmer response channels for low-intent cognitive turns.
- [x] Extend stage-aware affordance policy into prompt/context projection: hide skill guidance and detailed approval posture on non-cognitive and low-intent turns.
- [x] Make approval interrupts stage-aware: redirect low-intent free-form approval asks back to direct response and reject non-cognitive approval interrupts while preserving real scripted/tool gates.
- [x] Define a governed `routing.policy.propose` tool and companion `routing.refinement` skill so the agent can reflexively suggest cognition/routing policy updates under operator approval and memory write-back.
- [x] Add first-class agent-graph routing preferences plus active `agent.graph.read`/`agent.graph.write` tool definitions so routing posture can be stored in agent-local graph state instead of only prompt text.
- [x] Feed stored agent-graph routing preferences into live `TurnRoutingPlan` compilation by projecting them through hotel session bindings and applying advisory provider/model hints per stage.
- [x] Project an explicit `effective_rights` key ring through hotel session bindings and enforce it in tool/component assembly so lower execution layers stop widening visibility just because a runner or route exists.
- [x] Add the first shared abstract-right catalog substrate and carry `effective_rights` into model-controller requests so lower execution layers can validate tool visibility against the hotel's key ring.
- [x] Carry opportunistic `agent_graph_snapshot` continuity on transported agent-directed task payloads and hydrate it before local delivery so an agent that lands on another aiua can bring its graph with it even before background mesh sync catches up.
- [x] Prefer explicit routed `agent_id` hints for transported graph continuity when the source hotel cannot infer ownership from a local session or guest incarnation, starting with remote operator-chat and peer-delegation paths.
- [x] Carry `authority_hotel` alongside transported `agent_id` hints and only attach authoritative `agent_graph_snapshot` continuity when the sending hotel is the agent's actual home authority, so transport placement does not masquerade as durable identity ownership.
- [x] Persist agent runtime provenance into session state and canonical session snapshots on the receiving hotel so `authority_hotel` and current delivery placement stay visible after transport instead of disappearing once the task is delivered.
- [x] Use persisted local delivery provenance to influence local agent routing and materialization when a session has no usable active incarnation, so foreign-owned sessions can keep executing on the intended local guest instead of immediately collapsing back to generic orchestrator fallback.
- [ ] Replace the transitional durable-rule bridge behind `routing.policy.propose` with routing-specific policy records, evaluation history, and operator disposition state.
- [ ] Fold model-graph metadata into live turn-routing-plan ranking so shared catalog truth informs selection without becoming mutable preference storage.
- [ ] Define the shared catalog layer for models, tools, skills, and rights as reference knowledge outside hotel-owned mutable state.
- [ ] Extend the hotel's projected key ring beyond `effective_rights` for tools/skills/component capabilities into scoped execution credentials and richer right classes.
- [ ] Audit `model-router` and runner dispatch beyond current tool/component assembly so lower execution layers consume projected rights but never inject or widen them.
- [x] Add a rebuild-first local watched-UAT workflow so stale materialized binaries and stale sockets do not masquerade as runtime regressions.
- [ ] Define hotel CLI OAuth UX:
  - browser launch
  - temporary localhost callback listener
  - token exchange
  - token storage/refresh
  - guest handoff
- [x] Add a transitional `ansible auth google start --provider gemini` flow with browser launch, localhost callback, token exchange, and access-token persistence.
- [x] Add a hotel-side Gemini OAuth validation command that performs a real model call with the stored auth path.
- [x] Refresh model-controller provider config per task so updated Gemini auth takes effect without a guest restart.
- [ ] Run a Keychain-backed Gemini OAuth smoke with `PHILOTIC_VAULT_MASTER_KEY` unset.
- [x] Run a full guest-path Gemini OAuth smoke through the materialized model-controller, not just hotel-side validation.
- [ ] Wire refresh-token persistence and refresh lifecycle behind the hotel vault.
- [x] Deliver an honest ElevenLabs end-to-end voice path beyond inline-audio/testing mode, including watched-live confirmation that `voice.synthesize` produces canonical audio artifacts instead of a model-router policy refusal.
- [ ] Add `OpenAIProvider` to `model-router` on the existing provider seam:
  - `text.generate`
  - structured outputs
  - tool calling
  - image-aware text input where supported
  - `text.embed`
- [ ] Extend provider config/auth loading for OpenAI:
  - API key path
  - OAuth bearer path
  - refreshable OAuth path owned by the hotel
- [ ] Define hotel CLI OAuth UX for OpenAI:
  - browser launch
  - temporary localhost callback listener
  - token exchange
  - token storage/refresh
  - guest handoff
  - validation command
- [ ] Add the first OpenAI startup smoke through the materialized model-controller guest.
- [ ] Define provider capability overrides for specialized OpenAI model features:
  - reasoning effort / depth
  - verbosity
  - background mode
  - built-in tools gated behind explicit provider options
  - realtime/audio kept as a follow-on slice, not part of the first parity path

## New Project: Key Vault

Seam IDs: `vault-secret-refs`, `remote-vault-delegation`

- [ ] Review [KEY_VAULT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/KEY_VAULT_PROPOSAL.md).
- [x] Define the first vault record schema and context-graph secret references.
- [x] Begin removing new OAuth access-token storage from plain `node_config` by storing secret refs instead.
- [x] Define and implement the first hotel-local secret fetch API for guests.
- [x] Define and implement the first envelope-encryption and root-key strategy:
  - OS keychain / TPM / Secure Enclave preferred
  - cloud KMS/HSM for hosted hotels
  - operator master key only as fallback
- [ ] Replace the current local root-key bootstrap with stronger platform-native backing beyond basic macOS Keychain item storage.
- [ ] Define secret lifecycle:
  - create
  - stage
  - rotate
  - revoke
  - rollback
- [ ] Define explicit secret classes and exposure policy:
  - `provider-runtime`
  - `provider-root`
  - `admin`
  - `transport`
- [ ] Define mesh-visible vault metadata:
  - ownership
  - state/version
  - rotation/health status
  - no raw secret material
- [ ] Define the first mesh-visible vault metadata record shape:
  - `secret_ref`
  - `owning_hotel`
  - `secret_class`
  - `state`
  - `version`
  - rotation/health fields
- [ ] Enforce the rule that admin key material never flows to model-facing components or prompt-visible tool output.
- [ ] Define admin-key workflows as hotel-owned control-plane operations rather than raw key release wherever possible.
- [ ] Move Gemini OAuth refresh tokens behind vault references.
- [x] Move Gemini OAuth access tokens behind vault references for model-controller consumption.
- [ ] Define Telegram-safe secret onboarding:
  - control-plane command in chat
  - Mini App or secure browser handoff
  - no plaintext secret entry in normal chat messages
- [ ] Define Telegram-safe rotation UX and operator approvals.
- [ ] Define the membrane/admin split for secret workflows explicitly:
  - `membrane` as operator entry point
  - hotel control plane as mutation authority
  - vault as secret authority
- [ ] Define remote vault admin delegation:
  - local admin surface may broker
  - owning hotel validates and executes
  - structured outcome returns across the mesh
  - no default raw secret export
- [ ] Define the first remote vault admin delegation envelope:
  - `request_id`
  - `source_hotel`
  - `target_hotel`
  - `principal_id`
  - `session_id`
  - `grant_id`
  - `action_class`
  - structured payload

## New Project: Mesh Visibility And State Placement

Seam IDs: `mesh-visible-state-contract`

- [ ] Review [MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md).
- [ ] Define the first shared state-classification rubric:
  - hotel-local only
  - hotel-owned canonical with remote query/delegation
  - mesh-visible metadata
  - single-writer leased state with mesh-visible owner
  - replicated/federated state
- [ ] Define the first common mesh-visible record envelope:
  - `record_type`
  - `record_id`
  - `owning_hotel`
  - `canonical_writer`
  - `state`
  - `version`
  - `updated_at`
  - optional health/lease/delegation fields
- [ ] Inventory the first current candidate record families:
  - Telegram poll lease authority
  - vault metadata
  - agent-home authority
  - routed capability availability
- [ ] Define the first decision checklist for when local SQLite/file-db truth is too cumbersome and needs a mesh-visible projection or a stronger state-plane boundary.
- [ ] Decide the first publication/query mechanism for mesh-visible records without treating raw SQLite rows as a network contract.
- [ ] Reclassify existing active seams against the shared rubric:
  - [TELEGRAM_POLL_LEASE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_POLL_LEASE_PROPOSAL.md)
  - [KEY_VAULT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/KEY_VAULT_PROPOSAL.md)
  - [MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md)

## New Project: Runtime Authority Leases

Seam IDs: `runtime-authority-leases`

- [ ] Review [RUNTIME_AUTHORITY_LEASES_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RUNTIME_AUTHORITY_LEASES_PROPOSAL.md).
- [ ] Define the shared lease contract fields:
  - `lease_type`
  - `lease_scope`
  - `authority_hotel`
  - `owner_guest_id`
  - optional `owner_hotel`
  - `lease_epoch`
  - `lease_expires_at`
  - `last_heartbeat_at`
  - `status`
- [x] Compare [SESSION_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SESSION_LOOP_PROPOSAL.md) session leases and [TELEGRAM_POLL_LEASE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_POLL_LEASE_PROPOSAL.md) poll leases against the shared archetype and note field/behavior convergence.
- [x] Define the first clear boundary contract between:
  - lease authority
  - materialization
  - supervision
  - routing
  - vault/secret access
- [x] Distinguish authority leases from retention leases so downscaling policy does not force exclusive authority semantics onto replicated capacity.
- [ ] Apply that boundary contract to the next concrete runtime seam so route demand, standby materialization, and lease grant order are proven in one non-Telegram path.
- [ ] Classify the first component families by authority profile and lease family:
  - agents / session actors
  - Telegram pollers
  - model workers
  - tool runners
- [x] Implement the first shared lease abstraction:
  - shared `LeaseEnvelope`
  - provider contract
  - observer hook vocabulary
  - event hooks for owner-change, expiry, revoke, and stale-owner cleanup
- [x] Adapt Telegram poll lease to the shared abstraction.
- [x] Restore startup dual-poller smoke to green under the shared lease abstraction.
- [x] Harden the startup dual-poller handoff so clearing dead guest PIDs cannot accidentally reactivate a retired poller.
- [ ] Decide which next non-Telegram runtime seam should adopt the lease archetype first.

## New Project: Desktop Membrane

Seam IDs: `desktop-membrane-boundary`, `desktop-membrane-lease`, `desktop-membrane-view-models`

- [x] Analyze the current `philotic-web serve` and embedded `jaredlikes-desktop` coupling.
- [x] Write [DESKTOP_MEMBRANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DESKTOP_MEMBRANE_PROPOSAL.md).
- [x] Define the first `desktop_membrane` authority lease scope and owner identity.
- [x] Acquire the desktop membrane lease before `philotic-web serve` exposes privileged routes.
- [x] Renew the lease while an authenticated local operator session is active.
- [x] Release the lease on clean shutdown and fail closed on lost lease or hotel disconnect.
- [x] Remove unauthenticated token injection from the embedded desktop bootstrap.
- [x] Replace websocket query-string token auth with a bounded same-session attach mechanism.
- [x] Stop persisting injected desktop session credentials by default.
- [ ] Replace direct SQLite/config reads in `serve` with the first hotel-owned read models for:
  - [x] service status
  - [x] guests
  - [x] redacted agents
- [x] Decide that apartment inspection does not belong in the default desktop membrane surface; if it returns later, it should come back only as a shaped hotel-owned diagnostic view.
- [ ] Define the first mesh-aware desktop routing contract for:
  - [x] local-hotel reads
  - [x] remote single-target inventory routing with source/freshness attribution
  - mesh-aggregate reads
- [ ] Define the first target selection and attribution model so desktop views can show:
  - [x] source hotel
  - [x] freshness
  - pending remote action state
- [ ] Define the first remote-target read models for:
  - [x] target inventory / mesh targets
  - [x] target hotel status (local-canonical, remote-canonical when query succeeds, heartbeat-observed fallback when it does not)
  - [x] target guest inventory
  - [x] bounded target agent inventory
- [ ] Extract the plug-and-play membrane boundary before adding more operator surface:
  - [x] write [OPERATOR_MEMBRANE_PLUGIN_BOUNDARY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OPERATOR_MEMBRANE_PLUGIN_BOUNDARY_PROPOSAL.md)
  - [x] define reusable operator surface planes separate from desktop-specific route names
  - [x] define the first target-oriented operator surface family:
    - `operator.targets.list`
    - `operator.targets.status`
    - `operator.targets.guests`
    - `operator.targets.agents`
  - [x] define caller-aware redaction/posture/grant semantics so agents and automation can query the same surfaces when allowed
  - [x] define the first router-mediated handoff envelope for non-local operator surface execution
  - [x] land the first generic operator target IPC contract for:
    - `operator.targets.list`
    - `operator.targets.status`
    - `operator.targets.guests`
  - [x] identify which current `desktop_membrane.*` IPC/query contracts are acceptable transitional adapters versus core-boundary drift
    - acceptable transitional adapters:
      - desktop HTTP route shapes in `philotic-web` for targets, target status, and target guests
    - unacceptable core-boundary drift:
      - new desktop-specific shared IPC variants where generic operator surface names can be used
      - new desktop-specific daemon worker/action families for operator surface execution
      - resuming remote agent inventory or operator chat on fresh `desktop_membrane.*` contracts
  - [ ] move desktop-specific operator feature assembly out of `aiua` bootstrap and shared IPC enums
    - first extraction foothold landed:
      - routed target status/guest queries now use shared `OperatorSurfaceQueryHandoff`
      - daemon worker role is now generic `management.operator_surface_query`
      - remote target status/guest execution no longer depends on desktop-named action aliases
      - shared target payload structs are now operator-owned, with `DesktopMembraneTarget*` names retained only as compatibility aliases
      - `operator.targets.agents` now exists as a bounded redacted target-agent inventory surface with local canonical reads and routed remote queries
    - still remaining:
      - move more operator feature assembly out of `aiua` bootstrap
      - reduce desktop-specific shared IPC view naming beyond the remaining compatibility aliases
  - [x] resume remote agent inventory only after the extracted operator seam exists
  - [ ] resume operator chat only after the extracted operator seam exists:
    - [x] define routed operator chat as a membrane ingress into the same canonical agent conversation path used by Telegram
    - [x] require local and remote chat turns to hand off through the router rather than desktop-specific transport choreography
    - [x] keep operator control-plane queries (`operator.targets.*`) separate from agent conversation surfaces
    - [x] preserve the same chat surface even if backing authority moves from hotel-local state to a graph-runner-backed authority
    - [x] land the first thin desktop adapter route:
      - `POST /api/mesh/targets/:target_node_id/agents/:agent_id/chat`
      - routes through `SendOperatorChatTurn`
      - reuses the canonical agent conversation path via routed `EmitTask`
    - [x] add first turn-event observation for operator chat replies
      - `SendOperatorChatTurn` now keeps listening for `turn_event` messages before the final reply
      - observed events are returned in the `OperatorChatTurnReply` envelope
    - [x] add live push reply streaming for operator chat while the turn is still in flight
      - desktop chat route now returns `202 Accepted`
      - `philotic-web` streams `operator_chat:turn_event`, `operator_chat:reply`, and `operator_chat:error` over `/ws` while the routed turn is active
      - `operator_chat:partial_reply` is now preserved as a non-terminal websocket frame too
    - [x] prove remote-hotel routed operator chat end-to-end
      - targeted `aiua` test now runs a local hotel, a remote hotel, and bidirectional bridge relays so a routed operator chat turn leaves local authority, reaches a remote agent, and returns through the local reply inbox
    - [x] let the lower conversation/model path emit optional partial reply chunks without treating them as final completion
      - `model-router` can now carry optional `partial_replies` in `model_result.result`
      - `philote` emits those as `partial_reply` frames before the final `send_reply`
      - default providers still emit no partial chunks unless they explicitly supply them
- [x] Make the target guest inventory seam explicit:
  - local target guest inventory uses canonical local hotel reads
  - remote target guest inventory now attempts a direct target-hotel management query over the mesh
  - when that query cannot complete, the membrane returns an explicit `remote-query-failed` state instead of inventing guest truth from registry observation
- [ ] Keep remote target truth hotel-owned:
  - no direct local cache treated as canonical
  - no browser-direct remote `aiua` protocol
  - no local mutation simulation standing in for target execution
- [ ] Define the first desktop-aware operator session posture flow for mesh-wide admin work.
- [ ] Define which remote actions require explicit target-scoped grants versus elevated session posture alone.
- [ ] Define the first high-trust remote action ceremonies for:
  - secret rotation
  - node shutdown/restart
  - mesh topology mutation
  - guest migration
- [ ] Define the first mesh-aggregate read models for:
  - node inventory
  - topology view
  - cross-node operation progress
  - grant/elevation status summaries
- [ ] Define the desktop UI asset source-of-truth split between:
  - `jaredlikes-desktop` source ownership
  - `philotic-web` embedding/runtime ownership
  - release pipeline provenance ownership
- [ ] Define the frontend development workflow for:
  - frontend-first local iteration
  - integrated membrane development
  - embedded release-shape verification
- [ ] Define the UI asset build contract:
  - deterministic `dist` expectations
  - asset manifest format
  - embedded build metadata surfaced at runtime
- [ ] Teach `philotic-web` embedding to record and expose:
  - UI build id
  - desktop source revision
  - asset hash
  - build timestamp
- [ ] Define release gating for embedded desktop assets so stable releases reject:
  - placeholder UI
  - stale `ui-dist`
  - missing provenance metadata
  - unknown/dirty desktop source state
- [ ] Define the CI/release workflow for acquiring desktop UI assets explicitly rather than relying on opportunistic sibling-repo discovery.
- [ ] Add integrated release-shape verification that proves the embedded assets served by `philotic-web` match the recorded asset manifest.
- [x] Write and pass `smoke-desktop-membrane` covering: lease acquisition, core REST endpoints, auth rejection (401 no-token + wrong-token), apartment not returning 200, and clean SIGTERM shutdown.
- [x] Add agent cognitive drill-down endpoints: `GET /api/agents/:id/roles`, `GET /api/agents/:id/rules`, `GET /api/skills`.
- [x] Add hotel config read endpoints: `GET /api/config`, `GET /api/config/telegram`, `GET /api/config/gemini`.
- [x] Add component management surface (Slice 3): `GET /api/components`, `GET /api/components/:guest_id`, `POST /api/components/:guest_id/enable|disable|restart` — via new `ListComponents`, `SetComponentActive`, `RestartComponent` IPC variants. Fixed serde untagged enum ordering hazard (`MemoryConfig` all-optional fields moved to last position).
- [x] Add graph runner instance inventory (Slice 4): `GET /api/graphs`, `GET /api/graphs/:graph_id` — via new `ListGraphInstances` IPC variant.
- [x] Add secret refs read-only inventory (Slice 4): `GET /api/secrets` — vault registry entries + known config-key ref presence flags; no values exposed.
- [x] Add skill assignment mutations (Slice 4): `POST /api/agents/:agent_id/roles/:role_name/skills`, `DELETE /api/agents/:agent_id/roles/:role_name/skills/:skill_name` — management-role bypass added to `AssignSkill`/`RevokeSkill` authority checks.

## Next Project: Tool Assembly and Routed Execution

- [ ] Review [TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md).
- [ ] Review [TOOL_MANAGEMENT_PLANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_MANAGEMENT_PLANE_PROPOSAL.md).
- [ ] Review [RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md).
- [x] Introduce a first-class `ToolAssembly` model with model-facing tool definitions and runtime-facing execution routes.
- [ ] Formalize the system tool management plane in the Context Graph:
  - known tool runners
  - runner incarnations
  - abstract tools
  - discovered hotel environments
  - agent default toolsets
  - session narrowing/hiding rules
- [x] Move real tool execution out of `philote` and behind routed tool runners/toolset components.
- [ ] Add runner readiness/materialization checks during tool assembly.
- [ ] Add environment-aware runner routing and materialization policy so tools can target non-IPC execution environments when needed.
- [x] Keep local config/session mutation commands in `philote`, but externalize real tool execution.
- [x] Let session-scoped allowed runner incarnations derive visible tools and preassembled execution routes.
- [x] Add execution taxonomy for routed tools:
  - `local_agent`
  - `capability`
  - `pinned`
- [x] Land the first basic tool-family split:
  - `session.status` as `local_agent`
  - `echo` as `capability`
  - `workspace.list` and `workspace.read` as `pinned`
- [ ] Return to skill design after tool assembly and routed execution boundaries are in place.
- [x] Add agent-specific runner routing:
  - preferred environment
  - preferred hotel/node
  - preferred runner
  - route selection reason in `tool_assembly`
- [ ] Add runner fallback policy and smarter reroute behavior when the preferred route cannot materialize or should be bypassed.

## Next Project: Personality and Context

- [ ] Review [PERSONALITY_AND_CONTEXT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERSONALITY_AND_CONTEXT_PROPOSAL.md).
- [ ] Review [ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md).
- [ ] Review [HEURISTIC_MIND_AND_CONTEXT_PAPER.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HEURISTIC_MIND_AND_CONTEXT_PAPER.md).
- [x] Refactor `philote` prompt assembly into turn-time projection functions:
  - `project_agent_self`
  - `project_user`
  - `project_knowledge`
- [ ] Make skill and tool exposure goal-scoped turn projections instead of full inventory dumps when the goal is clear.
- [ ] Compose those projections with explicit context layers:
  - soul
  - identity
  - user context
  - memory context
  - session context
- [x] Add initial agent-level personality fields for:
  - `soul_text`
  - `identity_text`
  - `user_context_text`
  - `memory_summary`
- [ ] Define the import path from `openclaw.json`.
- [ ] Import legacy workspace bootstrap files when present:
  - `SOUL.md`
  - `IDENTITY.md`
  - `USER.md`
  - `MEMORY.md`
- [ ] Define how user continuity should follow an identified person across sessions.
- [ ] Define memory retrieval layers for:
  - context graph
  - knowledge graph
  - hippocampal / episodic memory
  - heuristic memory backends
  - tool-runner local indexes
- [ ] Define projection profiles for:
  - conversational agents
  - workers
  - subagents
- [ ] Keep the first implementation slice personality-first; do not try to solve the full memory backend story in the same change.
- [ ] Build the first ZeroClaw/OpenClaw bridge slice:
  - [ ] import one agent from `openclaw.json`
  - [x] ingest `SOUL.md`, `IDENTITY.md`, `USER.md`, and `MEMORY.md`
  - [x] store them as Philotic compatibility inputs
  - [x] expose the imported Jane profile through the canonical session snapshot
  - [ ] materialize the imported agent in the Philotic web
  - [ ] verify recognizable identity continuity

## Next Work Item: Muninn Heuristic Memory Experiment

Seam IDs: `wider-client-adoption`, `philotic-native-memory-integration`

- [ ] Review [MUNINN_MEMORY_PROTOCOL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MUNINN_MEMORY_PROTOCOL_PROPOSAL.md).
- [ ] Review [MUNINN_CLIENT_MEMORY_PROTOCOL.md](/Users/jaredlikes/code/philotic-stack/docs/reference/MUNINN_CLIENT_MEMORY_PROTOCOL.md).
- [x] Validate the local Muninn MCP handshake and core tool calls.
- [x] Establish a default Muninn retrieval/write-back habit for Codex.
- [x] Create a shared helper script for Muninn MCP transport and tool invocation.
- [x] Create a shareable client skill/instruction package for adopting the helper-backed Muninn protocol.
- [ ] Wire the helper into at least one additional cognitive client beyond Codex.
- [ ] Measure whether Muninn materially improves continuity, personalization, and decision recall over repeated sessions.
- [ ] Decide whether Muninn remains an external heuristic memory service or should inform a future Philotic-native memory layer.

## New Project: Dev Engine Optimization

- [x] Review [DEV_ENGINE_OPTIMIZATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DEV_ENGINE_OPTIMIZATION_PROPOSAL.md).
- [x] Port 9 specialized skills from `~/.codex/skills` to `skills/` and make them repo-local.
- [x] Establish mandatory session bootstrap in `CLAUDE.md` and `AGENTS.md`.
- [x] Implement `just engine-check` for one-command verification of Muninn, repo-local bootstrap assets, and the cargo check/test baseline.
- [x] Implement `just session-start` as the mandatory Muninn bootstrap gate for meaningful sessions.
- [x] Require explicit user/operator approval before continuing without Muninn when the bootstrap gate fails.
- [ ] Deploy Muninn "Truth Cache" to `vps-jane` with automated sync from local.
- [ ] Formalize semantic "Optimization Loop" to update `AGENTS.md` rules based on recurring "Reality Gaps."

## New Project: Homebrew Distribution

- [x] Research current Homebrew distribution guidance for taps, formula acceptance, and bottles.
- [x] Inspect Philotic binary/release shape and identify packaging constraints.
- [x] Write [HOMEBREW_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HOMEBREW_DISTRIBUTION_PROPOSAL.md).
- [ ] Decide the first public Homebrew binary name and install surface.
- [ ] Add tagged release automation for the chosen public binary.
- [ ] Create a dedicated tap repository and first formula.
- [ ] Add bottle automation for supported platforms.

## Deferred Design Threads

- [ ] Agent workflow formalization: adopt a standing Codex process for context gathering, slice sizing, verification ladders, watched live runs, proposal disposition updates, per-slice commit/push discipline, and assumption-vs-reality capture.
- [ ] Proposal lifecycle hygiene: add concise `Disposition` sections and task/work-item links to active architecture proposals.
- [ ] Skill/rules optimization loop: define a lightweight end-of-slice review for instruction gaps, reusable skills, and recurring reality-gap lessons.
- [x] Add executable workflow commands for the trusted vertical slice and operator checklist.
- [ ] Command Center / architect continuity: define how architecture-impact work should be surfaced to Aria once the new home is ready.
- [ ] Fresh onboarding flow: design repo/bootstrap onboarding from scratch for a new operator or agent entering Philotic.
- [ ] `openclaw.json` ingestion: define a migration/import path that can consume legacy agent manifests and materialize Philotic agents.
- [ ] Context graph deployment model: decide local-first vs cloud-backed vs hybrid graph ownership, sync, and operational model.
- [ ] Context graph decentralization: decide how much of the graph can be replicated/federated across hotels versus kept locally authoritative.
- [ ] Perimeter egress control (`egress-policy-object`): define the canonical outbound egress policy object and finding schema.
- [ ] Perimeter egress inventory (`outbound-classification`): classify current direct outbound HTTP paths into perimeter-controlled, temporary exceptions, and future violations.
- [ ] Perimeter egress first implementation: route one non-model outbound HTTP path through a perimeter-controlled boundary while keeping model/provider egress as an explicit exception for now.
- [ ] Review [MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md).
- [ ] External membrane transport contract (`a2a-membrane-contract`, `nostr-membrane-contract`): define the first normalized inbound/outbound envelope and session-binding inputs for `A2A` / `Nostr` membranes.
- [ ] External principal trust records (`transport-edge-trust-gates`): define the first shared trust record for external agent peers, pubkeys, and relays, including trust classes, allowed capability classes, quarantine state, and policy refs.
- [ ] Membrane sentinel findings (`membrane-sentinel-checks`): define the first `SentinelFinding` schema and enforcement modes (`allow`, `allow_audit`, `deny`, `quarantine`, `require_review`) for membrane-edge auth, replay, schema, attachment, capability, destination, and anomaly checks.
- [ ] External membrane v1 choice: choose one narrow first transport slice (`membrane.nostr` addressed-event/DM mode or one trusted-peer `membrane.a2a` slice) instead of proving both at once.
- [ ] Approval UX evolution (`session-preapproval-ux`): add `/preapprove`, `/approval status`, `/approval reset`, and richer session policy editing for constrained transports like Telegram.
- [ ] Review [TELEGRAM_INTEGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_INTEGRATION_PROPOSAL.md).
- [ ] Review [TELEGRAM_POLL_LEASE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_POLL_LEASE_PROPOSAL.md).
- [ ] Review [PERIMETER_EGRESS_CONTROL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERIMETER_EGRESS_CONTROL_PROPOSAL.md).
- [ ] Review [VOICE_MACHINE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/VOICE_MACHINE_PROPOSAL.md).
- [x] Telegram slash-command elevation (first slice): `/ping` handled in `membrane` before agent-core — `handle_membrane_command` short-circuits the `EmitTask` dispatch and replies directly.
- [ ] Telegram slash-command elevation (next): `/new` resets session_id in membrane (start fresh conversation without round-trip); `/help` lists available commands from membrane directly.
- [x] Telegram bot command registration/UI: call Telegram `setMyCommands` from `membrane` startup so supported slash commands show up in the Telegram client command UI instead of existing only as hidden transport behavior.
- [x] Telegram poll ownership (first slice): add a hotel-owned poll lease per bot token fingerprint so only one local membrane long-polls `getUpdates` for a token at a time, and fail closed when lease acquisition is denied.
- [x] Telegram poll ownership (authority slice): anchor agent identity to `authority_hotel` and deny poll-lease acquisition when the current hotel is not that agent's home authority.
- [x] Telegram poll delegated authority (transitional slice): allow a non-home hotel to acquire the poll lease only when the agent identity bundle explicitly lists that hotel in `telegram_poll_delegate_hotels`.
- [x] Telegram poll failover (renewal slice): teach `membrane` to renew the poll lease, expire stale owners, and stop polling immediately on lost renewal or stale epoch.
- [x] Telegram poll graceful release: explicitly release the poll lease on intentional membrane shutdown instead of relying only on disconnect cleanup.
- [x] Telegram poll lease smoke: prove only one of two membranes sharing a bot token polls at a time, then prove standby takeover after release or expiry.
- [ ] Telegram poll lease mesh authority: move poll-lease truth from local hotel runtime state toward canonical mesh-visible authority so two hotels cannot race the same bot token.
- [ ] Telegram approval card UX (`approval-card-ux`): include request IDs, tool/action names, args summaries, and resolution messages in a more native Telegram approval experience.
- [x] Telegram streaming Layer 1: add typing indicator heartbeat to `membrane` — `ActiveTurn` map, `sendChatAction(typing)` on dispatch, 4-second refresh loop, cancel on `send_reply`.
- [ ] Telegram streaming: add message length chunking to `membrane` — split at paragraph boundaries before `sendMessage`, shared `send_formatted_text` helper.
- [x] Telegram streaming Layer 2 protocol: add `TurnEventPayload` to `agent-core/src/protocol.rs` and `emit_turn_event` helper to `AgentRuntime`; emit `waiting_tool`, `waiting_approval` events back to membrane via the existing `EmitTask` path.
- [x] Telegram streaming Layer 2 membrane: handle `action = "turn_event"` in `InboundTask` dispatch — maintain or cancel typing heartbeat per event type; stop typing on `waiting_approval`.
- [x] Telegram streaming partial reply: teach the lower conversation/model path to emit real `action = "partial_reply"` content chunks, then implement edit-based progressive delivery in `membrane`.
  - [x] preserve `partial_reply` as a non-terminal transport frame across operator chat and Telegram membrane handling so future producers do not get mistaken for `send_reply`
  - [x] allow `model-router` / `philote` to carry optional `partial_replies` through the lower conversation path
  - [x] edit the active Telegram draft message on `partial_reply` and final text completion when possible
  - [ ] upgrade providers from optional batch partials to native incremental generation
- [ ] Voice machine design: define STT, TTS, speech-to-speech, transcript generation, and media artifact/session handling.
- [ ] Nostr communication-plane investigation (`nostr-membrane-contract`): evaluate Nostr as a decentralized/event-native membrane with relay trust classes, addressed-event gating, signature verification, replay defense, and perimeter/sentinel integration before any implementation.
- [ ] A2A membrane investigation (`a2a-membrane-contract`): evaluate `A2A` as an external agent interoperability membrane with explicit peer trust records, bounded capability exposure, approval semantics for privileged actions, and no inheritance of internal mesh trust.
- [ ] Tool runner lifecycle policy (`runner-materialization-policy`): define idle retention, sleep/teardown timing, wake-up thresholds, and environment-specific materialization rules for routed tools.
- [ ] Runner artifact plane: define builder trust, sandboxing, testing, signing, release, and distribution policy for executable tool runners.
- [ ] Memory consolidation / dreaming: define how short-term session state becomes long-term memory, including sleep/dream cycles, compaction, and candidate memory backends such as `scryper/miniminddb`.

## MVP 1: Single-Node Mesh & Basic Tools

- [x] Scaffold the `ansible-mesh-core` crate.
- [x] Define the UDP `BeaconMessage` transport envelope.
- [x] Implement the beacon daemon listener.
- [x] Implement the `AgentBundle` struct and a basic `AgentRuntime`.
- [x] Build a simple `ToolInvoker` with local mock tools.
- [x] Set up a hard-coded `node_capabilities.json` for testing.

## MVP 2: Multi-Node Mesh + Model Manager + Ansible

- [x] Implement `node_capabilities` sync and simple health/heartbeat.
- [x] Build the initial Component Model Manager node exposing `model.manager.list` and `model.manager.route`.
- [x] Integrate the Rust Ansible port as `ansible.mesh.*` tools on a designated node.
- [x] Update the Zeroclaw orchestrator adapter to call remote tools (e.g., `mesh.tool_call("ansible.mesh.broadcast")`).

## MVP 3: Context Graph & Edge Client

- [x] Scaffold the memory primitives (`MemoryApartment` struct and queries) inside `crates/ansible-mesh-core/src/graph.rs`.
- [x] Expose `memory.read@1`, `memory.write@1` over the ToolInvoker interface.
- [x] Define `ios_capabilities.json` outlining HealthKit, iOS Contacts, and a localized on-device Swift LLM `ModelRef`.
- [x] Refactor the existing ZeroClaw `src/memory/mod.rs` to query the `MeshAdapter` for episodic persistence.

## MVP 4: Infrastructure Provisioning (Red Hat Ansible)

- [x] Initialize `ansible/` directory structure (inventory, playbooks, roles).
- [x] Create `roles/mesh_node/` to install dependencies (Rust, systemd).
- [x] Create `deploy_mesh_node.yml` playbook to compile and run ZeroClaw `mesh run` as a service.

## Phase 1: Making it Ours (The Membrane Workspace)

- [x] Initialize new Rust crates to build the Philotic architecture alongside the legacy code.
- [x] Formalize the Cargo Workspace (Monorepo) structure:
  - `crates/ansible` (The Hotel Manager / local UDP/IPC Event Bus).
  - `crates/membrane` (The primary CLI and routing logic).
  - `crates/philotic-ipc` (The IPC client library for child processes).
- [x] Port the essential `ansible-mesh-core` MVP 3 logic into the new `crates/ansible` bin.
- [x] Leave the legacy `src/` monolith untouched for reference and gradual migration.

## Phase 2: Universal Materialization (The Hotel, Guests, & Context Graph)

- [x] Implement a concrete, disk-backed `ContextGraph` store (e.g. SQLite/RocksDB/Sled) to hold the entire system configuration, identities, and memory apartments.
- [x] Scaffold the `PhiloticClient` (IPC) to allow external Guests (MCP Wrappers, Agent Personas) to connect to the local Ansible.
- [x] Connect the `Membrane` Telegram poller to the Philotic Web via the new IPC trait.
- [x] Close the UDP Request/Response loop (Ansible Echoes back `MsgType::Result` to Membrane).
- [x] Write a sample Python and Rust MCP wrapper that registers tools dynamically with the local Ansible.
- [x] Implement Agent Materialization: Refactor the runtime to spawn child OS processes dynamically from graph data.

## Phase 3: The End-to-End Philotic Stack (Telegram -> Agent -> Model)

We will materialize the core ZeroClaw pipeline as three completely independent binaries that communicate exclusively over the Ansible's UDP IPC:

### 1. The Gateway (Telegram Membrane)

- [x] Port the legacy `TelegramChannel` struct from `src/channels/telegram.rs` into the `crates/membrane` binary.
- [x] Connect the `Membrane` Telegram poller to the Philotic Web via the `UdpPhiloticClient`.
- [x] Ensure inbound messages over Telegram are translated to `IpcRequest::EmitTask` and routed to the Agent persona.
- [x] Refactor the long-polling loop to read the `bot_token` via an IPC config pull rather than the static `config.toml`.

### 2. The Persona (Agent Core)

- [x] Create a new `crates/agent-core` binary in the workspace.
- [x] Implement the core agent loop (receiving a prompt, building context) checking in as a Guest.
- [x] When the agent receives a task from Telegram, it queries the Model capabilities via an IPC `EmitTask`.

### 3. The Mind (Model Router)

- [x] Create a new `crates/model-router` binary in the workspace.
- [x] Implement the Gemini API payload constructor for text generation.
- [x] Subscribe to Model invocation tasks over IPC, trigger inference, and pass the text back to Membrane.uter receives an inference task from the Agent, it calls the Gemini API and routes the text response back via IPC.

## Phase 4: The Philotic Split & Metaphor Visualization

Now that the End-to-End Philotic architecture is complete, we need to separate it from the legacy monolith and create visual documentation.

### 1. Repository Separation

- [x] Create a new repository for the Philotic architecture.
- [x] Migrate the `aiua`, `membrane`, `philote`, `model-router`, `philotic-ipc`, and `ansible-mesh-core` crates to the new repository.
- [x] Ensure the legacy ZeroClaw/OpenClaw code remains accessible in the original repository as a reference for migrating future capabilities (tools, MCPs).

### 2. Veo3 Metaphor Video

- [x] Brainstorm the visual concepts for the Veo3 video explaining the system metaphors in motion.
- [x] Draft a storyboard artifact documenting the scenes (The Universal Materialization, The Hotel, The Ansible, The Guests).
- [x] Refine prompts for Veo3 video generation based on the storyboard.

## Documentation Process And Architecture Truth

Seam IDs: `active-proposal-frontmatter-rollout`, `architecture-doc-metadata-rollout`, `proposal-disposition-rollout`

- [x] Define architecture documentation domains as a lightweight organization layer instead of adding deep proposal folders immediately.
- [x] Create a living architecture status snapshot that acts as the single source of truth for implemented behavior, transitional seams, and currently active work.
- [x] Make the docs index point to an architecture hub and status snapshot before deeper reference or proposal reading.
- [x] Define a lightweight tagging and frontmatter strategy for active architecture/process docs, including controlled `domain`, `doc_type`, and `status` vocabularies.
- [x] Define stable seam IDs in [docs/architecture/SEAM_REGISTRY.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SEAM_REGISTRY.md) so proposals and the task surface can share one seam vocabulary.
- [ ] Apply the frontmatter schema to the highest-value active docs first:
  - [x] [docs/architecture/ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md)
  - [x] [docs/architecture/ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md)
  - [x] current active repo-local proposal set
- [x] Align model-controller and key-vault proposal docs with the same frontmatter discipline in this repository.
- [x] Create a scope-first architecture domain catalog for navigating current truth and active proposals by domain.
- [x] Decide that seam docs remain exception-based artifacts and only graduate from proposal + registry + task surfaces when cross-cutting complexity or repeated confusion justifies their own boundary doc.
- [x] Tighten [docs/architecture/ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md) against current execution-transport and current session-authority reality.
- [x] Audit historical docs and clearly mark any remaining non-authoritative architecture narratives as legacy or historical.
