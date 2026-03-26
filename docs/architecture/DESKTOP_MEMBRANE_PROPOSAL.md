---
title: "Desktop Membrane Proposal"
doc_type: proposal
domain: membrane-transport
status: proposed
last_updated: 2026-03-19
tags:
  - desktop
  - membrane
  - philotic-web
  - localhost
  - leases
  - operator-surface
  - mesh-management
related_docs:
  - ARCHITECTURE_STATUS.md
  - MEMBRANE_COMPONENT_PROPOSAL.md
  - OPERATOR_MEMBRANE_PLUGIN_BOUNDARY_PROPOSAL.md
  - MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md
  - PHILOTIC_WEB_PROPOSAL.md
  - CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
  - RUNTIME_AUTHORITY_LEASES_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: desktop-membrane
implements:
  - runtime-authority-leases
  - membrane-component
implemented_by: []
active_seams:
  - desktop-membrane-boundary
  - desktop-membrane-lease
  - desktop-membrane-view-models
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# Desktop Membrane Proposal

## Goal

Define the first honest boundary between the embedded `jaredlikes-desktop` operator UI and the Philotic runtime so the desktop surface can become a real membrane component instead of a mixed bag of browser delivery, control-plane actions, and direct SQLite/config inspection.

This proposal also makes explicit that the desktop membrane is intended to give the operator access to the **entire reachable mesh**, including updates to any reachable `aiua` regardless of where it is hosted.

This proposal covers:

- what the desktop membrane is
- what it is allowed to expose
- how it should reach the full Philotic mesh
- how UI assets should be developed, built, embedded, and released
- how it should authenticate and expire
- how it should hold a runtime lease so it does not outlive operator intent
- how it should preserve target-hotel authority while still allowing mesh-wide operator reach
- which current `philotic-web serve` behaviors are transitional and should be retired

The desktop membrane is therefore not the owner of a special desktop-only API. It is one membrane client over reusable operator surfaces.

## Core Recommendation

Treat the desktop/operator surface as a **membrane implementation** with a bounded **runtime authority lease** and **mesh-aware control-plane reach**, not as a privileged web shim that happens to run on localhost.

Recommended shape:

1. `philotic-web serve` becomes the first `membrane.desktop` implementation for local operator UX
2. the desktop membrane is allowed to inspect and initiate actions across the **entire reachable mesh**, not only the local hotel
3. the desktop membrane exposes only membrane-shaped view models and bounded actions
4. each target `aiua` remains authoritative for its own canonical reads, writes, policy, and audit trail
5. the desktop membrane must acquire and renew a hotel-governed authority lease while it is serving a live operator session
6. mesh-wide actions flow through explicit control-plane routing, remote management transport, and target-scoped grants rather than ambient trust
7. losing the lease must fail closed: privileged reads stop, mutating routes stop, websocket clients are disconnected, and session credentials are invalidated

Those operator surfaces should eventually be reusable by agents and automation too, with caller-aware redaction and posture/grant checks.

The desktop UI should not become a second control plane with nicer CSS, more direct filesystem access, and vague mesh-wide god mode.

## Disposition

`accepted for current slice`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

First slice is smoke-green: `just smoke-desktop-membrane` (lease, REST API auth, core endpoints, clean shutdown).

Subsequent slices shipped:
- **Slice 1 & 2**: agent cognitive drill-down (`/api/agents/:id/roles`, `/api/agents/:id/rules`, `/api/skills`) and hotel config read (`/api/config`, `/api/config/telegram`, `/api/config/gemini`).
- **Slice 3 (components)**: component inventory + lifecycle management (`/api/components`, `/api/components/:guest_id`, enable/disable/restart) via new `ListComponents`, `SetComponentActive`, `RestartComponent` IPC variants.
- **Slice 4 (graphs + secrets + skill mutations)**: graph runner instance inventory (`/api/graphs`, `/api/graphs/:graph_id`), secret ref inventory (`/api/secrets`, vault registry + config ref presence, no values), skill assignment mutations (`POST /DELETE /api/agents/:id/roles/:role_name/skills[/:skill_name]`) with management-role operator bypass.

## Current Slice

This slice has started landing in code, but the boundary is still transitional in a few important places.

Current behavior in `crates/philotic-web/src/serve.rs`:

