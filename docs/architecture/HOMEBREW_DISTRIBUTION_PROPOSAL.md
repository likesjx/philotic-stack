# PROPOSAL: Homebrew Distribution

## Goal
Define a low-friction, repo-realistic path to distribute Philotic binaries through Homebrew without pretending the current internal daemon layout is already a polished public CLI surface.

## Core Recommendation
Start with a dedicated third-party Homebrew tap, not `homebrew/core`.

The first distribution slice should package one clearly operator-facing binary with stable tagged releases and bottled artifacts. Do not begin by publishing the current internal guest binaries as-is.

Recommended first shape:

- publish via `jaredlikes/homebrew-tap`
- ship one user-facing formula
- back that formula with GitHub Releases and bottled artifacts
- keep `homebrew/core` out of scope until the CLI surface, naming, and cross-platform release automation are stable

## Why This Path

### 1. Homebrew policy strongly favors taps first
Official Homebrew guidance makes taps the normal path for third-party distribution, while `homebrew/core` has stricter acceptance rules and explicitly disfavors binary formulae.

Sources:

- [How to Create and Maintain a Tap](https://docs.brew.sh/How-to-Create-and-Maintain-a-Tap)
- [Acceptable Formulae](https://docs.brew.sh/Acceptable-Formulae)
- [Bottles](https://docs.brew.sh/Bottles)

### 2. Philotic does not yet present one obvious public binary
Current workspace binaries are:

- `ansible`
- `agent-core`
- `membrane`
- `model-router`
- `tool-runner`

These are meaningful internal runtime components, but they are not yet a clean public install story for Homebrew users.

### 3. The current binary names are collision-prone
The strongest immediate operator entry point is the hotel daemon in `crates/ansible`, but `ansible` is already an established Homebrew formula name. Shipping a Philotic formula that installs a binary named `ansible` would create naming and linking conflicts.

## Repo Reality

Observed current state in this repository:

- no active `.github/workflows/` release automation is present in this checkout
- the workspace currently builds multiple binaries rather than one consolidated product CLI
- the documented quick start still expects manual config creation via `mesh-config.json`
- the workspace baseline is not currently green, so release automation would inherit active compile failures unless those are resolved first

## Proposed Distribution Shape

### Phase 1: Establish a public install target
Choose one install target for Homebrew users. Recommended options, in order:

1. introduce a dedicated public binary such as `philotic` or `philotic-hotel`
2. if the daemon remains the public entry point, rename the installed binary away from `ansible`
3. keep guest binaries out of the initial formula unless there is a concrete operator need for direct execution

Recommendation:

- create a stable public binary name such as `philotic`
- keep internal crate and process names separate from the external package name if needed

### Phase 2: Add release discipline
Create the minimum release contract required for Homebrew:

- semver tags
- release notes
- reproducible release archives or binaries
- SHA256 checksums
- at least one supported install path for macOS

Preferred release assets for the first slice:

- source tarball for the tagged release
- bottled binaries for Apple Silicon macOS, Intel macOS, and Linux if feasible

### Phase 3: Create the tap
Create `jaredlikes/homebrew-tap` using `brew tap-new` and add a single formula for the public Philotic CLI.

Initial install UX target:

```bash
brew install jaredlikes/tap/philotic
```

### Phase 4: Add bottle automation
Once tagged releases exist, automate bottle generation and upload so users are not forced into source builds by default.

### Phase 5: Revisit broader package surface
Only after the first formula is stable should the project decide whether to:

- install additional helper binaries from the same formula
- split binaries into multiple formulae
- pursue `homebrew/core`

## Recommended First Slice
Land the smallest coherent preparation slice before touching Homebrew itself:

- decide the public binary name
- define the initial install contract
- add release automation for tagged artifacts
- verify the workspace can build the intended binary on a clean baseline

## Disposition
- **Status**: `proposed`
- **Current Slice**: research and planning for a tap-first Homebrew distribution path, with naming conflicts and release-pipeline gaps called out explicitly

## Open Questions

- Should the public Homebrew install target be a new `philotic` binary, or a renamed install projection of the existing hotel daemon?
- Is the first Homebrew audience expected to run only a local single-hotel setup, or a fuller multi-process operator stack?
- Should the first formula install only one executable, or also install companion guests for local development?
- Is cross-platform Linuxbrew support required in the first slice, or can macOS lead?

## Current Slice
This slice only establishes the recommended path and the decision points. It does not yet add release automation, a tap repository, or formula files.

## Task Links
- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
