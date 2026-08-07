---
title: Philotic Web Hardening — The Management Plane Nobody Declared
doc_type: proposal
domain: operator-control-plane
status: active
disposition: proposed
last_updated: 2026-08-06
verification_level: none
tags:
- philotic-web
- management-plane
- operator-auth
- perimeter
- edge-client
- hardening
related_docs:
- PHILOTIC_WEB_PROPOSAL.md
- CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
- HOTEL_USER_IDENTITY_AND_OPERATOR_AUTH_PROPOSAL.md
- OPERATOR_IDENTITY_AND_DANGEROUS_ACTION_CEREMONIES_PROPOSAL.md
- HOTEL_PERIMETER_TRUST_PROPOSAL.md
- PERIMETER_EGRESS_CONTROL_PROPOSAL.md
- NATIVE_APPLE_APP_PROPOSAL.md
- SUBSTRATE_HARDENING_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: philotic-web-hardening
active_seams:
- management-plane-security
---

# Philotic Web Hardening — The Management Plane Nobody Declared

> `PHILOTIC_WEB_PROPOSAL.md` (2026-03-31) says the web server is "strictly a
> local process serving a read-mostly dashboard … **not** a public-facing
> service and **not** a management API. Remote node management always goes
> through the CLI's mTLS management port, never through the web server."
>
> The mTLS management port was never built. Everything went through the web
> server anyway.

## Goal

Bring the security model of `philotic-web` into line with what it actually
became: the mesh management plane. This proposal does not invent a new
architecture — the intended one is already written across six accepted
proposals. It closes the distance between that intent and the shipped code,
and it names the one seam the graph has declared since March and never
populated: `seam:management-plane-security`.

## Evidence Discipline

Per [AGENTS.md](../../AGENTS.md) §2.4, every claim below is labelled:

- **PROVEN** — code read directly during this analysis; file:line cited.
- **INFERRED** — reported by a review pass, not independently re-verified.
- **INTENDED** — a proposal says so; no implementation claim attached.

## 1. The Frame: Scope Drift Without a Trust-Model Update

**PROVEN.** `crates/philotic-web/src/serve.rs` routes a full remote-hotel
administration surface. From the module header (serve.rs:23–60) and the route
table (serve.rs:~840–895), a single listener carries:

- `POST /api/mesh/targets/:node/vault` — add vault entries on another hotel
- `POST /api/mesh/targets/:node/secrets/rotate` — rotate another hotel's secrets
- `PUT /api/mesh/targets/:node/config/:key` — write another hotel's config
- component create / patch / delete / enable / disable / restart, locally and
  cross-mesh
- `PUT .../agents/:agent/roles/:role/home` — move a role between hotels
- `POST .../agents/:agent/chat` — drive another hotel's agent
- cron create/delete/enable/disable, skill assign/revoke, guest restart/stop

That is not a read-mostly dashboard. It is the control plane, and the document
that defines its trust model still describes a different program.

**PROVEN — there is no TLS.** `crates/philotic-web/Cargo.toml` pulls `rustls`
only through `reqwest` (Cargo.toml:32) for *outbound* calls. There is no
`axum-server`, no server-side rustls config; `serve.rs:904` binds a plain
`tokio::net::TcpListener` and hands it to `axum::serve` (serve.rs:948). Every
URL the process prints is `http://` (serve.rs:928).

**PROVEN — the session cookie is consistent with that, and that is the
problem.** serve.rs:2260 sets:

```
{AUTH_COOKIE_NAME}={...}; Path=/; HttpOnly; SameSite=Strict; Max-Age={...}
```

`HttpOnly` and `SameSite=Strict` are correct and deliberate. `Secure` is
absent — necessarily, since there is no TLS to be secure over. On any
non-loopback bind the operator session cookie and every admin request body
cross the wire in cleartext. On a Tailscale bind WireGuard carries the
confidentiality; on a LAN or `0.0.0.0` bind nothing does.

Someone already saw this: `jareds-macbook-air.tail28cc54.ts.net.crt` and `.key`
sit in the repo root, gitignored. Tailscale-issued TLS material was obtained
and never wired in.

## 2. Perimeter Classification Silently Downgrades the Public Case

**PROVEN.** serve.rs:607 calls:

```rust
let bind_classification = classify_bind_addr(bind_host, false);
```

The second argument is `has_public_ip`, hardcoded `false` with the comment "we
do not probe for a public IP here." In
`crates/perimeter-core/src/classifier.rs:34–44`, a `0.0.0.0` bind with
`has_public_ip == false` returns `Lan`, not `Internet`.

