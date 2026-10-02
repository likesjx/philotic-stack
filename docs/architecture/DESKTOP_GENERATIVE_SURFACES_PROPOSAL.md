---
title: Desktop Generative Surfaces — Philote-Authored UI as Data (A2UI over the Routed Operator Stream)
doc_type: proposal
domain: operator-control-plane
status: accepted
disposition: accepted
last_updated: 2026-10-01
tags:
  - desktop
  - generative-ui
  - a2ui
  - ag-ui
  - philotic-web
  - operator-chat
  - approval-ux
  - skilldag
  - telegram-mini-app
  - apple-webview
  - mcp-apps
related_docs:
  - DESKTOP_WORKSPACE_COMPONENTS_PROPOSAL.md
  - DESKTOP_MEMBRANE_PROPOSAL.md
  - ROUTED_OPERATOR_CHAT_PROPOSAL.md
  - APPROVAL_UX_PROPOSAL.md
  - DATA_DRIVEN_TOOL_GRANTS_PROPOSAL.md
  - NATIVE_APPLE_APP_PROPOSAL.md
  - PHILOTIC_WEB_HARDENING_PROPOSAL.md
  - ARCHITECTURE_STATUS.md
task_refs:
  - docs/task.md
proposal_id: desktop-generative-surfaces
implements:
  - philote-app-publication
  - desktop-membrane-boundary
implemented_by: []
active_seams:
  - surface-schema-and-types
  - surface-render-tools
  - surface-stream-projection
  - surface-renderer-catalog
  - surface-action-return
  - surface-persistence-rehydrate
  - surface-agui-adapter
  - approval-action-ids
  - surface-web-renderer
  - surface-https-edge
  - surface-telegram-miniapp
  - surface-apple-webview
  - surface-mcp-app
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# Desktop Generative Surfaces — Philote-Authored UI as Data

## Goal

Let a philote build an interface for the operator inside `jaredlikes-desktop` (Likes OS) without ever shipping code to the browser: the philote emits a declarative UI description, the desktop renders it with its own component catalog inside a normal workspace window, and the operator's clicks and form entries flow back into the same conversation the philote is already having.

This proposal answers the operator's ask of 2026-09-14 ("investigate creating an A2UI/AG-UI application within jared-desktop so that my philotes can create an interface for me") with a concrete shape, a protocol decision, and an ordered set of seams.

## Amendment 2026-10-01 — Philote Surfaces Across Every Membrane (accepted)

The operator widened the ask from "an interface in jared-desktop" to an A2UI/AG-UI strategy for the philotes as a whole, and accepted the amended plan on 2026-10-01. **Where this section conflicts with the 2026-09-14 text below, this section wins.** The 09-14 text stays as the investigation record.

### What changed and why

1. **One web renderer, many hosts.** The A2UI renderer is a small bundle **owned by philotic-stack and served by `philotic-web`** at `/s/:surface_id`, not code inside `jaredlikes-desktop`. Each host plugs into the same renderer through a small action bridge:

   | Host | Loads the renderer from | Actions return via |
   | --- | --- | --- |
   | Desktop window (`jaredlikes-desktop`) | `philotic-web` (embed) | `SendOperatorChatTurn` `ui_action` |
   | Apple app (`WKWebView`) | `philotic-web` (server-loaded, operator decision 2026-10-01) | `WKScriptMessageHandler` → Swift → the app's existing authenticated `EdgeMessage` connection. The page holds **no** web credential of its own |
   | Telegram Mini App | `philotic-web`, launched from the emitting philote's own Telegram bot (`web_app` inline button, private chats only) | `initData`-authenticated POST into the same chat path |
   | MCP App (`ui://` resource from `membrane-mcp`) | the same fixed renderer page, packaged as one MCP App | MCP Apps host bridge → gateway `tools/call` |

   A fixed, Philotic-owned A2UI→HTML renderer keeps the "UI is data, never code" invariant: philotes still only emit A2UI data. Native SwiftUI rendering becomes optional polish. The uncommitted `jaredlikes-desktop` work leaves the critical path; it only gates the desktop embed.