- binds an HTTP + WebSocket server on `127.0.0.1`
- generates a random bearer token at startup
- acquires and renews a dedicated desktop membrane lease before serving
- binds the embedded desktop to a same-origin `HttpOnly` session cookie instead of JS token injection
- uses same-session cookie auth for websocket attach
- now routes local status, guest summaries, and redacted agent summaries through explicit hotel-owned IPC view models
- now exposes a first typed mesh target inventory view from the hotel-owned registry with source-hotel, target-hotel, reachability, and freshness attribution
- now exposes a first target-status view that is `local-canonical` for the local hotel, attempts a direct target-hotel query for remote targets, and falls back to `remote-heartbeat-observed` when that query path does not complete
- now exposes a first target-guest inventory contract that is local-canonical for the local hotel and attempts a direct target-hotel management query for remote targets, with explicit fallback when that path cannot complete
- denies apartment inspection on the default desktop membrane surface
- routes guest restart/stop actions to the hotel over local IPC

Current embedded desktop behavior in `jaredlikes-desktop`:

- defaults the membrane client to `window.location.origin` for embedded same-origin use
- relies on cookie-backed membrane session probing instead of injected startup credentials
- keeps explicit bearer-token `connect(token, baseUrl)` only as a remote/debug path
- leaves broader desktop/OS components in source, but does not load them in the default membrane entrypoint

This proves the direction, but it is still transitional rather than a finished membrane boundary because the first remote status and guest query paths are narrow and still rely on a daemon-owned management worker plus reply delivery over the existing task transport, guest inventory still falls back to an explicit error state when that remote path is unavailable, any future apartment-style diagnostic surface still needs a shaped hotel-owned design, and bearer compatibility fallbacks still exist.

One more boundary correction became explicit while exploring remote agent inventory and operator chat: continuing to add `desktop_membrane.*` actions and desktop-shaped reply contracts directly inside `aiua` core would undermine the intended plug-and-play membrane model. That correction is now tracked in [OPERATOR_MEMBRANE_PLUGIN_BOUNDARY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OPERATOR_MEMBRANE_PLUGIN_BOUNDARY_PROPOSAL.md).

The next chat-specific boundary is now explicit too: desktop operator chat should not invent a second conversation plane. It should route into the same canonical agent conversation/session path used by Telegram, with the router resolving whether the backing authority is local hotel runtime, remote hotel runtime, or a future graph-runner-backed authority. That seam is tracked in [ROUTED_OPERATOR_CHAT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROUTED_OPERATOR_CHAT_PROPOSAL.md).

The currently preferred extraction target is a reusable target-oriented operator surface family:

- `operator.targets.list`
- `operator.targets.status`
- `operator.targets.guests`
- `operator.targets.agents`

with router-mediated handoff for non-local fulfillment.

Current UI asset pipeline behavior in `crates/philotic-web/build.rs`:

- `philotic-web` looks for a sibling `jaredlikes-desktop` repository or `PHILOTIC_DESKTOP_DIR`
- if found, it runs `npm install` when needed and then `npm run build`
- it copies the resulting `dist/` into `crates/philotic-web/ui-dist/`
- `rust-embed` then bakes those files into the Rust binary
- if the desktop repo is missing, the build falls back to a placeholder `index.html`

Current release pipeline behavior in `.github/workflows/release.yml`:

- builds Rust binaries including `philotic-web`
- packages those binaries as release assets
- does not currently establish an explicit UI asset provenance contract for the embedded desktop bundle
- does not currently prove which desktop source revision or asset manifest was embedded into a released `philotic-web` artifact

Current observed repo direction outside `serve` is broader:

- [PHILOTIC_WEB_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PHILOTIC_WEB_PROPOSAL.md) already treats `philotic-web` as the operator surface for the full mesh, including remote `aiua` management over an explicit management-plane contract
- [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md) already requires hotel-owned validation, elevation, and action grants for dangerous operations

So the real design task is not whether the desktop membrane should stay local-only.

The real design task is how it becomes the operator's full-mesh app without collapsing target-hotel authority.

That now includes one more constraint: router-mediated handoff should be the shared mechanism for non-local operator surface execution, rather than teaching the desktop membrane to grow its own special remote query transport family.

## Why This Is A Membrane Question

The desktop/operator UI is an outside-facing interaction surface relative to the hotel runtime even when it runs on the same machine.

That means it still needs a membrane contract:

- it introduces a separate client/runtime boundary
- it needs auth and lifecycle policy
- it should normalize hotel state into UI-facing views
- it should not quietly inherit direct ownership of runtime truth just because both processes are local

