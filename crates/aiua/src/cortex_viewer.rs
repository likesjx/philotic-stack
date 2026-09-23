//! Read-only operator adapter. Credentials never leave the hotel and no URL,
//! HTTP method or arbitrary upstream path comes from the caller.
use ansible_mesh_core::domain::GraphDomain;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::Duration;

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c))
}

async fn get(client: &reqwest::Client, url: reqwest::Url) -> Result<Value> {
    let mut response = client.get(url).send().await?;
    if !response.status().is_success() {
        bail!("Cortex read unavailable ({})", response.status());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > 4 * 1024 * 1024 {
            bail!("Cortex response exceeds limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&body)?)
}

pub async fn read(
    graph: &GraphDomain,
    cortex_id: &str,
    vault: Option<String>,
    id: Option<String>,
    offset: u32,
) -> Result<Value> {
    if offset > 1_000_000
        || vault.as_ref().is_some_and(|v| !valid_name(v))
        || id
            .as_ref()
            .is_some_and(|v| v.len() != 26 || !v.bytes().all(|c| c.is_ascii_alphanumeric()))
        || (id.is_some() && vault.is_none())
    {
        bail!("Invalid Cortex query");
    }
    if graph.get_muninn_write_route()?.is_some() {
        bail!(
            "Connect to the Cortex hotel to browse canonical memory; observer fallback is disabled"
        );
    }
    let endpoint = graph
        .get_muninn_endpoint()?
        .context("Cortex is not configured")?;
    // Absence of a remote write route does not prove that this is the Cortex.
    // Rollout must explicitly attest the inspected canonical endpoint. The
    // value must be changed/revoked when moving Cortex to another host.
    let attested = graph
        .get_config_value("cortex_viewer_endpoint")?
        .and_then(|raw| serde_json::from_str::<String>(&raw).ok());
    if attested.as_deref() != Some(endpoint.as_str()) {
        bail!("Canonical Cortex endpoint has not been approved for browsing");
    }
    let base = reqwest::Url::parse(&endpoint)?;
    if !matches!(base.scheme(), "http" | "https")
        || !base.username().is_empty()
        || base.password().is_some()
    {
        bail!("Invalid configured Cortex endpoint");
    }
    let credential = crate::muninn_provision::resolve_admin_credential(graph)?
        .context("Cortex operator adapter is not configured")?;
    let client = reqwest::Client::builder()
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()?;
    let mut login = base.join("/api/auth/login")?;
    login
        .set_port(Some(
            base.port_or_known_default()
                .and_then(|p| p.checked_add(1))
                .context("Invalid Cortex port")?,
        ))
        .map_err(|_| anyhow::anyhow!("Invalid Cortex login port"))?;
    let response = client
        .post(login)
        .json(&json!({"username":credential.username,"password":credential.password}))
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("Cortex authentication unavailable");
    }
    let names = get(&client, base.join("/api/vaults")?).await?;
    let names: Vec<String> = serde_json::from_value(names).context("Invalid Cortex inventory")?;
    if names.len() > 512 {
        bail!("Cortex vault inventory exceeds limit");
    }
    if names.iter().any(|name| !valid_name(name))
        || names.iter().collect::<std::collections::HashSet<_>>().len() != names.len()
    {
        bail!("Invalid Cortex vault inventory");
    }
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(vault) = vault {
        if !names.contains(&vault) {
            bail!("Vault not in Cortex inventory");
        }
        let path = id
            .as_ref()
            .map(|id| format!("/api/engrams/{id}"))
            .unwrap_or_else(|| "/api/engrams".into());
        let mut url = base.join(&path)?;
        url.query_pairs_mut()
            .append_pair("vault", &vault)
            .append_pair("limit", "50")
            .append_pair("offset", &offset.to_string())
            .append_pair("sort", "created");
        let data = get(&client, url).await?;
        if let Some(id) = id {
            return Ok(memory(&vault, &id, &data));
        }
        let rows = data["engrams"].as_array().context("Invalid Cortex page")?;
        if rows.len() > 50
            || rows
                .iter()
                .any(|row| row["id"].as_str().is_none_or(|id| id.is_empty()))
        {
            bail!("Invalid Cortex page identity or size");
        }
        let total = data["total"]
            .as_u64()
            .context("Missing Cortex page total")?;
        let next = offset as u64 + rows.len() as u64;
        let items: Vec<Value> = rows
            .iter()
            .map(|row| memory(&vault, row["id"].as_str().unwrap_or(""), row))
            .collect();
        return Ok(
            json!({"cortex_id":cortex_id,"vault_id":vault,"observed_at":now,"memories":items,
            "next_cursor": if next < total && !rows.is_empty() { Some(next.to_string()) } else { None }}),
        );
    }
    let mut vaults = Vec::new();
    for name in names {
        if !valid_name(&name) {
            bail!("Invalid Cortex vault identity");
        }
        let mut url = base.join("/api/engrams")?;
        url.query_pairs_mut()
            .append_pair("vault", &name)
            .append_pair("limit", "1");
        let count = get(&client, url)
            .await
            .ok()
            .and_then(|v| v["total"].as_u64());
        vaults.push(json!({"id":name,"status":if count.is_some(){"available"}else{"unavailable"},
            "memory_count":count,"reason":if count.is_none(){Some("Cortex count unavailable")}else{None}}));
    }
    Ok(
        json!({"cortex_id":cortex_id,"observed_at":now,"catalog_complete":true,"vaults":vaults,
        "exclusions":["Local session scratch and non-Muninn stores are outside this inventory. Replication completeness is not verified."]}),
    )
}

fn memory(vault: &str, id: &str, row: &Value) -> Value {
    json!({"vault_id":vault,"memory_id":id,"concept":row["concept"].as_str().unwrap_or("Untitled memory"),
        "content":row["content"].as_str().unwrap_or(""),"state":state_name(&row["state"]),
        "tags":row["tags"].as_array().cloned().unwrap_or_default(),"source":row["source_type"].as_str(),
        "created_at":row.get("created_at").filter(|v|!v.is_null()).map(|v|v.as_str().map(str::to_owned).unwrap_or_else(||v.to_string())),
        "updated_at":row.get("updated_at").filter(|v|!v.is_null()).map(|v|v.as_str().map(str::to_owned).unwrap_or_else(||v.to_string()))})
}

fn state_name(value: &Value) -> &str {
    if let Some(value) = value.as_str() {
        return value;
    }
    match value.as_u64() {
        Some(0) => "planning",
        Some(1) => "active",
        Some(2) => "paused",
        Some(3) => "blocked",
        Some(4) => "completed",
        Some(5) => "cancelled",
        Some(6) => "archived",
        Some(127) => "soft-deleted",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_path_and_query_injection() {
        for value in ["", "../default", "default?x=1", "a/b", "with space"] {
            assert!(!valid_name(value));
        }
        assert!(valid_name("self_agent-beacon"));
    }
    #[test]
    fn missing_metadata_is_not_fabricated() {
        let row = memory("default", "test", &json!({"concept":"Test"}));
        assert_eq!(row["state"], "unknown");
        assert!(row["source"].is_null());
        assert_eq!(row["vault_id"], "default");
    }
}
