---
title: Component Template Schema Proposal
doc_type: proposal
domain: operator-control-plane
status: accepted for current slice
last_updated: 2026-04-02
tags:
  - desktop
  - components
  - schema
  - vault
related_docs:
  - DESKTOP_COMPONENT_AUTHORING_PARITY_PROPOSAL.md
  - KEY_VAULT_PROPOSAL.md
  - MODEL_CONTROLLER_PROPOSAL.md
task_refs:
  - /Users/jaredlikes/code/philotic-stack/docs/task.md
---

# Component Template Schema Proposal

## Goal

Make desktop component authoring representative of real component shape without inventing a second manifest authority, while making secret handling explicit enough that operators are pushed toward vault-backed config instead of plaintext env values.

## Core Recommendation

Add a backend-owned component template/schema surface for known component families.

Each template should define:

- canonical `command` and default `role`
- operator-editable `env` fields
- operator-editable `component_config` fields
- config or secret dependencies that do not belong in the component manifest itself
- field metadata such as `required`, `input_kind`, `help`, and `vault_only`

The desktop should render structured fields from that backend schema, but keep raw manifest JSON available as an explicit advanced escape hatch.

## Secret Handling Rule

If a field is credential-shaped:

- do not encourage plaintext entry in `env` or `component_config`
- mark the field as `vault_only`
- explain that the actual secret belongs in the vault/config surface
- store only a config key or `secret_ref` in the component-facing path

This follows existing runtime truth: refreshable and API-key auth should live behind vault references rather than raw config values.

## Disposition

Accepted for current slice.

## Current Slice

The current implementation slice adds:

- `GET /api/component-templates` in `philotic-web`
- backend-owned templates for the current known component families
- desktop consumption of those templates in the Aiua component window
- explicit vault/config dependency guidance for secret-backed fields

This slice is intentionally transitional:

- templates are still hand-authored on the backend rather than emitted by each component crate
- raw JSON remains available and canonical for advanced/custom cases
- not every possible custom binary is self-describing yet

## Initial Template Scope

The first schema surface covers the known component families currently represented in repo/runtime truth:

- `membrane-telegram`
- `membrane-discord`
- `model-controller-gemini`
- `model-controller-elevenlabs`
- `tool-runner`
- `graph-runner`

## Follow-On Seams

- move template ownership closer to each component crate once a stable registry shape exists
- add direct desktop affordances for “create vault entry” from a `vault_only` field
- distinguish system-managed env vars from operator-authored fields more aggressively
- extend the same schema discipline to agent/role-worker onboarding where appropriate