If the desktop surface reads raw tables directly, keeps long-lived ambient credentials, and survives independently of hotel authority, it is not acting like a membrane. It is acting like an unsupervised sidecar admin plane.

## Current Observed Risks

The current local dashboard shape still has three important boundary smells and one next-step mesh gap:

1. bearer compatibility paths still exist for explicit remote/debug attach
2. target-specific remote read models do not yet exist beyond mesh target inventory
3. pending remote action state and target-scoped grants are not yet modeled in the membrane
4. apartment and profile-like data must stay denied or return only as explicitly shaped hotel-owned diagnostics

These are useful scaffolding shortcuts, not a target architecture.

There is also a build/distribution risk:

7. the current UI asset embedding path is repository-adjacent and convenient, but not yet a disciplined supply chain for released operator surfaces

## Boundary Recommendation

### Desktop membrane responsibilities

`membrane.desktop` should own:

- local operator-session ingress for the embedded desktop UI
- bounded operator-session authentication
- operator-facing mesh topology views and multi-node navigation
- shaping hotel/runtime state into UI-facing view models
- selecting a target hotel or mesh-wide scope for reads and actions
- presenting target-hotel status, eligibility, and action-grant ceremony
- local operator-session websocket or event-stream delivery
- local session liveness and lease renewal
- immediate fail-closed teardown on lease loss

### Hotel-owned responsibilities

The hotel should remain authoritative for:

- who may hold the desktop membrane lease
- canonical guest, session, agent, and routing state
- validation and execution of control-plane actions
- session and approval policy
- audit persistence
- revocation of the desktop membrane authority

For remote mesh operations, the **target hotel** remains authoritative for:

- whether the requesting principal/session/grant may perform the requested action there
- what its current canonical state is
- whether the requested action is valid under local policy, lease state, and runtime conditions
- how the action is recorded, audited, and executed on that node

### Explicit non-responsibilities

The desktop membrane should not own:

- direct SQLite truth
- direct `mesh-config.json` truth
- long-lived secret/session-token storage
- remote node canonical truth
- cross-node authority issuance by itself
- durable session authority
- lease issuance authority

If the desktop membrane starts reading internal state directly because "it is all local anyway," locality has become an excuse for authority drift.

If it starts acting as if selecting a remote node means it now owns that node's truth, the membrane has become an empire with a window manager.

## Mesh-Wide Scope

The desktop membrane is intended to give the operator access to the **entire reachable Philotic mesh**.

That means the operator should be able to:

- inspect mesh topology
- inspect any reachable hotel's status
- inspect guests, sessions, agents, and health across nodes
- initiate lifecycle, admin, and topology actions against any reachable `aiua`
- observe action results, denials, and audit-facing status across the mesh

This is not limited to:

- the localhost hotel
- the currently selected profile
- one node at a time in the architecture sense, even if the UI focuses one at a time in the interaction sense

### Important distinction

Mesh-wide reach does **not** mean mesh-wide ambient authority.

The correct model is:

- one desktop membrane session may navigate the whole mesh
- each addressed hotel still decides what that session may do there
- dangerous actions still require target-scoped validation and, when needed, target-scoped grants

So the membrane is the operator's **mesh-wide ingress surface**, not the universal owner of every node it can see.

## UI Asset Source-Of-Truth Split

The desktop membrane needs an explicit source-of-truth split for UI assets.

Recommended roles:

- `jaredlikes-desktop` repository owns:
  - UI source code
  - frontend component architecture
  - frontend tests
  - local dev server workflow
  - asset bundling rules
- `philotic-web` owns:
  - embedding contract
  - runtime bootstrap contract
  - operator-session injection contract
  - release/distribution contract for the embedded desktop membrane
- release automation owns:
  - proving which desktop asset build was embedded
  - attaching provenance to released binaries
  - rejecting placeholder or stale asset bundles for real releases

The important split is:

- frontend repo owns how the UI is built
- Philotic repo owns what runtime contract that UI must satisfy
- release pipeline owns proving that the two were assembled honestly

Without that split, `ui-dist/` becomes an awkward shrine to whichever `dist/` directory happened to be standing nearest the compiler.

## Development Workflow

The desktop membrane should support an intentional developer workflow, not only compile-time embedding.

Recommended development modes:

### 1. Frontend-first development

Purpose:

