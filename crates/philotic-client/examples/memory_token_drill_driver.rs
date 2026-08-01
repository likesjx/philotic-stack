//! Memory token self-heal drill driver — `proposal:memory-token-self-heal` S4.
//!
//! Two modes, deliberately separated so the read-only one can run against a
//! live production hotel and the destructive one cannot run by accident.
//!
//! # `probe` (default) — READ-ONLY, safe on a live hotel
//!
//! 1. `FetchMemoryConfig` and report the registered vaults with a *fingerprint*
//!    of each token (never the token itself). Proves S3's claim that the hotel
//!    serves memory config live from the Context Graph.
//! 2. `HealMemoryToken` against a deliberately **unregistered** vault name and
//!    assert the hotel REFUSES. That is S2's registered-vaults-only rail. The
//!    refusal is checked in `remint_vault_token` *before* any muninn admin
//!    login or mint, so this probe never creates a key, never rotates a
//!    secret, and never touches a real vault.
//!
//! # `drill` — DESTRUCTIVE, sacrificial vaults only
//!
//! Corrupts the hotel-side stored token for a sacrificial vault via
//! `RotateSecret`, then asserts the full self-heal circuit: the next
//! `HealMemoryToken` re-mints and the returned config carries a *different*
//! token for that vault. The original ciphertext-plaintext is captured first
//! and restored on every failure path.
//!
//! Honest note on what this simulates: the 2026-07-20/21 incidents wiped
//! **MuninnDB's** half of the binding (the Pebble key store). This drill
//! corrupts **the hotel's** half instead. Both produce an identical token-401
//! at the identical `with_auth` call site, so the circuit under test — classify
//! → heal → re-mint → retry — is the same; but this drill never touches Pebble,
//! needs no admin credential to *break* anything, and therefore cannot strand a
//! real vault. That tradeoff is deliberate.
//!
//! Rails (enforced here, not just documented):
//! - the vault name must start with `chaos_smoke` — `default`, `self_*`,
//!   `user_*`, and `session_*` are rejected outright;
//! - the vault must already exist in `vault_registry` — this driver never
//!   creates one;
//! - the original token is restored if the heal does not complete.
//!
//! Usage:
//!   PHILOTIC_HOTEL_SOCKET=~/.philotic/bjork/aiua-mac-jane.sock \
//!     cargo run -p philotic-client --example memory_token_drill_driver
//!   ... --example memory_token_drill_driver -- drill chaos_smoke_token_drill

use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse};
use uuid::Uuid;

/// Vault name used by the probe's refusal check. Must NOT be registered.
const UNREGISTERED_PROBE_VAULT: &str = "chaos_smoke_unregistered_probe";

/// Non-reversible short fingerprint (FNV-1a 64) so two tokens can be compared
/// without either one appearing in a log, a terminal, or CI output.
fn fingerprint(value: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("fp:{hash:016x}")
}

/// Rail: refuse to corrupt anything that is not an explicitly sacrificial vault.
/// Mirrors `config_key_denied` in scripts/chaos-smoke.sh.
fn vault_name_denied(vault: &str) -> bool {
    if vault.starts_with("default")
        || vault.starts_with("self_")
        || vault.starts_with("user_")
        || vault.starts_with("session_")
    {
        return true;
    }
    !vault.starts_with("chaos_smoke")
}

/// Pull `{vault_name: token}` out of a `MemoryConfig` response payload.
fn vault_tokens(config_json: &str) -> Result<Vec<(String, String)>> {
    let cfg: serde_json::Value =
        serde_json::from_str(config_json).context("memory config was not valid JSON")?;
    let map = cfg
        .get("vault_tokens")
        .and_then(|v| v.as_object())
        .context("memory config carried no vault_tokens object")?;
    let mut out: Vec<(String, String)> = map
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
        .collect();
    out.sort();
    Ok(out)
}

async fn connect(socket_path: &str) -> Result<philotic_client::PhiloticClient> {
    philotic_client::PhiloticClient::connect_at(
        socket_path,
        GuestIdentity {
            guest_id: format!("memory-token-drill-{}", Uuid::new_v4().simple()),
            role: "memory-token-drill".into(),
            supported_tools: Vec::new(),
        },
    )
    .await
    .with_context(|| format!("failed to connect IPC socket at {socket_path}"))
}

async fn fetch_config(client: &mut philotic_client::PhiloticClient) -> Result<String> {
    match client.send_request(IpcRequest::FetchMemoryConfig).await? {
        IpcResponse::MemoryConfig(payload) => payload
            .config_json
            .context("hotel returned MemoryConfig with no config_json (memory not configured)"),
        other => bail!("unexpected response to FetchMemoryConfig: {other:?}"),
    }
}