`ExposureTier` is ordered `Local=0, Lan=1, Mesh=2, Internet=3`
(`crates/ansible-mesh-core/src/lib.rs:106–117`). Two deliberate Internet-tier
protections in `edge_fence_allows` (serve.rs:2152–2184) are keyed on that
ordering and therefore do not fire:

```rust
if path == "/api/edge/enroll" {
    return state.exposure_tier <= ExposureTier::Mesh;   // Lan(1) <= Mesh(2) → open
}
...
if peer.is_loopback() && state.exposure_tier <= ExposureTier::Mesh {
    return true;                                        // loopback bypass stays on
}
```

Failure scenario: on vps-jane (public IP `31.97.130.98`), setting
`PHILOTIC_WEB_BIND=0.0.0.0` classifies as `Lan`. Device enrollment — which the
code's own comment says "a static invite is not strong enough for a
public-facing bind" — stays open to the internet behind a static invite code
plus throttling, and the loopback auth bypass the Internet tier is designed to
remove remains active for any local process on that host.

**This is not currently live.** No deploy config sets `web_bind` or
`PHILOTIC_WEB_BIND` anywhere in `ansible/`, `scripts/`, or the tracked configs
(verified 2026-08-06); the default is `127.0.0.1` (serve.rs:DEFAULT_WEB_BIND).
The defect is latent and fires on the first non-loopback deployment — which is
precisely what the tailnet certs suggest is coming.

**Severity: medium.** It is a defense-in-depth downgrade, not an auth bypass:
`edge_fence_allows` is deliberately stricter than
`perimeter_core::fence::check_ingress` and still requires a valid operator
session for every `/api/` route and `/ws` at any non-`Local` tier
(serve.rs:2138–2184). The credit belongs where it is due — that stricter gate
is the right design and it is correctly documented in place.

**Fix:** probe for a public IP, or thread the hotel's known public address into
the call, or refuse to bind `0.0.0.0` without an explicit
`--i-know-this-is-public` acknowledgement.

## 3. The Declared Seam Was Never Populated

**PROVEN.** `doc:philotic-web` declares five seams. All five are empty shells
in the intel graph — one incoming `applies_to` edge from the proposal, zero
outgoing edges, no `file_path`, no verification record:

| Seam | Outgoing edges | file_path |
|---|---|---|
| `seam:management-plane-security` | 0 | null |
| `seam:philotic-web-crate` | 0 | null |
| `seam:distribution-pipeline` | 0 | null |
| `seam:repo-identity` | 0 | null |
| `seam:binary-rename-ansible-to-aiua` | 0 | null |

The proposal itself carries `verification: none`. The graph has named the
exact seam this analysis is about since March and has never had a single fact
attached to it. Populating `seam:management-plane-security` — linking it to
`serve.rs`, `serve/edge.rs`, `perimeter-core`, and the tests that cover them —
is a prerequisite for any honest verification claim here, not a bookkeeping
chore.

## 4. Documentation Truth Is Actively Misleading

**INFERRED** (documentation review pass, citations spot-checked):

1. **`PHILOTIC_WEB_PROPOSAL.md` is the most dangerous stale doc in the repo.**
   Its entire Security Model section — threat model, mTLS management port, PKI
   mesh CA, action grants, `ManagementEvent` audit ledger, UDS
   challenge/response, envelope-encrypted `secret set` — describes a system
   that does not exist. Meanwhile the system that *does* exist (cookie
   sessions, OIDC, typed confirmations, edge bearers) appears nowhere in it. A
   reader doing threat modelling from this document would model the wrong
   program. Its frontmatter also self-conflicts: `status: proposed` +
   `disposition: accepted-current-slice` + a body disposition of `proposed`.

2. **Two accepted proposals contradict themselves internally.** Both
   `OPERATOR_AUTH_BOOTSTRAP_STRATEGY_PROPOSAL.md` and
   `HOTEL_USER_IDENTITY_AND_OPERATOR_AUTH_PROPOSAL.md` have a "Current Slice"
   section claiming a capability is implemented and a "Reality Gap" section in
   the same file denying it exists. The Reality Gap sections are stale, but a
   reader cannot tell which half to believe without reading the code.

3. **`MCP_MEMBRANE_HARDENING_PROPOSAL.md` reads `proposed` /
   `implemented_by: []`** while `docs/DEFECTS.md` records slices H1–H4 fixed on
   2026-07-15 (DEF-053 through DEF-056).