- build and iterate on `jaredlikes-desktop` UI components quickly
- use Vite dev server/HMR
- mock or stub membrane data where appropriate

Recommended contract:

- frontend can run against:
  - mock membrane responses
  - local `philotic-web serve` in explicit dev mode
  - later, a recorded or simulated mesh fixture

### 2. Integrated membrane development

Purpose:

- validate the real runtime bootstrap, auth/session flow, and membrane view models

Recommended contract:

- `philotic-web serve` should support a dev-facing mode where:
  - the frontend may be served from a local dev origin
  - CORS/origin allowances are explicit and temporary
  - auth/session semantics still remain honest enough to catch boundary mistakes

### 3. Embedded release-shape development

Purpose:

- prove what the shipped experience actually looks like

Recommended contract:

- developers can build the exact embedded shape that release artifacts will ship
- this path should use the same asset manifest and embedding logic as the release path

## Build Contract

The membrane needs a stricter UI build contract than "if a sibling repo exists, copy its `dist/`."

Recommended build contract:

1. the desktop source build produces a deterministic asset bundle
2. that bundle includes a manifest with:
   - desktop source revision
   - asset build timestamp
   - asset content hash
   - frontend version/build id
3. `philotic-web` embedding records which asset manifest was embedded
4. `serve` can report the embedded UI build id at runtime
5. production/release builds must fail or warn loudly if they would embed:
   - placeholder UI
   - stale cached `ui-dist`
   - assets with missing manifest/provenance

Suggested minimum embedded metadata:

- `ui_build_id`
- `ui_source_rev`
- `ui_asset_sha256`
- `ui_built_at`

These values should be available to:

- the operator UI
- `philotic-web status` or equivalent inspect surface
- release notes or build metadata output

## Runtime Bootstrap Contract For UI Assets

The embedded desktop membrane should have a small, explicit runtime bootstrap contract.

At minimum, the UI should be able to learn:

- membrane base URL
- operator-session/bootstrap mode
- UI build id
- selected hotel/profile context
- feature flags relevant to desktop membrane capabilities

This should be provided through one deliberate bootstrap path, not scattered global variables that accreted during early development.

The current token injection path is transitional and should be replaced by a bounded bootstrap/session mechanism, but the general idea of a well-defined runtime bootstrap contract is correct.

## Distribution And Release

If the desktop membrane is the operator's real mesh app, its UI assets are part of the product, not optional decoration.

Recommended release rules:

### Release artifact composition

A released `philotic-web` artifact should have a provable relationship to one specific embedded desktop asset build.

That means release automation should record:

- Rust binary version/tag
- desktop source revision
- desktop asset manifest/build id
- hashes for the embedded asset bundle

### Release gating

Stable release builds should reject:

- placeholder UI assets
- missing UI provenance metadata
- desktop asset bundles built from dirty or unknown source state
- release artifacts whose embedded asset manifest does not match the recorded release metadata

### Distribution posture

For Homebrew, tarballs, and future package channels, the operator should receive:

- one `philotic-web` artifact with embedded assets
- not a second manual "go build the UI yourself" ceremony after install

That is especially important for:

- Homebrew users
- CI or clean-machine installs
- reproducible support/debugging
- stable release provenance

### Developer convenience vs release discipline

It is fine for local development to remain more permissive.

It is not fine for stable release automation to rely on:

- opportunistic sibling checkout discovery
- whatever `node_modules` happened to contain
- stale `ui-dist` leftovers
- placeholder fallback with no explicit release failure

Convenient local assembly and disciplined release assembly can coexist, but they should not be the same mode wearing different optimism.

## Deployability Expectations

The desktop membrane supply chain should support at least three deployment/distribution contexts:

### 1. Local developer machine

- fast frontend iteration
- easy integrated testing against local `aiua`
- optional explicit dev-origin allowances

### 2. CI/release builder

- deterministic asset build
- explicit provenance capture
- no hidden dependence on a manually checked-out sibling repo unless the workflow fetches it intentionally

### 3. Installed operator environment

- no separate asset build step after install
- shipped embedded assets match the runtime/bootstrap contract
- operator can inspect which UI build is embedded

## Control-Plane Routing Model

Mesh-wide desktop access should follow the same control-plane split already described in [PHILOTIC_WEB_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PHILOTIC_WEB_PROPOSAL.md).

Recommended path:

