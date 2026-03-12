# Local Admin Fallback Model Proposal

## Goal

Define a local, emergency-capable management model path so Philotic can still perform critical admin and control-plane tasks when external model providers are unavailable.

## Disposition

`proposed`

Track related work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Core Recommendation

Philotic should support a **local admin fallback model** for bounded management and recovery tasks.

Recommended first direction:

- Gemma / FunctionGemma style local model
- ONNX-backed runtime where possible
- scoped to admin and control-plane support, not as a pretend universal replacement for frontier models

## Intended Uses

- emergency hotel administration
- bounded policy inspection or mutation
- tool-call and structured management workflows
- local embeddings
- degraded-mode operation when external APIs fail

## Why This Matters

If all management capability depends on external models, then the system becomes least governable exactly when it is already having a bad day.

That is almost performance art.

## ONNX Implication

ONNX support should be treated as a first-class plugin path for:

- embeddings
- local admin inference
- structured tool-calling where the model can support it

Tools should remain tied to model capacity; the local fallback should only be granted what it can honestly perform.

## First Slice Recommendation

Define a narrow capability envelope for local admin support:

- inspect state
- summarize state
- emit structured control suggestions
- perform bounded tool-call style management tasks

Then wire one local model path behind that envelope before broadening scope.