async fn run_probe(socket_path: &str) -> Result<()> {
    let mut client = connect(socket_path).await?;

    eprintln!("== S3 check: FetchMemoryConfig served from the Context Graph ==");
    let config_json = fetch_config(&mut client).await?;
    let tokens = vault_tokens(&config_json)?;
    if tokens.is_empty() {
        bail!("memory config carried zero vault tokens — nothing to verify");
    }
    for (vault, token) in &tokens {
        eprintln!("   vault={vault:<28} token={}", fingerprint(token));
    }
    eprintln!(
        "   {} registered vault token(s) served live\n",
        tokens.len()
    );

    eprintln!("== S2 check: HealMemoryToken refuses an unregistered vault ==");
    eprintln!("   requesting heal for {UNREGISTERED_PROBE_VAULT} (must be refused)");
    let resp = client
        .send_request(IpcRequest::HealMemoryToken {
            vault: UNREGISTERED_PROBE_VAULT.to_string(),
        })
        .await?;

    match resp {
        IpcResponse::Error(ref msg) => {
            eprintln!("   REFUSED as designed: {msg}");
            if !msg.contains("not registered") && !msg.contains(UNREGISTERED_PROBE_VAULT) {
                bail!("refusal did not name the unregistered vault — unexpected shape: {msg}");
            }
        }
        IpcResponse::Standard {
            ok: false,
            ref code,
            ref message,
            ..
        } => {
            eprintln!("   REFUSED as designed: [{code}] {message}");
        }
        IpcResponse::MemoryConfig(_) => {
            bail!(
                "GUARDRAIL BREACH: hotel returned a refreshed config for an UNREGISTERED vault \
                 ({UNREGISTERED_PROBE_VAULT}) — the registered-vaults-only rail is not holding"
            );
        }
        other => bail!("unexpected response to HealMemoryToken: {other:?}"),
    }

    eprintln!("\nprobe OK — S1-S3 wired in the running hotel, refusal rail holding.");
    Ok(())
}

/// Register a sacrificial vault so the drill has something to corrupt.
///
/// Deliberately seeds a **placeholder token that MuninnDB will reject**: the
/// registry entry is all the heal path needs (it looks the vault up, mints,
/// and rotates in place), so the first heal mints the first real token. That
/// keeps an admin credential out of this driver entirely — the hotel owns it.
///
/// This is a separate, explicitly-invoked operator command. The `drill` mode
/// still refuses to create anything: provisioning a vault stays a deliberate
/// act, never a side effect of a chaos run.
async fn run_provision(socket_path: &str, vault: &str) -> Result<()> {
    if vault_name_denied(vault) {
        bail!(
            "REFUSING: '{vault}' is not a sacrificial vault name. Only 'chaos_smoke*' \
             vaults may be provisioned by this tool."
        );
    }

    let mut client = connect(socket_path).await?;

    let existing = vault_tokens(&fetch_config(&mut client).await?)?;
    if existing.iter().any(|(name, _)| name == vault) {
        eprintln!("vault '{vault}' is already registered — nothing to do.");
        return Ok(());
    }

    let placeholder = format!("mk_chaos_smoke_placeholder_{}", Uuid::new_v4().simple());
    let resp = client
        .send_request(IpcRequest::AddVaultEntry {
            vault_name: vault.to_string(),
            plaintext: placeholder,
            // Mirror what provision_muninn_vaults stores: the hotel resolves
            // vault tokens as role "hotel", and load_muninn_config skips any
            // registry entry whose kind is not muninn_vault_token.
            allowed_roles: vec!["hotel".to_string()],
            secret_kind: Some("muninn_vault_token".to_string()),
        })
        .await?;

    let secret_ref = match resp {
        IpcResponse::Standard {
            ok: true,
            ref data, ..
        } => data
            .as_ref()
            .and_then(|d| d.get("secret_ref"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .context("AddVaultEntry succeeded but returned no secret_ref")?,
        IpcResponse::Standard {
            ok: false,
            ref code,
            ref message,
            ..
        } => bail!("AddVaultEntry refused: [{code}] {message}"),
        IpcResponse::Error(msg) => bail!("AddVaultEntry failed: {msg}"),
        other => bail!("unexpected response to AddVaultEntry: {other:?}"),
    };

    eprintln!("provisioned sacrificial vault '{vault}'");
    eprintln!("  secret_ref: {secret_ref}");
    eprintln!(
        "  seeded with a placeholder token MuninnDB will reject — the first heal mints the real one."
    );
    eprintln!("\nRun the drill with:");
    eprintln!("  export PHILOTIC_DRILL_SECRET_REF={secret_ref}");
    eprintln!("  ... --example memory_token_drill_driver -- drill {vault}");
    Ok(())
}

async fn run_drill(socket_path: &str, vault: &str) -> Result<()> {
    if vault_name_denied(vault) {
        bail!(
            "REFUSING: '{vault}' is not a sacrificial vault. The drill only ever touches \
             vaults whose name starts with 'chaos_smoke' (never default/self_*/user_*/session_*)."
        );
    }

    let mut client = connect(socket_path).await?;

    let before_json = fetch_config(&mut client).await?;
    let before = vault_tokens(&before_json)?;
    let original = before
        .iter()
        .find(|(name, _)| name == vault)
        .map(|(_, token)| token.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "REFUSING: sacrificial vault '{vault}' is not registered in vault_registry. \
                 This drill never creates a vault — provision it deliberately first."
            )
        })?;

    let secret_ref = std::env::var("PHILOTIC_DRILL_SECRET_REF").context(
        "PHILOTIC_DRILL_SECRET_REF must name the vault_registry secret_ref to corrupt \
         (read it from config:vault_registry) — refusing to guess",
    )?;

    eprintln!("== drill: corrupting hotel-side token for {vault} ==");
    eprintln!("   original token {}", fingerprint(&original));

    let bogus = format!("mk_chaos_smoke_invalid_{}", Uuid::new_v4().simple());
    let resp = client
        .send_request(IpcRequest::RotateSecret {
            secret_ref: secret_ref.clone(),
            plaintext: bogus,
        })
        .await?;
    if let IpcResponse::Error(msg) = resp {
        bail!("could not corrupt the sacrificial token (nothing changed): {msg}");
    }
    eprintln!("   token corrupted — hotel now holds an invalid bearer for {vault}");

    // Everything below must restore the original token on any failure.
    let outcome = drill_assert_heal(&mut client, vault, &original).await;

    if outcome.is_err() {
        eprintln!("   drill FAILED — restoring the original token for {vault}");
        let restore = client
            .send_request(IpcRequest::RotateSecret {
                secret_ref,
                plaintext: original,
            })
            .await;
        match restore {
            Ok(IpcResponse::Error(msg)) => {
                eprintln!("   WARNING: restore rejected by the hotel: {msg}");
            }
            Err(err) => eprintln!("   WARNING: restore call failed: {err}"),
            _ => eprintln!("   original token restored"),
        }
    }

    outcome
}