1. desktop UI talks to the local `membrane.desktop` surface
2. `membrane.desktop` talks to the local `philotic-web` / operator control process
3. local control process resolves whether the request is:
   - local-hotel
   - remote single-target
   - mesh-aggregate read
4. local control process routes remote operations over the explicit management-plane transport
5. the target `aiua` validates and executes the request under its own authority
6. results flow back through the same control-plane path into desktop-facing view models/events

This preserves three boundaries:

- browser/UI boundary
- operator control-process boundary
- target-hotel authority boundary

The desktop membrane therefore becomes the operator's window onto the mesh, not a browser that speaks raw remote-hotel protocol directly.

## Target Authority Preservation

The desktop membrane must preserve the rule that **every hotel owns its own truth**.

That means:

- local dashboard views should not be treated as canonical merely because they aggregate remote state
- remote hotel reads should come from target-hotel management/read-model surfaces, not local guesswork
- remote mutations should always be executed by the target hotel, not simulated locally and later reconciled
- cross-node views should clearly distinguish:
  - observed remote truth
  - cached/aggregated operator view
  - pending requested changes

Recommended invariant:

- desktop membrane may aggregate
- desktop membrane may route
- desktop membrane may present
- desktop membrane may request
- target hotel still validates, persists, and executes

That invariant matters because a mesh UI that quietly becomes canonical for node truth is just a split-brain admin plane with tasteful icons.

## Lease Model

The desktop membrane should use the shared runtime authority lease archetype from [RUNTIME_AUTHORITY_LEASES_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RUNTIME_AUTHORITY_LEASES_PROPOSAL.md).

### Lease family

Use an **authority lease** first.

Reason:

- only one membrane surface should own the active operator-serving authority for a given local scope
- stale desktop surfaces must lose the right to act
- failover, revocation, and expiry need fencing semantics

Retention policy may later decide how long an idle local membrane can stay warm, but that is a separate concern from who currently has the right to serve it.

### Recommended lease shape

Suggested envelope values:

- `lease_type`: `desktop_membrane`
- `lease_scope`: `desktop:<profile>:operator-surface`
- `authority_hotel`: current hotel/profile authority
- `owner_guest_id`: concrete membrane desktop instance identity
- `owner_component_type`: `membrane.desktop`
- `metadata`:
  - `port`
  - `origin_mode`
  - `ui_build_id`
  - `operator_session_mode`
  - `mesh_scope`
  - `selected_targets`

### Lease meaning

This lease governs the right to hold the **desktop operator membrane session**.

It does **not** by itself grant:

- root authority over the full mesh
- blanket permission to mutate every target hotel
- direct secret access on remote nodes
- bypass of target-hotel validation

It answers:

- who currently owns the active desktop membrane surface
- which local authority granted that surface
- until when that local operator surface remains live
- which fencing epoch invalidates stale local operator surfaces

It is therefore a **surface lease**, not a universal mesh supergrant.

### Lifecycle states

Recommended desktop membrane lifecycle:

1. `requested`
2. `granted`
3. `active`
4. `idle`
5. `released`
6. `revoked`
7. `expired`
8. `stale`

### Required behavior

- `serve` must acquire the desktop membrane lease before exposing privileged API routes
- the active membrane must renew on a bounded heartbeat while an authenticated UI session is live
- no renewal means no continued authority
- explicit shutdown releases the lease
- hotel revocation or epoch loss forces immediate fail-closed behavior
- connection loss between membrane and hotel should be treated as loss of authority unless revalidated

## Auth Recommendation

The desktop membrane should move toward **bounded local operator sessions**, not reusable ambient bearer tokens.

Recommended direction:

- same-origin local session bootstrap for the embedded UI
- no unauthenticated token injection into arbitrary `index.html` fetches
- no query-string token transport for websockets
- injected/session credentials remain in-memory by default
- explicit session expiry aligned to the membrane lease window
- websocket/event-stream attach should be bound to the same operator session rather than a second leaked credential path
- remote mesh operations should derive from the authenticated operator session and then be narrowed through per-target authorization checks and grants

The desktop membrane's local operator session should become the parent context for remote actions, but not a substitute for remote authorization.

This proposal does not require choosing cookie-versus-header immediately, but it does require ending the current "page load implies credential disclosure" pattern.

## Authority Stack

To avoid authority confusion, the desktop membrane should model three distinct layers:

### 1. Desktop membrane lease

What it means:

