---
title: Philotic Web Hardening — The Management Plane Nobody Declared
doc_type: proposal
domain: operator-control-plane
status: active
disposition: accepted-current-slice
last_updated: 2026-08-07
verification_level: test-green
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
became: the mesh management plane and the Apple edge client's server. This
proposal invents no new architecture — the intended one is already written
across six accepted proposals. It closes the distance between that intent and
the shipped code, and it populates the one seam the graph has declared since
March and never filled: `seam:management-plane-security`.

**Baseline:** `cargo test -p philotic-web` is green on `develop` at
`1a366e9f` — 173 unit + 10 integration tests, 0 failures. Nothing here is a
regression; these are gaps, not breakage.

## Evidence Discipline

Per [AGENTS.md](../../AGENTS.md) §2.4, every claim is labelled:

- **PROVEN** — the cited code was read on `develop` during this analysis.
- **INFERRED** — reported by a review pass, not independently re-verified.
- **INTENDED** — a proposal says so; no implementation claim attached.

All line numbers are against `develop` at `1a366e9f`.

> A note on how this was checked: the first draft cited an older `main`
> revision and described an OIDC token exchange that `develop` had already
> replaced with the hotel-delegated one. Every citation below was re-verified
> after rebasing onto `develop`. Where a review pass and the source
> disagreed, the source won — §5.2 in particular was rewritten after a clean
> worktree contradicted it.

## 1. The Frame: Scope Drift Without a Trust-Model Update

**PROVEN.** `crates/philotic-web/src/serve.rs` routes a full remote-hotel
administration surface (module header, serve.rs:23–60; route table
serve.rs:~840–895):

- `POST /api/mesh/targets/:node/vault` — add vault entries on another hotel
- `POST /api/mesh/targets/:node/secrets/rotate` — rotate another hotel's secrets
- `PUT /api/mesh/targets/:node/config/:key` — write another hotel's config
- component create / patch / delete / enable / disable / restart, local and remote
- `PUT .../agents/:agent/roles/:role/home` — move a role between hotels
- cron create/delete, skill assign/revoke, guest restart/stop

`POST /api/components` accepts an arbitrary `command`, `args`, and `env`
(serve.rs:5224–5244) which the hotel spawns as a supervised subprocess. That
is the operator's legitimate job — and it is why **any** authentication gap in
this crate is remote code execution, and why the mesh variant extends that to
*other* hotels. There is no read-only or reduced-trust session tier to fall
back on: every session is minted `posture: "admin"` against the single
`default_operator_user_id(hotel)` (serve.rs:7589, 7680).

**And philotic-web is the only thing checking.** The hotel does not
re-authorize what arrives over the UDS — operator-session auth, the typed
confirmation strings, and the remote config-key allowlist exist *only* in this
crate (§8.1). That makes every finding below a hole in the sole wall rather
than one layer of several, and it is the single most important fact in this
document.

**PROVEN — there is no TLS.** `Cargo.toml:32` pulls `rustls` only through
`reqwest`, for *outbound* calls. There is no `axum-server` and no server-side
rustls config; serve.rs:903–906 binds a plain `tokio::net::TcpListener` and
hands it to `axum::serve` (serve.rs:954–959). Every printed URL is `http://`.

**PROVEN — the cookie is consistent with that, and that is the problem.**
serve.rs:2258–2260:

```
{AUTH_COOKIE_NAME}={...}; Path=/; HttpOnly; SameSite=Strict; Max-Age={...}
```

`HttpOnly` and `SameSite=Strict` are correct and deliberate — and `SameSite`
is currently the *entire* CSRF defense, since there are no CSRF tokens on any
mutating route. `Secure` is absent, necessarily, because there is no TLS to be
secure over. On any non-loopback bind the admin session cookie, the edge
bearer tokens, and the secret plaintexts posted to `/api/vault` all cross the
wire in cleartext. On a Tailscale bind WireGuard supplies confidentiality; on
a LAN or `0.0.0.0` bind nothing does.

Someone already saw this: `jareds-macbook-air.tail28cc54.ts.net.crt` and
`.key` sit gitignored in the repo root. Tailnet TLS material was obtained and
never wired in.

## 2. Findings That Are Live Today on a Loopback Bind

These do not require a non-loopback deployment. They are true right now.

### 2.1 `GET /api/auth/status` leaks operator identity and vault key location pre-login — **PROVEN, high**

serve.rs:1568–1592 resolves the session **only to set a boolean**:

```rust
let session = current_operator_session(&headers, &state);
let root_user_key_refs = list_root_user_key_refs(&state.db_path, &state.hotel)...;
let external_identity_links = list_external_identity_links(&state.db_path, &state.hotel)...;
let oidc_providers = list_oidc_provider_statuses(&state.socket, Some(&headers)).await;
Json(AuthStatusView { authenticated: session.is_some(), ... })
```

`root_user_key_refs` and `external_identity_links` are returned regardless of
`authenticated`. That payload includes vault-ref strings such as
`keychain://ai.philotic.hotel-vault/default-root-key` or
`env://PHILOTIC_VAULT_MASTER_KEY/<key-id>`, a `sha256:` fingerprint prefix of
the vault master key, and — once OIDC is used — the operator's email, GitHub
login, display name, and provider subject.

Failure scenario: at `Local` tier the fence returns `true` for everything
(serve.rs:2159–2161), so any local process, any other account on a shared Mac,
or any web page that can reach `127.0.0.1:7700` reads this before logging in.
There is no `Host` header allowlist (§4.4), so a DNS-rebinding page harvests it
from a foreign origin. On this machine the live DB holds **2 root key refs**
that this route would disclose.

**Fix:** return only `authenticated`, `hotel`, and provider *names* when
unauthenticated.

