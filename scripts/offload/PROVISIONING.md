# Fallback provisioning plan, 2026-11-09

This document proposes setup. Nothing here installs software, changes an account,
changes UID or security settings, retries denied operations, or authorizes tests.

## Current source

Remote reads confirm issuer main remains
e408c470065cdb307cadd518a5703723e2a9e135. Philotic develop is now
732bf080197842f4cc0fbc1d82c02e7494f370db, nine commits beyond the tested local
b4c44b86292ffa07f72f27f4560a77f531f46816 snapshot. The comparison changes Rust
application/security code and one document, not toolchain, lockfile, release
package selection, workflows or the selected Node/Python tests. Local refs
were not fetched or changed. Do not present the older run as a latest-SHA pass.

Verified sources:
- https://github.com/likesjx/philotic-stack/compare/b4c44b86292ffa07f72f27f4560a77f531f46816...732bf080197842f4cc0fbc1d82c02e7494f370db
- https://github.com/likesjx/jaredlikes-com/commit/e408c470065cdb307cadd518a5703723e2a9e135

## Approved preparation: twelve npm packages, currently network-blocked

The smallest useful setup is a MongoDB-driver-only runtime, not all 537 entries
in the website lockfile. Node 24 and Python 3.12 are already available in cloud.
The CLI imports mongodb statically even when its plan mode needs no database.

Read the exact source and print the derivative preparation plan:

```sh
python3 -B scripts/offload/dependency_plan.py \
  --source-root /verified/issuer-source \
  --expect-sha e408c470065cdb307cadd518a5703723e2a9e135
```

The planner verifies the source manifest and original package-lock hash, walks
mandatory dependencies with nested Node resolution, validates supported semver
edges, and retains registry URLs/SHA512 integrities. It refuses unsupported
dependencies, install-script declarations in lock entries, bins, links,
non-registry sources and required peers.
It prints JSON only, including derivative package.json/package-lock.json.
It does not claim npm has accepted that derived lock.

Pinned package set:
- mongodb 6.21.0; @mongodb-js/saslprep 1.4.12; bson 6.10.4
- mongodb-connection-string-url 3.0.2; sparse-bitfield 3.0.3; memory-pager 1.5.0
- @types/whatwg-url 11.0.5; @types/webidl-conversions 7.0.3
- whatwg-url 14.2.0; tr46 5.1.1; webidl-conversions 7.0.0; punycode 2.3.1

Original issuer package-lock SHA256:
a8a35532e98a5a622cf170409bd64fbeb99cff75f9d43b8ea4c86abd3d37d26c.

Approved scope: download and install only these twelve lock-pinned
packages from registry.npmjs.org into a fresh private runtime directory, using
an empty dedicated cache, no inherited npm credentials/config, and lifecycle
scripts disabled. No application test execution is included in this setup.
No global packages, shell-profile changes, server, Mongo service or credential
creation is needed.

Once a supported network route is established, the approved preparation worker may write the derived package and
lock into that new directory and invoke:
`npm ci --ignore-scripts --omit=optional --omit=peer --no-audit --no-fund`.
Use a clean environment with explicit private HOME/cache and empty user/global
npm config. Do not copy a personal .npmrc. Do not relax lock validation or expand
the package set if npm rejects it; report the exact failure.

Before any later authorized test, retain the original/derived lock hashes,
installer logs, exact installed package inventory and package content hashes.
Validate installed inventory against the approved closure. Use a verified source
snapshot as a child of this runtime so ESM can resolve ancestor node_modules.
Never add node_modules or dependency symlinks inside the immutable source snapshot.
NODE_PATH is not a reliable ESM substitute. Dependency identity verification is a
separate required step; the current planner does not certify installed bytes.

The official npm contract describes frozen lock validation and lifecycle-script
suppression: https://docs.npmjs.com/cli/v11/commands/npm-ci/

## Rust is a separately gated preparation tier

Exact declared target: official Rust 1.94.0, x86_64-unknown-linux-gnu, Cargo/rustc/
rust-std; include rustfmt/clippy to satisfy rust-toolchain.toml. Provision explicit
dedicated RUSTUP_HOME/CARGO_HOME or absolute toolchain binaries. The current
runner deliberately resets HOME and does not reuse personal toolchains/caches.

Cloud currently has GCC/G++, make, pkg-config, OpenSSL development files and
glibc 2.41. Missing: Rust/Cargo and Opus development files. Current workflow uses
libopus-dev and pkg-config. Exact audiopus_sys 0.2.2 source confirms that successful
pkg-config discovery returns before the CMake build path. No CMake is required
for that default Linux dynamic-library route. Do not set static or pkg-config-
disabling overrides, and do not request blanket native packages.