- this local operator surface is the currently valid desktop membrane instance
- it may present mesh state and initiate control-plane requests
- it must renew or expire

What it does **not** mean:

- every action on every hotel is already authorized

### 2. Operator session posture

What it means:

- this operator session is authenticated
- this session may or may not be elevated for certain admin classes
- the session can be bound to a principal identity, posture, and expiry

This aligns with [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md).

### 3. Target-scoped action grants

What they mean:

- this exact principal/session may perform this exact dangerous action against this exact target hotel or target resource within a bounded lifetime

These are especially important for:

- secret rotation
- node shutdown
- mesh topology mutation
- guest migration
- vault operations
- other high-trust remote admin flows

Recommended rule:

- desktop lease gets you the surface
- operator session posture gets you eligibility context
- target-scoped grants get you execution rights for dangerous actions

Do not collapse these into one object just because the UI would prefer fewer nouns.

## Exposure Recommendation

The desktop membrane should expose **curated view models**, not raw internal records.

That recommendation applies to both local and remote mesh views.

### Good first views

- service status
- guest list
- guest lifecycle actions
- redacted agent list
- bounded session summaries
- bounded operator notifications and progress events
- mesh node inventory
- target-hotel health and reachability
- cross-node operation status
- grant/elevation status summaries

### Views that should stay deferred or heavily shaped

- raw apartment payloads
- raw agent identity bundles
- raw system prompts
- raw config file projections
- arbitrary table passthrough
- raw remote-management protocol frames

Rule of thumb:

- if a payload mostly mirrors a table row or internal JSON bundle, it is probably too raw for the membrane
- if a UI panel needs it, the hotel should publish a bounded read model deliberately

For mesh-wide panels, add one more rule:

- aggregate views may combine multiple hotel-owned read models, but the desktop membrane should still be able to attribute each field to a source hotel and freshness window

## Control Plane Interaction

Mutating actions should remain hotel-mediated.

Recommended first control actions:

- guest restart
- guest stop
- service status refresh
- bounded agent/session inspection
- mesh node status refresh
- remote guest lifecycle actions on explicitly selected targets

Recommended later high-trust actions:

- secret add/rotate/revoke
- node shutdown/restart
- mesh topology mutation
- guest migration
- trust inventory changes
- vault operations

All of these should flow through hotel-owned validation, authorization, audit, and execution.

The desktop membrane may initiate and render these actions, but it should not become the canonical executor or validator.

For remote actions, "hotel-owned" means **target-hotel-owned**, not merely "some hotel somewhere signed off once."

## Relationship To `philotic-web`

`philotic-web` remains the operator entrypoint and packaging seam.

Recommended split inside that product:

- CLI/TUI/operator process model remains the main control-plane story
- `serve` becomes a local membrane implementation for the embedded desktop
- the desktop membrane becomes a first-class UI for local and remote mesh administration
- remote mesh administration still belongs to explicit control-plane contracts, not direct browser-to-remote-hotel trust

This means the desktop membrane should be thought of as:

- the main rich operator app for the Philotic Web
- backed by the same explicit control-plane contracts the CLI would use
- able to govern any reachable `aiua`
- but never allowed to erase the line between operator ingress and target-hotel authority

This preserves the [PHILOTIC_WEB_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PHILOTIC_WEB_PROPOSAL.md) direction while stopping the local web surface from quietly becoming a privileged API with unclear ownership.

## Remote Management Transport

This proposal intentionally aligns with the remote-management direction already named in [PHILOTIC_WEB_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PHILOTIC_WEB_PROPOSAL.md):

- local desktop membrane should not speak raw remote-hotel protocol directly from the browser
- remote `aiua` access should flow through the explicit management-plane transport
- the management-plane transport should preserve:
  - mutual authentication
  - replay resistance
  - audit attribution
  - target identity clarity
  - grant-bound dangerous actions

Current repo truth does not claim that this full remote desktop-management path is implemented today.

This proposal does claim that the desktop membrane should be designed **for** that path rather than architected as a localhost-only cul-de-sac we later try to stretch across the mesh with duct tape and hope.

## Mesh Read Models

The desktop membrane will need at least three classes of read model:

### Local-surface read models

- local operator-session state
- local membrane lease state
- local connection/serve health

### Target-hotel read models

- service status
- guest inventory
- bounded agent/session views
- lease/reachability/health summaries

### Mesh-aggregate read models

