---
title: Desktop Workspace Components Proposal
doc_type: proposal
domain: operator-control-plane
status: accepted-current-slice
last_updated: 2026-05-12
tags:
- desktop
- workspace
- system-settings
- window-manager
- event-bus
- philote-apps
- customization
related_docs:
- DESKTOP_MEMBRANE_PROPOSAL.md
- HOTEL_USER_IDENTITY_AND_OPERATOR_AUTH_PROPOSAL.md
- CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md
proposal_id: desktop-workspace-components
implements: []
implemented_by: []
active_seams:
- desktop-component-map
- philote-app-publication
- desktop-system-boundary
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Desktop Workspace Components Proposal

## Goal

Document the actual desktop substrate that Philotic is embedding and clarify which parts belong to:

- system-level environment governance
- workspace apps and windows
- event routing and cross-component coordination
- widget/home-desktop surfaces
- future philote-authored or philote-published apps

This exists so the desktop stops feeling like a collection of pleasant accidents and becomes an explicit operator workspace that can evolve without rediscovering its own skeleton every week.

## Core Recommendation

Treat the desktop as a **workspace operating layer** with four explicit tiers:

1. **System tier**
   - global environment/state governance
   - settings, auth/bootstrap posture, credentials posture, OS integration toggles
2. **Workspace app tier**
   - task-facing hotel/agent/data/app surfaces opened as windows
3. **Coordination tier**
   - event bus, application registry, application manager, window manager, desktop manager
4. **Customization/publication tier**
   - philote-published apps, widgets, and contextual tools governed by catalog/policy rather than ad hoc static wiring

The desktop should not confuse:

- a setting with a work surface
- a membrane authority boundary with a widget
- a philote customization artifact with ambient ungoverned code execution

## Disposition

`accepted for current slice`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Current Slice

Observed substrate in `jaredlikes-desktop` today:

### Coordination tier

These are the real desktop coordination primitives already present:

- `applicationRegistry`
  - desktop-visible app registration and launch ownership
- `applicationManager`
  - application lifecycle metadata and capabilities
- `windowManager`
  - window creation, focus, close, restore, and per-app visibility
- `desktopManager`
  - multiple desktop/space management
- `eventBus`
  - cross-app and cross-service coordination
- `widgetManager`
  - desktop widget registration, layout, and persistence

These are not optional flourishes. They are the real OS-shaped integration boundary.

### System tier

Current `System Settings` already hosts:

- `Desktop`
- `Date & Time`
- `Notifications`
- `Appearance`
- `Weather`
- `PWA`
- `AI Agents`
- `Aiua Membrane`
- `Credentials`

Rule:

- if the surface governs environment, auth, credentials, or desktop posture, it belongs in `System Settings`
- if the surface is where the operator does hotel/agent/graph/catalog work, it belongs in an app/window

### Workspace app tier

Current workspace-level apps include:

- `Aiua`
- `Aiua Mesh`
- `Aiua Agents`
- `Aiua Components`
- `Aiua Config`
- `Aiua Catalog`
- `Finder`
- `Notes`

These are windowed work surfaces, not environment-governance panels.

### Widget tier

Current widget substrate exists and is already wired for:

- `Clock`
- `System Info`

This proves the desktop can support persistent ambient context surfaces separate from app windows.

## System Settings vs Apps

### Belongs in System Settings

- membrane auth/bootstrap
- operator session posture visibility
- credentials and secure references
- desktop appearance/behavior
- OS integration toggles
- long-running always-on desktop ingress posture

### Belongs in Apps

- hotel overview
- mesh topology
- guests
- agents
- components
- config workflows
- catalog browsing
- graphs/data exploration
- philote-specific contextual work surfaces

Short version:

- **Settings govern the environment**
- **Apps do the work**

## Event Bus Role

The event bus should be the canonical coordination layer between:

- auth/session state
- app lock/unlock behavior
- window focus/open behavior
- mesh refresh signals
- component/config/catalog updates
- philote-published app installation or availability

Examples that should be first-class:

- `aiua:auth-required`
- `aiua:auth-succeeded`
- `aiua:logout`
- `aiua:connected`
- `aiua:disconnected`
- `desktop:workspace-app-installed`
- `desktop:workspace-app-updated`

The browser DOM should not become the integration bus by accident.

## Philote Customization And Publication

Philotes should eventually be able to customize the desktop by publishing:

- workspace apps
- mini-apps
- widgets
- contextual data views

But the publication model should be governed.

Recommended split:

### Canonical graph/catalog truth

The mesh/global catalog should define:

- app id
- label
- icon metadata
- owner philote/role
- required tools/skills
- required data bindings
- placement/runtime constraints
- permissions posture
- artifact reference

### Artifact source

A separate artifact origin may hold the actual bundle/source:

- GitHub
- release artifact store
- internal content-addressed artifact registry

The graph/catalog should say **what the app is**.
The artifact source should say **where the app bundle comes from**.
The hotel should say **whether it may run here**.

That keeps philote customization from becoming “upload JavaScript and pray.”

## Persistence Model

We should distinguish:

### Persistent desktop records

- app installation/publication metadata
- widget layout
- desktop placement/layout
- app preferences
- operator-facing pinned/favorited surfaces

### Runtime-only state

- open windows
- temporary selection state
- transient auth prompts
- current live hotel refresh/loading state

Philote-created apps should persist as catalog/application records, not only as current-window accidents.

## Implication For Aiua Auth

The recent hotel-auth bootstrap slice in `philotic-web` makes the server the authority, which is right.

The desktop implication is:

- `System Settings > Aiua Membrane` should own the operator bootstrap/login UI
- Aiua workspace apps should render a locked state when the hotel says no operator session exists
- successful auth should unlock/focus Aiua apps through the event bus and application/window managers

That is the correct system-level integration path.

## Next Seams

1. Make `System Settings > Aiua Membrane` the primary auth/bootstrap surface in the desktop UI.
2. Replace the remaining token-era disconnected panels with locked-state workspace UX.
3. Define the first graph-canonical desktop app schema for philote-published apps.
4. Define how app artifacts are sourced, verified, and made available to hotels across the mesh.