4. **"philotic-web never holds secret material"** was false until recently.
   `PERIMETER_EGRESS_CONTROL_PROPOSAL.md` documents that philotic-web resolved
   OIDC provider secrets and built token clients until that path moved behind
   `egress-http-runner` — a genuinely good fix, smoke-green, and the one place
   the intended architecture actually landed. The macOS OIDC decrypt-mismatch
   on that path remains unverified live (DEFECTS.md tech debt).

5. **`docs/DEFECTS.md` stops at DEF-077 / 2026-07-29**, while DEF-078 and
   DEF-079 exist in merged PRs. The ledger understates open work.

6. **The three edge seam diagrams are empty placeholders** —
   `docs/architecture/generated/seam-edge-{sessions-bridge,cursor-ledger,push-notification}.puml`
   each contain a single orange rectangle. The seam content lives only in
   `NATIVE_APPLE_APP_PROPOSAL.md` prose.

7. **`crates/philotic-web/README.md` does not exist**, against the repo's own
   documentation standard.

## 5. Secret Handling and Local Surface

Findings from the CLI/PWA review pass. The four marked PROVEN were
independently re-read at the cited lines during this analysis.

### 5.1 Onboarding writes live secrets world-readable — **PROVEN, high**

`crates/philotic-web/src/onboard.rs:256`:

```rust
std::fs::write(config_path, &pretty)
```

The value being written (onboard.rs:235–241) contains the Muninn root
password:

```rust
"muninn": { "endpoint": "...", "admin_username": "root",
            "admin_password": muninn_password },
```

plus per-agent Telegram bot tokens (onboard.rs:406–413). `fs::write` uses the
umask default — typically `0644` — and no `set_permissions` follows. Contrast
`init.rs:75–77`, which correctly chmods the operator key to `0600`.

Failure scenario: any other local account reads
`~/.philotic/<profile>/config.json` and takes live Telegram bot tokens and the
Muninn root password. `.gitignore` covers `mesh-config.*` but not the profile
path.

**Fix:** `OpenOptions::new().mode(0o600).create_new(true)` — one call.

### 5.2 A placeholder API key ships in every binary — **PROVEN, medium**

`crates/philotic-web/ui-dist/config.js:9`:

```js
window.MONGODB_API_KEY = 'test-api-key-12345';
```

The file's own comment says "In production, this file is generated by Docker
entrypoint with the actual API key." So the *design* places a bearer-equivalent
key in a static JS file served unauthenticated to every client that can reach
the listener. `build.rs` embeds `ui-dist/` wholesale via `rust-embed`, so the
placeholder is baked into every `phil` binary; the service worker caches `.js`
stale-while-revalidate, so a rotated key keeps being transmitted by every
device that ever loaded the page.

Today only the dev placeholder is present — the severity is about the pattern,
not a live leak.

### 5.3 Unvalidated identifiers reach a persisted shell hook — **PROVEN, medium**

`crates/philotic-web/src/harness.rs:3070`:

```rust
let verify_cmd = format!(
    "phil graph harness verify {harness_id} 2>/dev/null | grep -E '...' || true"
);
```

This string is written into `.claude/settings.json` as a `SessionStart` hook —
a command executed at every future Claude Code session start. `harness_id` is
never character-validated. The same raw identifier is joined into home-directory
paths (harness.rs:2680–2688), and windsurf workflow/skill filenames are built
from intel-graph content (harness.rs:3204, 3228) — a surface agents can write
to over MCP.

**Fix:** an `[a-z0-9._-]+` allowlist at every harness/skill/workflow identifier
entry point.

### 5.4 `phil reset` wipes keys and memory with no confirmation — **PROVEN, medium**