- node inventory
- topology map
- fleet health rollups
- cross-node action progress
- distributed audit/event summaries

Recommended rule:

- aggregate views may compose target-hotel views
- they should not become a shadow source of truth
- when detail matters, the operator must be able to drill back into target-hotel-owned state

## Asset Verification

The desktop membrane should eventually have an explicit verification ladder for UI assets too.

Recommended rungs:

1. frontend unit/component tests in `jaredlikes-desktop`
2. frontend integration tests against mocked membrane contracts
3. integrated membrane smoke against local `philotic-web serve`
4. release-shape smoke proving the embedded assets served by `philotic-web` match the recorded build manifest

The release-shape smoke matters because the shipped operator app is the embedded combination, not the abstract idea that both halves pass separately somewhere else.

## First Implementation Slice

The first honest slice should be:

1. define a `desktop_membrane` lease scope and owner identity
2. define the desktop membrane as mesh-aware ingress rather than localhost-only UI
3. acquire the lease before privileged serve routes become active
4. renew it while the embedded operator session is live
5. release it on clean shutdown
6. fail closed on lost lease or hotel disconnect
7. remove unauthenticated credential injection and websocket query-token auth
8. replace direct SQLite/config reads with the first hotel-shaped read models for:
   - local status
   - local guests
   - redacted local agents
9. define the first remote-target read routing shape for:
   - target hotel selection
   - remote status
   - remote guest inventory
10. make explicit that dangerous remote actions require target-scoped authorization/grants rather than only possession of the desktop membrane session

That is the smallest slice that proves this surface is becoming a membrane with real mesh ambitions instead of a localhost dashboard wearing a big future tense.

## Follow-On Slices

After the first slice, likely next slices are:

1. target-scoped remote read models over the management-plane transport
2. desktop-aware operator posture and elevation UX
3. explicit target-scoped action-grant ceremonies for high-trust remote operations
4. mesh-wide inventory and topology panels
5. cross-node action progress/audit views
6. guest migration and mesh topology mutation flows
7. explicit UI asset manifest/provenance embedding
8. release gating that rejects placeholder or unverifiable embedded assets
9. integrated frontend dev mode with honest membrane bootstrap semantics

## Open Questions

1. Should the desktop membrane be materialized by the hotel as a first-class guest identity, or remain a `philotic-web`-owned local process that still acquires hotel authority?
2. Should the operator session be cookie-backed, memory-backed, or represented as an explicit bounded local grant record?
3. Should lease renewal be tied to websocket liveness alone, or to a richer operator activity window?
4. Which first read models belong in hotel IPC directly versus a thin local control-plane adapter inside `philotic-web`?
5. When the desktop UI is opened in multiple tabs or windows, should they share one membrane lease or require a single canonical owner with secondary client attachment semantics?
6. Should mesh-aggregate reads be served by the local operator process, by a designated home hotel, or by explicit fan-out to selected targets?
7. How should the UI represent partial mesh reachability, stale remote data, or conflicting target-hotel freshness windows?
8. Which dangerous remote operations require explicit one-time grants versus bounded elevated session posture alone?
9. How should multi-target operations be modeled so one granted action on node A does not quietly imply the same right on nodes B through Z?
10. Should `jaredlikes-desktop` remain a sibling repo long-term, or should the frontend source eventually move closer to the Philotic release pipeline if the embedded desktop becomes the primary operator surface?
11. What is the canonical asset manifest format, and where should it be surfaced at runtime?
12. Should release automation fetch and build the desktop repo explicitly, or consume a separately versioned frontend artifact?

## Reality Gap

Current repo truth has a useful embedded dashboard, but not yet a secure desktop membrane.

The architecture gap is not that we lack a browser UI.

The architecture gap is that we have not yet made the browser-facing local surface answer the same Philotic questions that every other bounded-authority runtime surface is expected to answer:

- who owns this surface right now
- under which authority
- for how long
- with which fencing epoch
- and what happens when that right expires

And for full-mesh operator access, there is a second reality gap:

- how a desktop membrane can legitimately reach every `aiua`
- without becoming the canonical owner of every `aiua`
- without turning local operator-session possession into ambient mesh root
- and without collapsing local surface lease, operator posture, and target-scoped execution rights into one blurry credential

There is also a distribution reality gap:

- the current embedded UI assembly path is sufficient for local iteration
- but it is not yet a fully explicit build, release, and provenance contract for the operator app we actually intend to ship