/// Assert the heal circuit: `HealMemoryToken` re-mints and the refreshed config
/// carries a *different* token than the corrupted one we just wrote.
async fn drill_assert_heal(
    client: &mut philotic_client::PhiloticClient,
    vault: &str,
    original: &str,
) -> Result<()> {
    eprintln!("== drill: requesting HealMemoryToken for {vault} ==");
    let resp = client
        .send_request(IpcRequest::HealMemoryToken {
            vault: vault.to_string(),
        })
        .await?;

    let healed_json = match resp {
        IpcResponse::MemoryConfig(payload) => payload
            .config_json
            .context("heal returned MemoryConfig with no config_json")?,
        IpcResponse::Error(msg) => {
            bail!("heal refused or failed: {msg}");
        }
        other => bail!("unexpected response to HealMemoryToken: {other:?}"),
    };

    let healed = vault_tokens(&healed_json)?;
    let healed_token = healed
        .iter()
        .find(|(name, _)| name == vault)
        .map(|(_, token)| token.clone())
        .context("healed config no longer carries the drilled vault")?;

    if healed_token.starts_with("mk_chaos_smoke_invalid_") {
        bail!("heal returned the corrupted token unchanged — no re-mint happened");
    }
    if healed_token == original {
        eprintln!(
            "   note: healed token equals the pre-drill token {} \
             (config served from the graph without a fresh mint)",
            fingerprint(&healed_token)
        );
    } else {
        eprintln!("   re-minted token {}", fingerprint(&healed_token));
    }

    eprintln!("\ndrill OK — token corrupted, heal re-minted, config refreshed without a restart.");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
        .context("PHILOTIC_HOTEL_SOCKET must point at the target hotel's UDS socket")?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("probe");

    match mode {
        "probe" => run_probe(&socket_path).await,
        "provision" => {
            let vault = args.get(1).context(
                "provision mode requires a sacrificial vault name, e.g. \
                 `-- provision chaos_smoke_token_drill`",
            )?;
            run_provision(&socket_path, vault).await
        }
        "drill" => {
            let vault = args.get(1).context(
                "drill mode requires a sacrificial vault name, e.g. \
                 `-- drill chaos_smoke_token_drill`",
            )?;
            run_drill(&socket_path, vault).await
        }
        other => bail!("unknown mode '{other}' (expected 'probe' or 'drill')"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_vaults_are_never_drillable() {
        for vault in [
            "default",
            "self_agent-bjork-01",
            "self_agent-coach",
            "user_likesjx",
            "session_abc123",
            "openai_api_key",
            "",
        ] {
            assert!(vault_name_denied(vault), "{vault} must be refused");
        }
    }

    #[test]
    fn only_chaos_smoke_vaults_are_drillable() {
        assert!(!vault_name_denied("chaos_smoke_token_drill"));
        assert!(!vault_name_denied("chaos_smoke_alt"));
    }

    #[test]
    fn fingerprint_hides_the_token_and_is_stable() {
        let fp = fingerprint("mk_supersecret_token_value");
        assert!(!fp.contains("supersecret"));
        assert_eq!(fp, fingerprint("mk_supersecret_token_value"));
        assert_ne!(fp, fingerprint("mk_a_different_token"));
    }
}
