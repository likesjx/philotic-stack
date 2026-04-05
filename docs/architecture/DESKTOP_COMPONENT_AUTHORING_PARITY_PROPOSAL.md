---
title: Desktop Component Authoring Parity Proposal
doc_type: proposal
domain: operator-control-plane
status: implemented
last_updated: 2026-04-02
tags:
- desktop
- components
- philotic-web
- manifest
- operator-surface
related_docs:
- DESKTOP_MEMBRANE_PROPOSAL.md
- OPERATOR_CONTROL_PLANE.md
- PHILOTIC_WEB_PROPOSAL.md
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md
proposal_id: desktop-component-authoring-parity
implements:
- desktop-membrane-boundary
implemented_by: []
active_seams:
- desktop-component-authoring-parity
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Desktop Component Authoring Parity Proposal

## Goal

Make the desktop/operator surface able to create and update component registrations using the same manifest contract as `phil component add`, instead of forcing operators to switch between a real CLI shape and a thinner web-only near miss.

## Core Recommendation

Treat `ComponentManifest` as the canonical authoring contract across both CLI and desktop surfaces.

That means:

1. `philotic-web serve` should expose create and update routes for components.
2. Those routes should write through the existing `RegisterComponent` IPC path rather than inventing a second storage mutation flow.
3. Hotel-owned component inventory/detail reads must expose the manifest-relevant fields needed for safe update:
   - `guest_id`
   - `role`
   - `hotel`
   - `command`
   - `args`
   - `env`
   - `component_config`
   - `auto_start`
4. The desktop form should mirror that contract directly rather than deriving behavior from incomplete inventory payloads.

## Disposition

`implemented`

Track implementation in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Why This Matters

The current desktop membrane can inspect and toggle components, but it cannot author them.

That creates three problems:

- the CLI owns the real manifest shape
- the desktop lacks a canonical create/update path
- any future HTML form is tempted to patch against incomplete inventory data and silently drop fields like `env` or `args`

The irony is familiar: a management surface that can restart a component but cannot faithfully describe how that component exists.

## Current Slice

Land the first honest parity slice:

- enrich hotel component inventory/detail payloads with manifest-relevant fields
- add `POST /api/components`
- add `PATCH /api/components/:guest_id`
- keep `RegisterComponent` as the one canonical write path

This is intentionally not a full UI/form slice yet. It is the backend and architecture boundary needed so the eventual desktop form can be correct on day one.

## Reality Gap

The earlier desktop component slice treated `/api/components` as an inventory for model/tool-ish guests rather than a true component authoring surface. That was acceptable for enable/disable/restart, but it is not sufficient for update semantics.

If the desktop is allowed to mutate component state, its read model must be able to round-trip the actual manifest.