### 2.2 The bootstrap token is unlimited-use, never expires, and is printed to stdout — **PROVEN, medium**

serve.rs:632–635 mints 24 random bytes; serve.rs:938 prints
`Bootstrap token: philotic-<48 hex>` to stdout. It has no TTL, no one-shot
consumption, and mints `posture: "admin"` sessions for the life of the
process. serve.rs:1598 compares it with `!=` — a non-constant-time `String`
comparison, in a file that defines and uses `constant_time_eq` (serve.rs:2107)
everywhere else — and `POST /api/auth/bootstrap` has **no throttle**, unlike
edge enrollment which does.

The 24-byte token makes brute force impractical, so the real exposure is the
log: anything that captures this process's stdout retains an admin-minting
credential indefinitely.

### 2.3 Session tokens carry 64 bits of entropy and are stored in plaintext — **PROVEN, medium**

serve.rs:6989–6993 generates session tokens, OIDC state nonces, and session
ids as `format!("{:016x}", rng.gen::<u64>())`. `thread_rng()` is a CSPRNG, so
values are unpredictable, but 64 bits is below the 128-bit norm for a bearer
credential — and this is the one that persists for 8 hours, while the
bootstrap token uses 24 bytes and edge tokens 32. Tokens are stored unhashed
in `operator_sessions`, so read access to the DB file replays every live
session.

Neither `operator_sessions` nor `operator_auth_challenges` is ever reaped. The
live DB currently holds 5 sessions (4 active) and 2 pending challenges — small
today, unbounded by design.

### 2.4 `POST /api/auth/challenges` is unauthenticated and unbounded — **PROVEN, medium**

serve.rs:1830–1901 has no `check_auth`. `auth_path` is allowlisted to
`membrane_challenge|oidc`, but `bind_label` and `verifier_hint` are free text,
and every call INSERTs a row with no cap. Any local process can grow the
context DB without limit, and the `bind_label` it plants is later used as a
redirect target (§3.3).

### 2.5 `/api/config/telegram` returns the first 8 characters of the live bot token — **PROVEN, medium**

serve.rs:3950–3956:

```rust
let hint = val.as_str().map(|s| format!("{}…", &s[..s.len().min(8)]))
```

A Telegram bot token is `<numeric bot id>:<secret>`; its first 8 characters are
the identifying, non-random half. This is the only route in the file that
returns any part of a secret value — every sibling route returns a boolean and
a ref. It should return a boolean too. (Minor: byte-slicing at index 8 would
panic on a non-ASCII stored value.)

### 2.6 Error bodies return raw `err.to_string()` throughout — **INFERRED, medium**

Pervasive (e.g. serve.rs:2304–2308, 3127–3131, 4380–4386). Callers learn
absolute DB paths, SQLite error text, IPC response debug, and internal
role/guest names. Combined with §2.1 this is a useful pre-login reconnaissance
surface.

## 3. The OIDC Flow: Excellent Mechanics, Missing Authorization

**PROVEN — the mechanics are the best security code in the crate**, and the
one place the intended architecture actually landed. PKCE is real
(`code_challenge_method=S256`, serve.rs:8472). State is a server-minted nonce
persisted with the challenge and validated on callback against `auth_path`,
`verifier_kind`, `status == "pending"`, and expiry (serve.rs:1979–1988).
Consumption is a single atomic conditional UPDATE (serve.rs:8069–8087), so a
state token cannot be replayed — and there is a test for it (serve.rs:9137).
Critically, **client secrets never enter philotic-web**: the code exchange is
delegated to the hotel over IPC (`exchange_oidc_identity_via_hotel`,
serve.rs:8591–8642), exactly as `PERIMETER_EGRESS_CONTROL_PROPOSAL.md`
requires. `oidc_loopback_bootstrap_only` (serve.rs:8544) refuses OIDC outright
when the base URL resolves to loopback.

Then the authorization step is missing.

### 3.1 Any subject who completes the flow becomes the root operator with admin posture — **PROVEN (code), latent (deployment), critical-if-configured**

`upsert_operator_external_identity_link` (serve.rs:7672–7680) hardcodes the
identity it binds to:

```rust
let user_id = default_operator_user_id(hotel);   // "root-user:{hotel}"
```

There is no comparison of `identity.provider_subject` or `identity.email`
against anything. The callback then unconditionally issues a session
(serve.rs:2043–2049) with `posture: "admin"` (serve.rs:7589). Worse, if the
current mesh principal still starts with `local-user:`, the link **rebinds**
it to `user:{provider}:{subject}` derived from whoever just logged in
(serve.rs:7698–7715), and broadcasts that as a `ProjectedUserIdentitySync`
mesh event (serve.rs:7918–7950).

**I checked the hotel side, because that is what decides the severity.**
`crates/aiua/src/service/ipc.rs:9645–9660` does authorize the exchange — but
it authorizes the *caller*, not the *subject*:

```rust
let authorized = current_identity.as_ref().is_some_and(|identity| {
    identity.role == "management" && identity.guest_id == "philotic-web-oidc"
});
```

Those two strings are exactly what philotic-web self-asserts when connecting
(serve.rs:8597–8605). A workspace-wide search for `allowed_subject`,
`allowed_email`, `subject_allowlist`, `allowed_domain`, or any equivalent
returns **nothing**. No allowlist exists at either tier.

Failure scenario: the operator configures Google OIDC for
`https://brain.example.com`. Anyone on the internet who reaches that callback
host completes a normal Google login **with their own account**, receives a
`philotic_session` cookie with `posture: "admin"`, and then `POST
/api/components` with `command: "/bin/sh"` — RCE on the hotel host, plus read
of `/api/secrets` and write of `/api/vault`, plus the same against every
reachable hotel via `/api/mesh/targets/:node/*`.