Exact ort-sys 2.0.0-rc.12 honors CARGO_NET_OFFLINE=true: when a native library
is unavailable it sets link_error and returns, intentionally permitting cargo
check. The current compile-only profile therefore needs no ONNX binary download.
This finding is specific to the pinned crate, not a general promise that Cargo
offline prevents arbitrary build-script networking. Actual linking/runtime
qualification needs separately approved ONNX Runtime 1.24.2 library acquisition
and an explicit verified library path; prepopulated cache alone is not enough.

Two source/execution blockers remain before compile preparation is complete:
1. philotic-web/build.rs writes ui-dist/index.html inside source and may invoke
   npm when a sibling desktop checkout exists. Keep that sibling absent and
   design an explicit build-copy/generated-output manifest. Do not silently
   exempt source mutation from verification.
2. native-linux-build requires native-core gates first. Their socket failures
   remain unresolved. A future independent typecheck-only gate must be labeled
   partial and must not waive the failed full-profile prerequisite.

No Rust installation, Cargo dependency fetch, OS-package change or compile is
included in the first npm approval. Exact Cargo.lock acquisition requires its
own reviewed source/registry inventory and prepared cache. Do not use a personal
Cargo config or credentials.

Verified pinned build sources:
- https://github.com/pykeio/ort/blob/v2.0.0-rc.12/ort-sys/build/main.rs
- https://github.com/pykeio/ort/blob/v2.0.0-rc.12/ort-sys/build/vars.rs
- https://github.com/Lakelezz/audiopus_sys/blob/v0.2.2/build.rs

Official references:
- https://rust-lang.github.io/rustup/installation/index.html
- https://rust-lang.github.io/rustup/environment-variables.html
- https://doc.rust-lang.org/cargo/commands/cargo-fetch.html

Project evidence:
- rust-toolchain.toml:21–23 and .github/workflows/build-linux.yml:29–48
- crates/membrane-discord/Cargo.toml:26; crates/aiua/Cargo.toml:29,40
- crates/model-router/Cargo.toml:19; crates/onnx-runner/Cargo.toml:16
- crates/philotic-web/build.rs:24–66,105–125

## Execution permissions remain separate

- Six handoff Unix sockets failed EPERM with 104-byte paths. Do not relocate or
  retry those tests without resolving the access restriction.
- Four owner sockets failed EINVAL with 113-byte paths. Future compatible Linux
  layout should leave a canonical TMPDIR at most 75 bytes, preferably shorter.
  This is a static requirement, not permission to rerun denied operations.
- Issuer trusted-parent guards accept UID 0/1001, while cloud is UID 1000. Four
  tests need an explicitly provisioned disposable UID 1001 fixture environment
  or a narrowly reviewed test-only fixture refactor. Do not weaken production
  guards or run all tests as root to make them green.
- Two root-only cases cover mixed-UID chown and real Compose. Keep
  REAL_COMPOSE_BIN absent. Root/container qualification is separate and not
  authorized by ordinary dependency installation.
- No Docker socket retry, ps, blocked receipt-capture job, GitHub budget change,
  production operation, or CI-policy change is part of this plan.

## Supported environment route remains unresolved

The approved isolated npm preparation failed twice with EAI_AGAIN for
registry.npmjs.org. No package bytes were downloaded, no packages were installed,
and no lifecycle scripts ran. The selected cloud network allowlist does not
include that registry. This evidence does not establish a general DNS outage.

Read-only inspection found all twelve exact lock-integrity tarballs in the Pro's
ordinary npm cache. They were only verified, never copied, imported or executed.
They must not be used to circumvent the cloud network restriction.

Current personal-dot documentation exposes computer inspection but no verified
network exception control for this running cloud. Work's network toggle and
enterprise admin controls are not verified personal-dot setup instructions.
The next supported product step is feedback asking which personal control, if
any, permits the registry and required isolated Linux capabilities. No settings,
DNS, proxy, alternate registry, alternate executor or feedback submission has
been performed as part of this implementation.

- https://learn.chatgpt.com/docs/dots/computers-and-apps
- https://learn.chatgpt.com/docs/dots/controls
- https://learn.chatgpt.com/docs/reference/slash-commands

Rust/Cargo and libopus-dev remain separately unapproved provisioning. Permitted
Unix sockets, the issuer's disposable UID1001 fixture, and isolated root/Compose
qualification require explicit execution support. Do not ask again for the
already-approved npm scope merely because infrastructure remains unavailable.