2. **Persistence moves ahead of every non-desktop host.** Mini Apps and webviews open a surface *later, by id*; they are not on the live `/ws` stream (which only exists during a desktop-initiated turn). Hotel-owned surface records plus `GET /api/surfaces/:id` are a prerequisite, not the last slice.
3. **Delivery outcome is part of say-do.** Every membrane reports one of `displayed`, `offered_link` (Telegram: an Open button was sent), or `rejected` (cannot meet the surface's minimum fidelity). The philote's reply must match the reported outcome ("I sent you a link to open it", never "I showed you the form" when only a link went out). No half-rendering anywhere.
4. **Native lowering stays for small things.** Approval cards and model pickers stay native Telegram inline keyboards (instant, no page load). Forms, tables, and dashboards go to the web renderer. Voice gets a spoken summary plus spoken choices. Discord components are net-new and out of the first slices.
5. **Compact action ids everywhere.** Telegram `callback_data` is at most 64 bytes, so every actionable element carries a short Philotic id; context stays hotel-side. A2UI v1.0 may add a native action id (unconfirmed as of 2026-10-01); ours is required regardless. Today the approval id exists hotel-side (`ApprovalRequest.approval_id`) but never reaches any channel, so a stale Approve tap resolves whatever is pending now and a stale **Trust for session** tap grants session-wide pre-approval; `approval-action-ids` fixes that first.
6. **Transport-agnostic reachability.** Mini Apps need HTTPS. The first slice uses **Tailscale Serve** (`*.ts.net` certificate; the phone is on the tailnet; Telegram Web's browser iframe is unsupported), which keeps `philotic-web` (the only authz boundary) off the public internet. The renderer base URL is configuration (`web_public_base_url`), not a Tailscale assumption, so the operator can move to the native Philotic hotel VPN (outbound dial + vps relay) without touching hosts or philotes.
7. **AG-UI becomes a membrane investigation.** Operator direction 2026-10-01: investigate AG-UI as a possible membrane, an external-client surface in the same family as `membrane-mcp`, so AG-UI clients (CopilotKit and others) can drive a philote and receive A2UI surfaces as AG-UI events. Rust crates `ag-ui` / `ag-ui-a2ui` are at `0.5.0-alpha.3` (2026-09-28); the investigation decides whether to depend on them or hand-map the event vocabulary.

### Telegram Mini App authentication rules

`initData` is a new way into `philotic-web`, the only authz boundary, so all of these are required:

- Verify Telegram's **Ed25519 third-party signature** against the bot id. This needs no bot token, so no persona bot tokens are copied into `philotic-web`.
- Reject stale `auth_date` (short freshness window).
- Allowlist the operator's Telegram user id.
- Require that the bot id belongs to one of the operator's philote bots (from the hotel's membrane bindings).
- The Mini App gets a short-lived session scoped to the surface it was launched for, not a general operator session.

### Accepted seam order (supersedes the 09-14 order)

A0 `approval-action-ids` → S0 `surface-schema-and-types` → S1 `surface-persistence-rehydrate` + `surface-render-tools` → S2 `surface-web-renderer` + `surface-https-edge` → S3 `surface-telegram-miniapp` ∥ `surface-apple-webview` → S4 `surface-stream-projection` (desktop embed) + `surface-action-return` ∥ `surface-mcp-app` → later: SwiftUI-native rendering, voice lowering, Discord components. `surface-agui-adapter` (AG-UI membrane investigation) runs independently.

Superseded: `surface-renderer-catalog` (the renderer no longer lives in `jaredlikes-desktop`; its catalog work moves into `surface-web-renderer`).

### AG-UI membrane investigation (2026-10-02, seam `surface-agui-adapter`)

**Recommendation: defer building until an AG-UI client is actually wanted.** When it is, build it as an SSE route inside `philotic-web`, not as a standalone `membrane-agui` guest. The route would be `POST /api/agui/agents/:agent/run`, and events would be hand-mapped with serde, with no runtime dependency on the `ag-ui` crates.

**Why:**
- AG-UI adds nothing the A2UI plan lacks for the operator's own hosts. Its value is third-party clients (CopilotKit), and none exist yet.
- `philotic-web` is the only audited authz boundary, so a separate guest would be a second one.
- The Rust crates `ag-ui` and `ag-ui-a2ui` are an unaffiliated `0.5.0-alpha.3` (2026-09-28, a few hundred downloads). `ag-ui-core` and `ag-ui-client` have been stale at 0.1.0 since 2025-08.

**Protocol facts to build against:**
- **Interrupts are not a mid-stream pause.** A run ends with `RUN_FINISHED{outcome:{type:"interrupt", interrupts:[{id, reason, message, responseSchema}]}}`, and a new run on the same `threadId` resumes with `resume:[{interruptId, status, payload}]`. A Philotic approval maps to one interrupt whose `id` is the A0 approval reference.
- **A2UI rides as activity events.** It goes out as `ACTIVITY_SNAPSHOT{activityType:"a2ui-surface"}`, and clicks return in `forwardedProps.a2uiAction`.
- **Transport and auth.** Transport is HTTP POST with SSE; auth is up to the host.
- **Frontend-declared client tools are rejected.**

**Mapping:**
- `threadId` → philote session
- `runId` → turn
- turn_event / partial_reply / send_reply → lifecycle, text and step events

**Seams when activated:** `agui-event-model` → `agui-run-endpoint` → `agui-interrupt-resume` → `agui-a2ui-activity` → `agui-scoped-credential`.

**Still unverified:** the exact wire `type` strings, the `a2ui_operations` payload schema, and real CopilotKit compatibility.

### Open after acceptance

- Which personas get `desktop.surfaces` by default (09-14 open question 1).
- Whether to commit the uncommitted `jaredlikes-desktop` work. No longer blocking; it gates only the desktop embed.
- Unverified research to confirm before relying on it: whether A2UI v1.0 adds an action id; whether `@a2ui/web_core` still depends on Lit; AG-UI's interrupt event name.

## Investigation Summary (2026-09-14)

What exists today, verified in code:

- **The stream already exists.** `philotic-web` streams in-flight philote turn pushes to the browser over the existing `/ws` broadcast as `operator_chat:turn_event`, `operator_chat:partial_reply`, `operator_chat:reply`, `operator_chat:error` (`crates/philotic-web/src/serve.rs`, `stream_operator_chat_turn`). Its catch-all arm forwards **any unrecognized philote action verbatim** as `operator_chat:event`. That is an AG-UI-shaped event stream in all but name.
- **The desktop already has the matching seam.** `src/services/aiua-service.js` remaps known websocket frame types onto the desktop event bus and forwards unknown frames as `aiua:ws-event` with the raw envelope. Both ends of the pipe have an untyped extension slot that needs no transport work.
- **Actions have a canonical return path.** `POST /api/mesh/targets/:node/agents/:agent/chat` submits a routed `SendOperatorChatTurn` into the same conversation plane Telegram uses ([ROUTED_OPERATOR_CHAT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROUTED_OPERATOR_CHAT_PROPOSAL.md)).
- **There is no rich outbound payload type anywhere.** `OutboundReply` (`crates/membrane/src/envelope.rs`) is `Text | StreamingToken | Error | ApprovalRequired`. Telegram "cards" and numbered approval cards are formatted text. A surface payload is new.
- **Tools are data-driven.** A new tool is a `ToolDefinition` in `crates/philote/src/catalog.rs` plus a `handle_tool` arm in `crates/philote/src/tool_exec.rs`, granted through the SkillDAG, not a hardcoded grant.
- **The desktop substrate has a slot reserved for this.** [DESKTOP_WORKSPACE_COMPONENTS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DESKTOP_WORKSPACE_COMPONENTS_PROPOSAL.md) already defines a customization/publication tier for philote-published apps and widgets, governed by catalog and policy "rather than upload JavaScript and pray", and names `philote-app-publication` as its open seam. `desktop-membrane` requires every field on screen to be attributable to a source hotel and a freshness window.
- **Desktop primitives are thin.** Nine Shadow-DOM elements exist (`ui-button`, `ui-input`, `ui-checkbox`, `ui-toggle`, `ui-select`, `ui-slider`, `ui-number-input`, `ui-dialog`, `ui-toolbar`). There is no list, table, or card primitive; the Aiua panels hand-roll cards as template strings and set `innerHTML` without a shadow root, so their `:host` rules never apply. `ajv` is already a desktop dependency.
- **The desktop working tree is ahead of its own history.** The committed Aiua app uses URL-param bearer tokens; the uncommitted working tree has replaced that with cookie-session probing, an `operator-session-gate`, and a login dialog. Anything built here must target the uncommitted cookie model, and that work should be committed before this proposal's desktop slices land on top of it.

What the two candidate protocols are, as of 2026-09-14:

| | A2UI (Google, now `a2ui-project/a2ui`) | AG-UI (CopilotKit) |
| --- | --- | --- |
| What it is | A declarative **UI payload schema**: a flat component tree plus a JSON data model, rendered by a client-owned catalog | An **event stream protocol** between an agent run and a frontend: lifecycle, text, tool calls, state snapshots and JSON-Patch deltas, human-in-the-loop resume |
| Current version | v0.9.1 (`createSurface`, `updateComponents`, `updateDataModel`, `deleteSurface`; client→agent `action`). The v0.8 names `beginRendering`/`surfaceUpdate`/`dataModelUpdate` are obsolete | ~33 event types over SSE, WebSocket, or protobuf; `RunAgentInput{threadId, runId, messages, tools, state, forwardedProps, resume}` |
| Safety model | UI is data, never code. The agent can only reference a client-controlled catalog by `catalogId` | Transport only; carries whatever the agent emits |
| Browser renderer | `@a2ui/web_core` and `@a2ui/lit` depend on Lit 3. No first-party vanilla renderer exists despite the site's "plain JavaScript" wording | `@ag-ui/client` runs without React (rxjs, zod, fast-json-patch) |
| Rust | No SDK. JSON Schema files ship in `specification/v0_9/json/` and are suitable for `typify` codegen | `ag-ui-core` / `ag-ui-client` 0.1.0, community tier inside the official monorepo, client-oriented |
| Human-in-the-loop | v0.9 `action` carries **no action id**; correlation is by `name` + `sourceComponentId` only. Action ids arrive in the v1.0 candidate | `resume` / interrupt entries on `RunAgentInput` are mature |
| Composition | Designed to ride any transport, including AG-UI `CUSTOM` events, MCP Apps, and A2A | Sits below A2UI, Open-JSON-UI, and MCP Apps |

Alternatives considered and set aside: MCP Apps / MCP-UI and the OpenAI Apps SDK are sandboxed-iframe HTML surfaces, which is code, not data, and would reopen the "upload JavaScript and pray" door the workspace proposal closed. Vercel `json-render` and Thesys C1 are declarative-JSON-safe siblings of A2UI but are single-vendor and framework-shaped.

## Core Recommendation

**Adopt A2UI v0.9 as the surface payload schema. Carry it on the routed operator stream Philotic already has. Do not adopt the AG-UI SDKs now; keep the frame vocabulary AG-UI-mappable so an adapter is a later, thin seam.**

Concretely:

1. **UI is data.** A philote can only emit A2UI messages against a Philotic-owned catalog (`catalogId: "philotic.desktop.v1"`). The hotel validates every message against the JSON Schema before it leaves the philote; the desktop validates again with `ajv` before it renders. Invalid surfaces fail loud on both sides and never render partially.
2. **One conversation plane.** Surfaces are emitted as philote turn events inside a session and reach the desktop through the existing `operator_chat:*` stream; user actions return as structured turns through `SendOperatorChatTurn`. Telegram and desktop stay two membranes over one conversation, as ROUTED_OPERATOR_CHAT requires. No second "UI RPC" family is created.
3. **A surface is a workspace-app-tier record, not a window accident.** Each surface has an owner philote, a source hotel, a session, a freshness stamp, and a lifecycle. The hotel owns the record; the desktop renders it in a window and can rehydrate it after reload.
4. **Rendering is a hand-written vanilla renderer over the A2UI schema, not the Lit packages.** The desktop is vanilla custom elements by design and `@a2ui/web_core` pulls Lit in. The renderer maps the A2UI standard catalog names onto desktop `ui-*` primitives plus three new primitives (`ui-card`, `ui-list`, `ui-table`) and refuses anything outside the catalog. Owning the renderer is also what makes the catalog a real allowlist.
5. **Actions correlate by Philotic ids, not by A2UI's v0.9 gap.** Every `Button`/form action the philote declares carries a Philotic `action_id` in the A2UI `context` object, and the desktop echoes it back. When the operator's click resolves a pending approval, the same id ties the click to the approval record. This is how the Telegram numbered approval card and the desktop approval card become one contract with two renderings ([APPROVAL_UX_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/APPROVAL_UX_PROPOSAL.md)).
6. **AG-UI is a projection, later.** The internal frame vocabulary maps one-to-one onto AG-UI events (`turn_event` → `STEP_*`/`TOOL_CALL_*`, `partial_reply` → `TEXT_MESSAGE_CONTENT`, `reply` → `RUN_FINISHED`, surface messages → `CUSTOM{name:"a2ui", value}`). If an external client (CopilotKit, another IDE, a second desktop) ever needs AG-UI, `philotic-web` grows one SSE adapter route; nothing in the philote or hotel changes.

## Why Not the Other Two Options

- **A2UI over a bespoke new transport** would reinvent the lifecycle, error, and approval semantics the routed operator chat stream already carries, and would create the second conversation plane the desktop membrane proposals explicitly forbid.
- **AG-UI only, with client-defined tool-call rendering** is a real competitor. It skips the A2UI catalog and lets each tool call render its own widget. It loses the property that matters most here: a philote-authored *interface* the operator did not pre-build. With tool-call rendering the desktop must already know every widget; with A2UI the philote composes from primitives. AG-UI's stronger human-in-the-loop story is real but is covered by the `action_id` rule above.

## Wire Shape

### Philote → desktop (server-to-client)

The philote emits an ordinary turn event whose `event` is `ui_surface` and whose payload is one A2UI v0.9 message envelope plus Philotic attribution:

```json
{
  "action": "turn_event",
  "event": "ui_surface",
  "session_id": "…",
  "agent_id": "beacon",
  "source_hotel": "mac-jane",
  "surface_id": "s01JQ…",
  "seq": 3,
  "emitted_at": "2026-09-14T15:40:00Z",
  "a2ui": {
    "version": "v0.9",
    "updateComponents": {
      "surfaceId": "s01JQ…",
      "components": [
        { "id": "root", "component": "Column", "children": ["title", "guests"] },
        { "id": "title", "component": "Text", "text": "mac-jane — 4 guests live", "variant": "h2" },
        { "id": "guests", "component": "List", "children": { "componentId": "guest-row", "path": "/guests" } },
        { "id": "guest-row", "component": "Card", "child": "guest-body" },
        { "id": "guest-body", "component": "Row", "children": ["guest-name", "guest-restart"] },
        { "id": "guest-name", "component": "Text", "text": { "path": "name" } },
        { "id": "guest-restart-label", "component": "Text", "text": "Restart" },
        { "id": "guest-restart", "component": "Button", "child": "guest-restart-label",
          "action": { "event": { "name": "restart_guest",
                                 "context": { "action_id": "a3f9c01b2d4e5", "guest_id": { "path": "id" } } } } }
      ]
    }
  }
}
```

**Corrected 2026-10-02 against the vendored v0.9 schemas** (`crates/ansible-mesh-core/specs/a2ui/v0_9/`). The 09-14 example used pre-spec shapes:
- A Button takes a `child` component (usually a Text) and `action.event.{name, context}`, not a `label` with a flat `action`.
- A List repeats a template through `children: {componentId, path}`, with relative paths inside the template.
- A Card has a single `child`.

The surface id and every `context.action_id` are **minted by the hotel**. Ids the model supplies are overwritten.

`philotic-web` forwards this today through the catch-all as `operator_chat:event`; the first slice gives it a typed name, `operator_chat:ui_surface`, and the desktop service maps that to `aiua:ui-surface` on the event bus. `createSurface`, `updateDataModel`, and `deleteSurface` ride the same envelope.

### Desktop → philote (client-to-server)

An A2UI `action` is submitted as a structured operator turn, not free text:

```json
POST /api/mesh/targets/:node/agents/:agent/chat
{
  "conversation_id": "…",
  "ui_action": {
    "version": "v0.9",
    "action": {
      "name": "restart_guest",
      "surfaceId": "hotel-status-2026-09-14",
      "sourceComponentId": "guest-restart",
      "timestamp": "2026-09-14T15:41:02Z",
      "context": { "action_id": "act_01J…", "guest_id": "philote-beacon" }
    },
    "data_model": { "…": "mirrored only when the surface was created with sendDataModel" }
  }
}
```

`SendOperatorChatTurn` carries `ui_action` alongside `content`; `aiua` submits it into the canonical session path via routed `EmitTask`; the philote sees a `ui.action` observation in its dialogue window and decides what to do under its normal tool, approval, and say-do gates. A click never executes anything by itself.

### Catalog `philotic.desktop.v1`

**Source of truth (2026-10-02):** `crates/ansible-mesh-core/specs/a2ui/philotic_desktop_v1.json`. Both the hotel validator (`ansible-mesh-core::surface`) and the web renderer read it.
- It is the basic-catalog subset plus `Table`, with no catalog functions in v1. `{"call": …}` and `functionCall` actions are refused, which also covers `openUrl`.
- Themes accept only `primaryColor` and `agentDisplayName`.
- Limits: 200 components, depth 16, 64 KiB per message, 256 KiB data model, 4 KiB per string.

The 09-14 element mapping below predates the decision that the renderer lives in philotic-stack:

| A2UI component | Desktop element | Notes |
| --- | --- | --- |
| `Text` | native, tokenized | `variant` h1/h2/body/caption |
| `Column`, `Row`, `Divider` | native flex | |
| `Card` | new `ui-card` | header/body/footer slots |
| `List` | new `ui-list` | `children: {componentId, path}` template; relative-path binding per item |
| `Button` | `ui-button` | action requires `context.action_id` |
| `TextField` | `ui-input` | two-way bound to a data-model path |
| `CheckBox` | `ui-checkbox` / `ui-toggle` | |
| `ChoicePicker` | `ui-select` | |
| `Slider` | `ui-slider` | |
| `Modal` | `ui-dialog` | |
| `Table` (Philotic extension) | new `ui-table` | Not in the A2UI standard set; declared in the catalog as a custom component |

Explicitly **excluded** in v1: `Image`, `Video`, `AudioPlayer` (remote URL fetch from a philote-authored surface needs an egress posture decision), `openUrl` (same), `Tabs`, `DateTimeInput`, `Icon`. Excluded components fail validation; they do not render as blanks.

## Governance and Safety Rules

- **Grant by SkillDAG.** A `desktop.surfaces` abstract skill implies `ui.surface.create`, `ui.surface.update`, `ui.data.update`, `ui.surface.delete`. On-demand for `orchestrator`, `admin`, and the operator-facing personas; other roles via `skill.assign`. Same risk tier as a message send, not a mutation.
- **Validation on both sides.** Hotel rejects with `INVALID_SURFACE` before emission; desktop rejects with a visible error card. Neither side "does its best" with a malformed tree.
- **Attribution is mandatory.** Every rendered surface shows owner philote, source hotel, and freshness, per the desktop membrane rule that local dashboards are never canonical merely because they aggregate.
- **Actions are observations, not commands.** A surface action becomes a philote observation. Any side effect goes through the philote's tools, its approval posture, and the outcome reflex. The desktop never calls hotel mutation routes on a philote's behalf because a button said so.
- **Operator session gate applies.** The Surfaces app is a normal workspace app: locked until the hotel reports an authenticated operator session.
- **Size and rate bounds.** Per-surface component and data-model byte ceilings, and a per-session surface-update rate limit, enforced at the hotel. A runaway philote cannot flood the desktop.
- **No script, no HTML.** `Text` is rendered as text nodes. Markdown, if ever allowed, is a separate catalog decision with its own sanitizer.

## Disposition

`accepted` (operator, 2026-10-01), as amended by the 2026-10-01 section above.

Recorded `proposed` on 2026-09-14 as the outcome of the operator's investigation request.

## Seams

Order and scope per the 2026-10-01 amendment. New seams first; the 09-14 rows below are kept and re-numbered by the amendment's order.

| Seam | Scope | Rung to claim done |
| --- | --- | --- |
| `approval-action-ids` (A0) | Every approval card carries a compact approval id end to end (Telegram `callback_data`, edge `ApprovalRequest`/`ApprovalResolve`, desktop). A tap on a card that is no longer pending is refused with a visible "no longer pending" reply, and a stale Trust tap never grants session pre-approval. Bare legacy callbacks and typed `/approve` keep working | watched-live-green: a stale Telegram tap is refused, a fresh one resolves |
| `surface-web-renderer` (S2) | Philotic-owned A2UI→DOM renderer bundle in philotic-stack, served by `philotic-web` at `/s/:surface_id`; catalog map, JSON Pointer binding, validation, card/list/table primitives; host action bridge interface. Supersedes `surface-renderer-catalog` | test-green |
| `surface-https-edge` (S2) | HTTPS for `philotic-web` via Tailscale Serve; `web_public_base_url` config so the transport can later move to the native Philotic VPN | smoke-green |
| `surface-telegram-miniapp` (S3) | Emitting philote's bot sends a `web_app` Open button; `initData` Ed25519 third-party validation, `auth_date` freshness, operator user-id allowlist, operator-bot check; surface-scoped short session | watched-live-green: operator opens and acts on a surface from Telegram |
| `surface-apple-webview` (S3) | `WKWebView` host in PhiloticApp loading the server renderer; `WKScriptMessageHandler` bridge onto the existing `EdgeMessage` connection; no web credential in the page | smoke-green on the operator's iPhone |
| `surface-mcp-app` (S4) | `membrane-mcp` serves the fixed renderer as a `ui://` MCP App for hosts such as Claude and ChatGPT | smoke-green from one MCP Apps host |
| `surface-schema-and-types` (S0) | Vendor the A2UI v0.9 JSON Schema subset for `philotic.desktop.v1`; generate Rust types (`typify`) into `ansible-mesh-core::surface`; `validate()` with catalog allowlist and size ceilings; fixtures for every catalog component and every excluded one | test-green |
| `surface-render-tools` (S1a) | `ui.surface.*` tools in `philote/catalog.rs` + `tool_exec.rs`; `desktop.surfaces` skill seeded and SkillDAG-implied; emission as `turn_event{event:"ui_surface"}` with attribution | test-green |
| `surface-stream-projection` (S1b) | `philotic-web` types the frame as `operator_chat:ui_surface`; desktop `aiua-service` maps it to `aiua:ui-surface`; new Surfaces workspace app opens one window per `surface_id` via `windowManager` | smoke-green: a philote renders a hotel-status card in a desktop window on mac-jane |
| `surface-renderer-catalog` (superseded 2026-10-01 by `surface-web-renderer`) | `a2ui-surface` custom element: adjacency-list tree, JSON Pointer + relative binding, `ajv` validation, catalog map; new `ui-card`, `ui-list`, `ui-table` Shadow-DOM primitives; web-test-runner coverage at the repo's 80% gate | test-green in `jaredlikes-desktop` |
| `surface-action-return` (S2) | `ui_action` on the chat adapter and on `SendOperatorChatTurn`; `ui.action` observation in the philote dialogue; `action_id` correlation; approval card rendered as an A2UI surface resolving the same approval record Telegram's numbered card resolves | watched-live-green: operator approves a real pending tool call from a desktop surface |
| `surface-persistence-rehydrate` (S1, moved up 2026-10-01) | Hotel-owned `ui_surfaces` records (`ListSurfaces`, `GetSurface` IPC; `GET /api/surfaces/:id`); `OutboundReply::Surface` / `EdgeMessage::Surface` carry a surface reference plus a native-lowering hint; delivery outcome `displayed`/`offered_link`/`rejected` reported back to the philote; desktop rehydrates open surfaces after reload; surfaces emitted outside a desktop-initiated turn (cron, Telegram-initiated) reach the desktop via the edge cursor ledger seam already flagged in `serve/edge.rs` | smoke-green |
| `surface-agui-adapter` (investigation, 2026-10-01) | **AG-UI as a membrane**: decide guest shape (`membrane-agui` alongside `membrane-mcp`), auth, mapping AG-UI threads and runs onto philote sessions, A2UI carriage (`CUSTOM`/activity events), and `ag-ui` crate dependency vs hand-mapped events. Originally a deferred SSE adapter | investigation report + decision recorded |

Order: see the 2026-10-01 amendment (A0 → S0 → S1 → S2 → S3 → S4; the AG-UI membrane investigation runs independently).

## Reality Gaps and Traps

- The desktop's committed Aiua auth model is stale relative to its working tree. Commit the cookie-session work in `jaredlikes-desktop` before S1b/S1c land, or the Surfaces app will be built against a model that is about to disappear.
- `windowManager.createWindow` accepts `content` only as an HTML string; app bootstrap state must travel as attributes on the tag (`<a2ui-surface surface-id="…">`), not as objects.
- The catch-all forwarding in `stream_operator_chat_turn` only runs while a desktop-initiated routed turn is in flight. Until S3, a surface emitted from a cron-fired or Telegram-initiated turn has no listener. S1 must state this limit in the tool description so philotes do not claim "I put it on your desktop" when nobody was subscribed (say-do gate).
- The Aiua panels' `:host` styling pattern is broken (no shadow root). The renderer must follow the `ui-*` Shadow-DOM pattern, not the panel pattern.
- A2UI v0.9's `action` has no action id; the Philotic `context.action_id` rule is load-bearing, not optional. When v1.0 ships action ids natively, migrate the field, keep the semantics.
- `@a2ui/web_core` is at 0.11 and moving; vendoring the v0.9 JSON Schema files pins the contract on our side.

## Open Questions

1. Should `desktop.surfaces` be an on-demand skill for every persona, or should only the operator-facing personas (Beacon, Jane) have it by default?
2. Do surfaces belong in the LifeGraph as first-class nodes (a philote "publishes a view") or stay hotel-local runtime records with a catalog projection? S3 must decide; the workspace proposal leans toward catalog records.
3. When the same surface is updated from two hotels (relocation ceremony mid-render), which hotel's `seq` wins? Proposed: the session's current home hotel, last-writer-wins, matching the memory consistency rule.