**Why this is not currently live** (verified on the machine that matters, and
why it is still the top priority):

philotic-web **is** deployed on vps-jane, the public-IP host —
`/opt/philotic/bin/philotic-web serve --port 7700 --db
/opt/philotic/data/aiua_context.db` (binary dated 2026-08-05). Its own
`node_config` has no `oidc*`, `web_bind`, or `web_edge*` keys, and its
`external_identity_links` and `operator_sessions` tables both hold **0 rows**.
No provider is configured, and the flow has never been completed anywhere.
`oidc_loopback_bootstrap_only` covers the local case.

This fires the day someone configures a provider with a public base URL —
exactly what the `oidc_public_base_url` support exists for, on a host that is
already serving the control plane.

> **Correction to an earlier draft.** This section first cited "0 rows on
> mac-jane" from `~/.philotic/bjork/aiua_context.db`. That file is not the
> hotel DB (mac-jane's is `context.db`); it is the session store of the stale
> UAT process described in §13, and its five tables are exactly the ones
> `ensure_operator_auth_tables` creates. It was also the wrong machine —
> the risk is about a public-IP host. Both were re-checked above.

**Fix:** an explicit subject/email allowlist checked before
`upsert_operator_external_identity_link`, failing closed when unset.

### 3.2 `redirect_uri` can be derived from attacker-controlled headers — **INFERRED, high**

`operator_auth_public_base_url` (serve.rs:8480–8509) falls back to
`x-forwarded-host` / `x-forwarded-proto` when no base URL is configured, and
that value becomes `provider.redirect_uri` — sent to the IdP *and* passed to
the hotel's token exchange. `oidc_loopback_bootstrap_only` only blocks the
literal loopback hosts. The IdP's own registered-redirect enforcement is the
only remaining control. There should be a host allowlist here.

### 3.3 Open redirect via `bind_label` — **PROVEN, medium**

serve.rs:2059–2065 uses the stored `bind_label` verbatim as the `Location`
header. `sanitize_return_path` (serve.rs:8557) exists and is applied at
*start* time (serve.rs:1784) but never at redirect time — and §2.4 lets an
unauthenticated caller create a challenge with an arbitrary `bind_label`.

Today this is not exploitable: such a challenge has `exchange_secret: None`
and the callback bails at serve.rs:1990–1998 before redirecting. **That guard
is incidental, not intentional.** Any future path that populates
`exchange_secret` on an externally-created challenge turns this into an open
redirect that hands over a freshly minted admin cookie on the way out.

## 4. Perimeter and Fence

**Done well, and worth stating plainly.** `edge_fence_allows`
(serve.rs:2152–2186) is deliberately *stricter* than the shared
`perimeter_core::fence::check_ingress` — which allows `Lan` unauthenticated —
and says so in a comment explaining why. Any non-`Local` bind requires a valid
operator session on every `/api/` route and `/ws`. Edge bearers are scoped to
`/api/edge/*` **structurally, at three independent layers**: the fence
(serve.rs:2174–2185), the credential store (edge tokens live in a JSON file
and are never rows in `operator_sessions`, so `check_auth` cannot resolve
one), and a per-handler re-check. There is a test asserting an edge bearer
cannot open `/api/secrets`, `/api/vault`, `/api/config/*`, `/api/event-log`,
or `/ws` (serve.rs:9622–9687). This is the right design, correctly built.

### 4.1 Public binds are classified as `Lan`, silently disabling two Internet-tier protections — **PROVEN, medium**

serve.rs:607:

```rust
let bind_classification = classify_bind_addr(bind_host, false);
```

The second argument is `has_public_ip`, hardcoded `false`. In
`crates/perimeter-core/src/classifier.rs:34–44` a `0.0.0.0` bind with
`has_public_ip == false` returns `Lan`, not `Internet`. `ExposureTier` is
ordered `Local=0, Lan=1, Mesh=2, Internet=3`
(`crates/ansible-mesh-core/src/lib.rs:106–117`), and two guards key on that
ordering:

```rust
if path == "/api/edge/enroll" {
    return state.exposure_tier <= ExposureTier::Mesh;   // Lan(1) <= Mesh(2) → open
}
...
if peer.is_loopback() && state.exposure_tier <= ExposureTier::Mesh {
    return true;                                        // loopback bypass stays on
}
```

Failure scenario: on vps-jane (public IP `31.97.130.98`), a `0.0.0.0` bind
classifies `Lan`. Device enrollment — which the code's own comment says "a
static invite is not strong enough for a public-facing bind" — stays open to
the internet, and the loopback auth bypass that `Internet` tier is designed to
remove stays active for every local process on that host.

Not an auth bypass — the session requirement still holds at every non-`Local`
tier.

**Live status, corrected.** An earlier draft said "nothing sets a non-loopback
bind." That is wrong: vps-jane's philotic-web is bound to
`100.64.212.8:7700` — its **Tailscale** address. That is CGNAT space, so
`classify_bind_addr` returns `Mesh` **correctly**, WireGuard carries the
confidentiality the missing TLS does not, and the open-enrollment and
loopback-bypass behaviours at that tier are the documented design rather than
the bug. The `has_public_ip` defect is genuinely latent — it needs a
`0.0.0.0` or public-IP bind to fire, and today's deployment is neither.

That said, the deployment posture is one config edit away: a `web_bind` of
`0.0.0.0` on that same host silently drops it to `Lan` and reopens both. Note
also that no config file sets the bind — it arrives by environment or DB, so
grepping `ansible/` and `scripts/` does not answer this question. Observed
listener state does.

**Fix:** probe for a public IP, or refuse `0.0.0.0` without an explicit
acknowledgement.

### 4.2 No `Host` allowlist; no `X-Content-Type-Options` — **INFERRED, medium**

DNS rebinding reaches the listener from a foreign origin; combined with §2.1
that is a real pre-login data grab. `nosniff` is set nowhere in the file, while
static assets are served with a guessed MIME. Separately, the index route's
CSP is genuinely tight (`default-src 'self'; object-src 'none';
frame-ancestors 'none'`, serve.rs:1294–1317) — but static assets get no CSP
and `/setup-guide` gets no security headers at all. CORS defaults are correct:
no `Access-Control-Allow-Origin` unless `--allow-origins` is passed, and
`allow_credentials` is never enabled.

## 5. The Edge Client Surface

### 5.1 The shared edge token can impersonate any enrolled device — **PROVEN, high**

`serve/edge.rs:1062–1069`:

```rust
if let EdgeBearerIdentity::Device(token_node) = auth {
    if token_node != &hello.node_id { /* node_mismatch */ }
}
```

The node-binding check runs **only** for the `Device` variant.
`EdgeBearerIdentity::Shared` — the `PHILOTIC_WEB_EDGE_TOKEN` / config
`web_edge_token` path — skips it entirely, so its holder may `Hello` as any
enrolled node. Node ids are not secret: `/api/edge/sessions` returns session
ids of the form `operator-chat:edge:{node_id}:{agent}`, and any edge bearer can
list them.

Failure scenario: a holder of the shared token enumerates node ids, then
Hellos as the operator's phone — `replay_after(node_id, 0)` dumps that
device's retained conversation frames, `begin_session` evicts the real device,
and every turn it submits is attributed to the victim device.

The existing test `handshake_binds_device_token_to_its_node` (edge.rs:2450)
exercises the `Shared` variant Hello-ing as another node and **asserts it
succeeds** — the gap is encoded as intended behaviour, so no test will catch
its removal.

**Fix:** bind the shared token to a node, or drop the shared path in favour of
per-device tokens only.

### 5.2 Client-chosen `conversation_id` writes into another session's history — **INFERRED, high**

`edge.rs:1565–1566` takes `conversation_id` from the client whenever present
(defaulting only when absent), and `serve.rs:3052` uses it verbatim as the
hotel `session_id`. `/api/edge/sessions` hands out the exact ids to target.
A device can therefore submit a turn into another device's — or the desktop
operator's — conversation, poisoning its history and receiving a reply
carrying that conversation's accumulated context.

### 5.3 Edge turns are stamped with operator provenance — **INFERRED, high**

`submit_operator_chat_turn` is the same function for the desktop chat route
and the edge WS (serve.rs:3017). It emits `EmitTask` with a client-chosen
`target_node` and `target_guest_id` and the identical
`"source": "operator_chat"` stamp the authenticated desktop operator gets;
only `operator_session_id` differs (`edge:{node}` vs `desktop-membrane`).
Downstream consumers cannot distinguish a low-trust phone from the operator at
the console unless they special-case that one string.

### 5.4 Enrollment: static invite, no expiry, no revocation — **INFERRED, medium**

The invite comparison is constant-time (edge.rs:250–253) and brute force is
throttled — but the throttle is a **single global queue** (edge.rs:178,
311–319), so 5 wrong codes from any source block *all* enrollment for 60s, and
the project's own test asserts that a **correct** invite is refused during the
window (edge.rs:2607–2625). The invite itself is a static string with no
expiry and no use count; issued tokens never expire; there is no revocation
API, so a lost device can only be removed by hand-editing
`edge-devices.json`. `device_pubkey_b64` is stored but never used for any
cryptographic proof — the field implies a device-key binding that does not
exist.

An enrollment token grants: the agent directory, **all** operator session
history and turn content, full LifeGraph read *and* write, 25 MiB blob upload,
and turn submission.

### 5.5 In-memory replay ring: silent message loss across restart — **INFERRED, medium**

The durable cursor ledger is a named, unbuilt seam (TODO at edge.rs:71–77).
Beyond the known UX cost, the security-relevant half is a silent-failure mode:
after a philotic-web restart the ring resets to `next_seq: 0` while clients
still hold cursors from the previous process, and the delivery arm only
forwards `if seq > last_sent_seq` — so a device reconnecting with a stale
cursor of 400 receives **nothing at all**, with no error and no resync, until
the counter climbs past 400.

### 5.6 What the edge path gets right

Auth strictly precedes side effects: the bearer is validated at the HTTP
upgrade (401 before `on_upgrade`), then a `Hello` must arrive within 30s, then
enrollment and device-node binding are checked — and only then is any session
evicted. An unauthenticated party who merely knows a node id cannot evict
anything. `/api/edge/blob` has no SSRF (the destination is env-derived, never
client-controlled), no read route, and a 25 MiB limit. The delivery broadcast
handles `Lagged` by resyncing from the ring rather than dropping silently.
Enrollment logging records node id and platform but never the issued token,
and the device file is chmod `0600` *before* the rename.

## 6. Resource Limits

**INFERRED, high in aggregate.** Neither WebSocket sets `max_message_size` or
`max_frame_size`, so tungstenite 0.24 defaults apply: **64 MiB per message,
16 MiB per frame**, buffered then handed to `serde_json::from_str`. The
operator `/ws` handler (serve.rs:7167–7194) has no limits of any kind — no
idle timeout, no keepalive ping, no connection cap — and holds a broadcast
receiver until the OS TCP timeout. The edge WS has a 30s pre-Hello timeout but
no post-handshake idle timeout and no cap on concurrent sessions. Retained
frames are minted for every enrolled node and rings are never GC'd for devices
that never connect. There is no rate limit, request timeout, or concurrency
limit layer anywhere in the router.

Credit where due: `/api/edge/blob` and `/api/edge/lifegraph/observe` carry
explicit `DefaultBodyLimit` overrides, every other JSON route inherits axum's
2 MiB default, `/api/event-log` clamps its limit to `1..=200`, the STT command
channel is a bounded `mpsc(128)` using `try_send`, and the STT relay has a
120s inactivity ceiling.

## 7. Secret Handling and Local Surface (CLI / PWA)

### 7.1 Onboarding writes live secrets world-readable — **PROVEN, high**

`src/onboard.rs:256` calls `std::fs::write(config_path, &pretty)` on a value
that contains the Muninn root password (onboard.rs:235–241) and per-agent
Telegram bot tokens (onboard.rs:406–413). No `set_permissions` follows, so the
file lands at the umask default — typically `0644`. Contrast `init.rs:75–77`,
which correctly chmods the operator key to `0600`. `.gitignore` covers
`mesh-config.*` but not `~/.philotic/<profile>/config.json`.

**Fix:** `OpenOptions::new().mode(0o600).create_new(true)` — one call.

### 7.2 The embedded UI is an unreproducible stale local cache — **PROVEN, medium**

This was first written up as "a placeholder API key ships in every binary."
A clean worktree corrected it, and the corrected version is more serious.

**Not true:** `crates/philotic-web/.gitignore` contains `ui-dist/` — the built
UI is gitignored. `config.js` (carrying
`window.MONGODB_API_KEY = 'test-api-key-12345'` and a comment saying "In
production, this file is generated by Docker entrypoint with the actual API
key") is **not committed**, nor is `index.html`, `service-worker.js`, or any
of the ~690 asset files in the developer's working copy. A clean checkout has
exactly two files under `ui-dist/`, both force-added orphans referenced by no
`index.html`. They should be deleted.

**True, and worse:** `build.rs:36–43` never rebuilds once `ui-dist/index.html`
exists — it prints `Reusing cached ui-dist/` unless `PHILOTIC_REFRESH_DESKTOP_UI=1`
is set — and `rust-embed` bakes that directory into the binary wholesale.

Failure scenario: the operator UI inside any locally built, and therefore any
Homebrew-pushed, `philotic-web` binary is a snapshot of one developer's
working directory from an arbitrary past date — currently 690 files including
52 generations of `index-*.js` and a `.DS_Store`. Every superseded bundle stays
fetchable by hash from the running server forever, so a UI security fix does
not reliably reach a deployed binary and the old vulnerable bundle is still
served beside the new one. Nothing in the repo lets you audit which UI a given
binary carries.

**Fix:** pin a desktop-repo commit and record its hash in the binary, or fail
the build on a stale `ui-dist/` instead of silently reusing it.

### 7.3 Unvalidated identifiers reach a persisted shell hook — **PROVEN, medium**

`src/harness.rs:3070` interpolates `harness_id` into a command string that is
written into `.claude/settings.json` as a `SessionStart` hook — executed at
every future session start. `harness_id` is never character-validated, and the
same raw value is joined into home-directory paths (harness.rs:2680–2688).
Windsurf workflow and skill filenames are built from intel-graph content
(harness.rs:3204, 3228), a surface agents can write to over MCP.

**Fix:** an `[a-z0-9._-]+` allowlist at every identifier entry point.

### 7.4 `phil reset` wipes keys and memory with no confirmation — **PROVEN, medium**

`src/reset.rs:77–88` calls `fs::remove_dir_all` on `~/.philotic` and then
unconditionally on `~/.muninn`. No prompt, no `--yes`, no backup. Without
`--keep-identity` this destroys the operator ed25519 keypair, every profile's
context DB, and the entire Muninn engram store. This fleet has already lost
operator data to an unguarded destructive verb (PR #404).

### 7.5 Remaining CLI/PWA findings — **INFERRED**

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
| Edge bearer tokens stored cleartext in `edge-devices.json`, never expiring | edge.rs:122–131 | med |
| LifeGraph `depth`/`max_nodes`/`edge_limit` forwarded unclamped despite a doc comment claiming bounds | edge.rs:939–951 | low |
| `/api/edge/lifegraph/observe` forwards 25 untyped `Value` observations into a write with no schema check | edge.rs:959–1008 | med |
| Mesh invite trust material written `0644` into CWD | mesh.rs:49 | low |
| No CSP or referrer policy on the control-plane shell; bundle hardcodes `api.jaredlikes.com` | ui-dist/index.html | low |
| `home_dir().expect(...)` panics on stripped `$HOME` | init.rs:11, service.rs:42, reset.rs:82 | low |

## 8. philotic-web Is the Only Authorization Boundary — and That Raises Every Stake Above

An earlier draft of this section called philotic-web "closer to a pass-through
than a bastion." **That was exactly backwards, and the correction is the most
important thing in this document.**

### 8.1 The hotel does not re-authorize — it trusts the socket — **INFERRED, high**

Three controls exist **only** in philotic-web and are replicated nowhere in
the hotel: operator-session authentication, the exact-match confirmation
strings on destructive routes, and the remote config-key allowlist. The
corresponding `aiua` IPC handlers perform no caller check at all:

| Handler | ipc.rs | Caller identity check |
|---|---|---|
| `AddVaultEntry` | 4807 | **none** — caller also chooses the new secret's `allowed_roles` |
| `SetConfig` | 4777 | **none** — only a `__mcp_` reserved-prefix filter |
| `RegisterComponent` | 7956 | **none** — and with `auto_start` it spawns the process immediately |
| `SetComponentActive` (stop) | 7978 | **none** |
| `RestartComponent` | 7988 | `steward_agent_admin_gate` — **fails open** for exactly this caller class |

`steward_agent_admin_gate` (ipc.rs:15156–15185) names the pass-through class
in its own doc comment: *"NOT an agent (heal-dispatcher, `phil` CLI, web,
operator surface): proceed exactly as before this gate existed."*

philotic-web connects asserting its own identity (serve.rs:8597–8605):

```rust
GuestIdentity { guest_id: "philotic-web-oidc".into(), role: "management".into(), ... }
```

That identity is **self-asserted** — no handshake, no signature, and peer
credentials are structurally unreachable (discarded at accept; `aiua` declares
neither `libc` nor `nix`). So any local process, any materialized guest, or
anything a guest executes can dial `~/.philotic/<profile>/aiua-<hotel>.sock`,
claim `role: "management"`, and issue the same `AddVaultEntry` /
`RegisterComponent` / `SetConfig` requests — bypassing all three controls.
**`RegisterComponent` with `auto_start` is therefore unauthenticated arbitrary
command execution for anything that can reach the socket.**

The socket carries no explicit mode. On this machine it happens to be
`srwxr-xr-x` (`~/.philotic/bjork/aiua-mac-jane.sock`), which denies non-owner
`connect()` — but that is umask-derived, not enforced. The code documents this
as accepted residual risk (ipc.rs:4204–4207: *"holding the hotel socket is
already root-equivalent"*), a premise the socket's own creation does not
establish.

**Why this inverts the reading of everything above:** philotic-web is not a
convenience layer in front of an independently-guarded hotel. It *is* the
guard. Every finding in §2, §3, and §5 is therefore a hole in the only wall,
not a redundant one.

### 8.2 A mesh peer is effectively admin on every reachable hotel — **INFERRED, high**

When `POST /api/mesh/targets/:node/vault` reaches the **remote** hotel, that
hotel verifies the *sending node's* HMAC and then re-runs the mutation through
its own unauthenticated local handlers. The caller's identity does not survive
the hop:

- The `*OperatorTarget*` family is dispatched without `current_identity` even
  being passed (ipc.rs:5356–5380; `operator_surface.rs:65–71` has no such
  parameter).
- The forwarded payload hardcodes `caller_kind: "operator_surface_adapter"`,
  `caller_id: local_node_id`, `visibility_scope: "operator"`, and
  `grant_scope: "default"` as literals, never validated on receipt
  (`operator_surface.rs:1763–1781`) — and carries the secret **plaintext**.
- Receive-side verification is HMAC + nonce on the *node*
  (`execution_transport.rs:97–124`), and it is conditional on
  `enable_rust_auth`, overridable via `PHILOTIC_ENABLE_RUST_AUTH`
  (`main.rs:762–764`).
- The worker then substitutes its **own** identity
  (`aiua-operator-surface-query-worker`, `main.rs:469–476`) and rewrites the
  target to itself (`main.rs:283`), landing in the ungated local handlers.

The one receive-side authority check (role-home,
`operator_surface.rs:1827–1838`) validates `calling_role` — a self-asserted
string read off the wire (`main.rs:371`). Config writes are key-allowlisted;
secrets and components are not.

This means mesh membership, not operator identity, is the real authorization
boundary for cross-hotel mutation — which is precisely what
`HOTEL_PERIMETER_TRUST_PROPOSAL.md` lists as **not** implemented (no
revocation, no capability-scoped trust). It is also a bigger lever than any
single web-tier finding, and it belongs in the same seam.

## 9. Test Coverage

**Green baseline:** 173 unit + 10 integration tests pass on `develop`.
Coverage of the fence and edge protocol is genuinely good — fence semantics per
tier, edge-bearer scoping against seven named operator routes, enrollment
throttling, handshake device-node binding, ring replay/ack/eviction, session
revocation, OIDC state single-use consumption, and a real e2e WebSocket test
against a fake hotel.

The gaps are the findings above, and they are gaps *because* nothing asserts
the invariant:

| Untested | Corresponding finding |
|---|---|
| An unexpected OIDC subject must not receive a session | §3.1 |
| A shared token must not Hello as an arbitrary node (current test asserts the opposite) | §5.1 |
| A submitted `conversation_id` must belong to the submitting device | §5.2 |
| An unauthenticated caller must not read key refs or identity links | §2.1 |
| Ring seq reset across a process restart | §5.5 |
| Oversized WS frames, idle connections, concurrent-session floods | §6 |
| `X-Forwarded-Host`-derived `redirect_uri` | §3.2 |
| `/api/secrets` and `/api/config/*` never return a plaintext value | §2.5 |
| `keys.rs` has zero tests | — |

## 10. Remediation Sequence

Ordered by risk reduction per unit of work.

**Slice 1 — Close the pre-login leaks (small, no design decisions).**
Gate `/api/auth/status` (§2.1). Drop the Telegram `token_hint` (§2.5).
`0600` on everything `onboard.rs` and `mesh.rs` write, and extend the `doctor`
permission check to `vault-master-key.env` (§7.1). Add `nosniff` and a `Host`
allowlist (§4.2).

**Slice 2 — Authorize OIDC before it is ever deployed publicly (§3.1, §3.2).**
Add a subject/email allowlist that fails closed when unset, plus a host
allowlist for the derived `redirect_uri`. Add the negative test. This is the
highest-severity item and it is cheap *today*, while `external_identity_links`
is still empty.

**Slice 3 — Fix edge identity (§5.1, §5.2, §5.3).**
Bind the shared token to a node or remove the shared path; validate that a
submitted `conversation_id` belongs to the submitting marker; give edge-origin
turns a provenance stamp distinguishable from the desktop operator's. Rewrite
the test that currently asserts the impersonation path succeeds.

**Slice 4 — Bound the resources (§6).**
`max_message_size` / `max_frame_size` on both sockets, an idle timeout on
both, and a request-timeout + concurrency-limit layer on the router.

**Slice 5 — Decide the TLS question (§1).**
Either terminate TLS using the tailnet certs already in the repo root and set
`Secure` on the cookie, or declare that non-loopback binds are supported
**only** over Tailscale and enforce it by refusing a non-CGNAT non-loopback
bind without an override. Doing neither is today's state and the weakest
option. Fix the `has_public_ip` classification (§4.1) as part of this.

**Slice 6 — Harden the local CLI surface (§7.3, §7.4).**
Identifier allowlist; confirmation on `phil reset`; exact-argv matching in
`flush.rs`/`footprint.rs` using the pattern `doctor.rs:541` already implements.

**Slice 7 — Credential lifecycle (§2.2, §2.3, §5.4).**
TTL + one-shot bootstrap token with a constant-time compare and a throttle;
128-bit session tokens, hashed at rest; expiry and a revocation API for edge
devices; reaping for `operator_sessions` and `operator_auth_challenges`.

**Slice 8 — Close the IPC identity gap (§8.1, §8.2).**
This is listed last by *sequence*, not by importance — it is the largest piece
of work and it changes a boundary other threads build on, so it wants its own
seam and its own slice. Two halves: give the hotel a caller identity it can
actually verify (peer credentials or a registration handshake) so
`AddVaultEntry` / `RegisterComponent` / `SetConfig` stop being reachable by
anything that opens the socket; and preserve caller authority across the mesh
hop instead of substituting the worker's identity. Until this lands, treat
philotic-web as the sole wall and weight §§1–7 accordingly.

**Slice 9 — Make the documentation stop lying (§11).**

**Slice 10 — Populate `seam:management-plane-security`.**

## 11. Documentation Truth

**INFERRED** (documentation review, citations spot-checked):

1. **`PHILOTIC_WEB_PROPOSAL.md` is the most misleading doc in the repo.** Its
   whole Security Model section — threat model, mTLS management port, PKI mesh
   CA, action grants, `ManagementEvent` audit ledger, envelope-encrypted
   `secret set` — describes a system that does not exist, while the system
   that does exist appears nowhere in it. Anyone threat-modelling from it
   models the wrong program. Its frontmatter self-conflicts (`status: proposed`
   + `disposition: accepted-current-slice` + a body disposition of `proposed`).
2. **Two accepted proposals contradict themselves.** Both
   `OPERATOR_AUTH_BOOTSTRAP_STRATEGY_PROPOSAL.md` and
   `HOTEL_USER_IDENTITY_AND_OPERATOR_AUTH_PROPOSAL.md` claim a capability
   implemented in "Current Slice" and deny it exists in "Reality Gap" in the
   same file. The Reality Gap sections are the stale halves.
3. **`MCP_MEMBRANE_HARDENING_PROPOSAL.md` reads `proposed` /
   `implemented_by: []`** while DEFECTS.md records H1–H4 fixed 2026-07-15
   (DEF-053…DEF-056).
4. **"philotic-web never holds secret material"** was false until the egress
   migration; it is true now, and that is the one place the intended
   architecture landed (§3).
5. **`docs/DEFECTS.md` stops at DEF-077 / 2026-07-29** while DEF-078 and
   DEF-079 exist in merged PRs, so the open-defect list understates reality.
6. **The three edge seam diagrams are empty placeholders** — each
   `docs/architecture/generated/seam-edge-*.puml` is a single orange rectangle.
7. **`crates/philotic-web/README.md` does not exist**, against the repo's own
   documentation standard.

## 12. The Declared Seam Was Never Populated

**PROVEN.** `doc:philotic-web` declares five seams. All five are empty shells
in the intel graph — one incoming `applies_to` edge, zero outgoing edges, no
`file_path`, no verification:

| Seam | Outgoing edges | file_path |
|---|---|---|
| `seam:management-plane-security` | 0 | null |
| `seam:philotic-web-crate` | 0 | null |
| `seam:distribution-pipeline` | 0 | null |
| `seam:repo-identity` | 0 | null |
| `seam:binary-rename-ansible-to-aiua` | 0 | null |

The proposal carries `verification: none`. The graph has named the exact seam
this analysis is about since March and never attached a single fact to it.
Until it is populated, no claim about philotic-web's security posture is
checkable by the graph — which is how a management plane grew a remote
mutation surface without anyone's dashboard turning a different colour.

## 13. Operational Findings (Not Code)

**A nine-day-old UAT control plane is running against the live profile.**
On mac-jane, PID 24009 has been running since 2026-07-28:

```
/opt/homebrew/Cellar/philotic-web/0.1.0-alpha/bin/philotic-web serve --port 7700
  --db /Users/jaredlikes/.philotic/bjork/aiua_context.db
  -c /Users/jaredlikes/.claude/jobs/5c81b5e7/tmp/mesh-config-uat.json
```

Its config path is inside a **deleted job tmp directory**, it holds a
desktop-membrane lease, and it listens on `127.0.0.1:7700` — the same port a
real local `phil serve` would want. It is also what produced the misleading
"0 identity links" evidence corrected in §3.1. Recommend a deliberate
shutdown by the operator; not touched by this analysis.

Related: `serve.rs` defaults `--db` to a **relative** `aiua_context.db`, which
is how `aiua_context.db-{shm,wal}.stale-quarantine` came to sit in the repo
root. Operator sessions and identity links scatter across whatever DB each
invocation happened to point at — which is its own reason the auth tables
should live in one declared place.

## Proposed Defect Entries

To be filed in `docs/DEFECTS.md` starting at DEF-080 (DEF-078/079 are already
used by merged PRs but absent from the ledger — see §11.5):

| Proposed | Sev | Summary |
|---|---|---|
| DEF-080 | high | OIDC callback issues an admin session to any subject; no allowlist at web or hotel tier (§3.1) |
| DEF-081 | high | `GET /api/auth/status` returns vault key refs and operator identity links unauthenticated (§2.1) |
| DEF-082 | high | Shared edge token bypasses node binding; impersonation, replay, and session eviction (§5.1) |
| DEF-083 | high | Client-chosen `conversation_id` writes into another session's history (§5.2) |
| DEF-084 | high | `onboard.rs` writes Muninn root password and Telegram tokens at umask default (§7.1) |
| DEF-085 | med | `classify_bind_addr(bind, false)` downgrades a public bind to `Lan` (§4.1) |
| DEF-086 | med | No WS message/frame size caps; no idle timeout on either socket (§6) |
| DEF-087 | med | Embedded `ui-dist/` is an unreproducible stale cache; superseded bundles served forever (§7.2) |
| DEF-088 | med | Unvalidated `harness_id` interpolated into a persisted `SessionStart` hook (§7.3) |
| DEF-089 | med | Bootstrap token: no TTL, unlimited use, printed to stdout, non-constant-time compare, unthrottled (§2.2) |
| DEF-090 | med | `phil reset` wipes `~/.philotic` and `~/.muninn` with no confirmation (§7.4) |
| DEF-091 | high | `AddVaultEntry` / `SetConfig` / `RegisterComponent` / guest-stop perform no caller check; `RegisterComponent` + `auto_start` is arbitrary command execution for anything that can open the UDS (§8.1) |
| DEF-092 | high | Mesh-forwarded operator mutations lose caller identity at the hop; the remote hotel verifies the sending node's HMAC then re-runs through ungated local handlers (§8.2) |

## Disposition

`accepted for current slice` — implementation started; see below.

## Current Slice

**Landed (test-green, PR #418).** 190 unit + 10 integration tests, 0 failed.

| § | Fix | How |
|---|---|---|
| 3.1 | OIDC subject allowlist | `oidc_subject_allowlist`, **fails closed** when unset; `provider:subject`, `provider:login`, email, `@domain` |
| 2.1 | `/api/auth/status` gated | key refs, identity links, and callback URLs withheld until authenticated |
| 2.5 | Telegram `token_hint` removed | presence only, matching every sibling route |
| 5.1 | Shared edge token cannot open a device session | it carries no device identity, so it cannot satisfy the per-node binding |
| 5.2 | Chat sessions namespaced per caller | `scoped_operator_session_id` prefixes the server-derived marker |
| 3.3 | Open redirect closed | `bind_label` re-sanitized at redirect time **and** rejected at issuance |
| 2.2 | Bootstrap token compare | `constant_time_eq` |
| 6 | WS framing bounded | 1 MiB/message, 256 KiB/frame on both sockets |
| 7.1 | Secrets written `0600` | `write_private_file` (`O_CREAT|0600`), identity dir `0700` |
| 7.5 | `secrets.master-key-permissions` | new `doctor` check, Error severity, narrowing-only repair |
| 4.2 | `nosniff` | shell and embedded assets |

### Live-data checks before merge

Three of these changes alter behaviour in ways no test can observe, because
tests mint their own fresh state. Each was checked against the running fleet:

| Risk | Check | Result |
|---|---|---|
| §5.2 namespacing orphans existing chat history under a new session id | `graph_nodes` where `kind='session'`, both hotels | mac-jane 106 sessions (33 `operator-chat:`-shaped), vps-jane 178 (33) — **zero bare `conv*` ids**. Every operator-chat session already carries the `operator-chat:{caller}:{agent}` prefix, so `scoped_operator_session_id` passes them through unchanged. Nothing is orphaned. |
| §5.1 refusing the shared token strands a live device | `edge-devices.json` on both hotels | 5 devices enrolled on mac-jane, registry present on vps-jane; **every entry holds its own `edge-tok-…`**. No device depends on the shared token. |
| §7.4 the reset prompt breaks automation | `grep -rn "phil reset" justfile scripts/` | no callers. |

A residual, documented discontinuity: a client that sends a *bare* conversation
id (`conv-1`) now lands in `operator-chat:{caller}:conv-1` rather than `conv-1`.
No such session exists on either hotel today, and the id is opaque to the
client, which receives its own value back unchanged.

Two notes on method, since both changed the work:

- The first cut of §5.2 required a submitted `conversation_id` to carry the
  caller's prefix. The e2e suite rejected it at once — real clients send opaque
  ids and require them echoed verbatim. Namespacing the *hotel session id*
  instead preserves the client contract and is the stronger property: cross-
  caller addressing becomes unrepresentable rather than merely refused.
- §5.1's existing test asserted the impersonation path **succeeded**. It was
  rewritten. A test can pin a vulnerability as intended behaviour, and this one
  did.

**Not yet implemented** — the remaining items keep their analysis above:
§8.1/§8.2 (the IPC and mesh identity gap — the largest piece, and the one that
decides whether this crate is the only wall), §5.4 (edge token expiry and
revocation), §5.5 (durable cursor ledger), §2.3/§2.4 (session entropy, hashing
at rest, and reaping), §7.2 (reproducible `ui-dist`), §7.3 (identifier
allowlist), §7.4 (`phil reset` confirmation), §3.2 (`X-Forwarded-Host`
allowlist), Slice 5 (the TLS decision), and §11 (documentation truth).
