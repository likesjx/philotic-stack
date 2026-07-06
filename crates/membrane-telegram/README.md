# membrane-telegram

Telegram gateway guest, built on the `membrane` SDK. One process runs N seats
(each a bot-token + target-agent pair); `MembraneRuntime` owns the IPC lifecycle
and `TelegramSeatGuest` supplies the Telegram-specific behaviour.

## Transport

The active transport is **long-polling** (`getUpdates`). No inbound HTTP
endpoint is exposed, so there is no public attack surface today.

## Webhook mode (contract — not yet a live server)

Telegram can also *push* updates to a webhook URL. The moment such an endpoint
is exposed, the only thing separating the public internet from the guest's
inbound turn pipeline is Telegram's **secret-token contract**. This crate ships
that contract ahead of any listener so it can never be an afterthought. See
[`src/webhook_secret.rs`](src/webhook_secret.rs).

### The contract

1. **Secret required.** When `setWebhook` is called it MUST include a
   `secret_token`. Telegram then stamps every delivery with the
   `X-Telegram-Bot-Api-Secret-Token` header
   (`webhook_secret::WEBHOOK_SECRET_HEADER`).
2. **Generation.** `webhook_secret::generate_webhook_secret()` mints a
   crypto-random secret (256 bits from the OS CSPRNG, 64 hex chars — a strict
   subset of Telegram's `[A-Za-z0-9_-]`, 1–256 char rule). It is stored as a
   **vault ref, exactly like the bot token** (plaintext-retrievable, because
   `setWebhook` needs the raw value), under a per-seat config key derived by
   `telegram_webhook_secret_key`
   (`telegram_bot_token_{agent}` → `telegram_webhook_secret_{agent}`).
3. **Hard refusal to start without a secret.** `resolve_transport` turns a
   `TelegramTransport` into a `WebhookRuntime`. `Webhook { secret_ref: None }`
   is a hard error — webhook mode cannot start unconfigured. A malformed or
   unresolvable secret is likewise refused.
4. **Every request is validated.** A `WebhookRuntime` is the *only* way to reach
   `authorize_webhook_request`, which calls `validate_webhook_secret` — a
   **constant-time** BLAKE3 comparison of the request header against the
   configured secret (the same timing-safe primitive `membrane-mcp`'s
   `auth.rs` uses). Any future listener is structurally forced through this
   check: no `WebhookRuntime`, no secret; no `authorize_webhook_request`, no
   body processing.

### Wiring a future listener

```rust
use membrane_telegram::webhook_secret::{
    TelegramTransport, resolve_transport, authorize_webhook_request,
    WEBHOOK_SECRET_HEADER,
};

// At startup: refuses unless a secret is configured.
let runtime = resolve_transport(&transport, |vault_ref| fetch_secret(vault_ref))?;

if let Some(runtime) = runtime {
    // register with Telegram: setWebhook(url, secret_token = runtime.secret())
    // per request, before touching the body:
    let header = req.headers().get(WEBHOOK_SECRET_HEADER).and_then(|v| v.to_str().ok());
    authorize_webhook_request(&runtime, header)?;
}
```

## Tests

```bash
cargo test -p membrane-telegram
```

`webhook_secret`'s tests cover generation entropy/shape, accept/reject header
validation (including truncation/extension and the empty-secret guard), the
hard-refusal paths, and the request-authorization gate.