`crates/philotic-web/src/reset.rs:77–88` calls `fs::remove_dir_all` on
`~/.philotic` and then unconditionally on `~/.muninn`. No prompt, no `--yes`,
no backup. Without `--keep-identity` this destroys the operator ed25519
keypair, every profile's context DB, and the entire Muninn engram store. Given
that this fleet has already lost operator data to an unguarded destructive verb
(PR #404), a total-destruction command should require an explicit gate.

### 5.5 Remaining CLI/PWA findings — **INFERRED**

| Finding | File:line | Sev |
|---|---|---|
| Vault master-key file mode never checked; `doctor`'s permission check omits the one file whose mode is an immediate key leak | start.rs:174, doctor.rs:1713 | med |
| Malformed `.claude/settings.json` silently replaced wholesale, destroying operator permission config | harness.rs:3065 | med |
| `.claude/CLAUDE.md` clobbered rather than merged; writes follow symlinks | harness.rs:3047, 3246 | med |
| `phil flush` / `footprint --kill` SIGKILL by raw substring of `ps aux` — kills an editor with "membrane" in its argv | flush.rs:84, footprint.rs:134 | med |
| Service worker caches authenticated `/api/` responses into browser Cache Storage, surviving logout | service-worker.js:141, 192 | med |
| Relative `target/release/aiua` path can be baked into a launchd plist (launchd cwd is `/`) | service.rs:75, 99 | med |
| CWD-ancestor-walk execution of `scripts/*.sh` under `phil doctor --fix` and `phil mcp uat` | start.rs:146, mcp.rs:156, doctor.rs:345 | med-low |
| Operator key write-then-chmod TOCTOU; `identity/` dir not `0700` | init.rs:75–77 | med-low |
| Mesh invite trust material written `0644` into CWD | mesh.rs:49 | low |
| No CSP or referrer policy on the control-plane shell; bundle hardcodes `api.jaredlikes.com` | ui-dist/index.html | low |
| 690 embedded `ui-dist/assets` files (52 generations of `index-*.js`), only 2 git-tracked, none the referenced bundle; SW precache list references files that do not exist, so SW install fails on every load | ui-dist/ | low |
| `home_dir().expect(...)` panics on stripped `$HOME` | init.rs:11, service.rs:42, reset.rs:82 | low |

### 5.6 What is done well

This crate is not careless, and a hardening proposal that only lists wounds
would misrepresent it:

- `integration.rs:28–37` deliberately omits a plaintext credential flag "so
  shell history cannot become a surprise vault"; credentials arrive by file or
  stdin only.
- `doctor.rs` opens the DB `SQLITE_OPEN_READ_ONLY`, quarantines rather than
  deletes, keeps an append-only repair journal, pins destructive repairs to
  `NeedsConfirm`, and states that findings "must never carry plaintext or
  ciphertext material."
- `service.rs` keeps the vault master key **out** of the launchd plist, pinned
  by tests.
- `constant_time_eq` (serve.rs:2107) is a correct constant-time comparison,
  used for edge bearer checks.
- `harness.rs` models exclusive vs co-owned files explicitly (`AuxCheck::Hash`
  vs `Contains`) and treats an unresolvable declared skill as a verify error,
  with regression tests pinning the real incident.
- The `edge_fence_allows` gate is deliberately stricter than the shared
  perimeter fence, and says so in a comment that explains why.

## 6. Remediation Sequence

Ordered by risk-reduction per unit of work, not by severity label.

### Slice 1 — Stop leaking secrets to the local filesystem
`0600` on everything `onboard.rs` and `mesh.rs` write; extend the `doctor`
`secrets.store-permissions` check to cover `vault-master-key.env`; switch
`init.rs` to `OpenOptions.mode(0o600).create_new(true)` and `0700` the
identity dir. Small, self-contained, testable.

### Slice 2 — Close the perimeter classification gap
Thread real public-IP knowledge into `classify_bind_addr` at serve.rs:607, or
gate `0.0.0.0` behind an explicit acknowledgement. Add tests asserting that a
public bind classifies `Internet`, that enrollment is refused there, and that
the loopback bypass is off.

### Slice 3 — Decide the TLS question
Either (a) terminate TLS in `philotic-web` using the tailnet certs already
sitting in the repo root and set `Secure` on the cookie, or (b) write down
that non-loopback binds are supported **only** over Tailscale, and enforce it
— refuse to bind a non-CGNAT, non-loopback address without an override. Doing
neither is the current state and is the weakest option.

### Slice 4 — Input allowlist on identifiers
`[a-z0-9._-]+` at every harness/skill/workflow/profile identifier entry point,
before any path join or command string interpolation.

### Slice 5 — Guard the destructive verbs
Confirmation or `--force` on `phil reset`; exact-argv matching (the pattern
`doctor.rs:541` already implements) instead of substring matching in `flush.rs`
and `footprint.rs`.

### Slice 6 — Make the documentation stop lying
Rewrite `PHILOTIC_WEB_PROPOSAL.md`'s security section to describe the shipped
system and mark the mTLS/action-grant design as intended-not-built; delete the
two stale "Reality Gap" sections that contradict their own "Current Slice";
update `MCP_MEMBRANE_HARDENING_PROPOSAL.md`'s disposition; bring `DEFECTS.md`
current; add `crates/philotic-web/README.md`.

### Slice 7 — Populate `seam:management-plane-security`
Link the seam to the code and tests that implement it and record a verification
level. Until this exists, no claim about philotic-web's security posture is
checkable by the graph.

## Disposition

`proposed`. No code has been changed by this analysis.

## Current Slice

None claimed. Slice 1 is the recommended starting point: it is small,
independently verifiable, and removes a plaintext credential exposure that
exists on every machine that has run `phil onboard`.
