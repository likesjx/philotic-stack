//! Shared MuninnDB admin/maintenance HTTP client.
//!
//! The canonical client for read/maintenance planes that talk to MuninnDB's
//! REST API outside the `MemoryEngine` recall/write path: nightly sweeps
//! (dream, hygiene), digests, and inspectors. Before this existed, each of
//! those modules built its own `reqwest::Client` and re-implemented base-url
//! trimming, bearer auth, timeouts, and JSON decoding — ~8 copies with
//! drifting conventions (2026-09-30 memory-RAG audit, gap 9).
//!
//! Not for: the engine's own recall/write path (`rest_client.rs`, which owns
//! caching, vault skip registries, and token-heal semantics) or the Cortex
//! operator viewer (cookie-session admin login, deliberately separate).

use std::time::Duration;

use anyhow::Result;
use serde::de::DeserializeOwned;

const ADMIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Bearer-token JSON client over one MuninnDB base URL.
#[derive(Debug, Clone)]
pub struct AdminClient {
    client: reqwest::Client,
    base_url: String,
}

impl AdminClient {
    pub fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder().timeout(ADMIN_TIMEOUT).build()?,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn url(&self, path_and_query: &str) -> String {
        format!("{}{}", self.base_url, path_and_query)
    }

    /// Append `vault=<vault>` to a path that may already carry a query.
    pub fn with_vault(path_and_query: &str, vault: &str) -> String {
        let sep = if path_and_query.contains('?') {
            '&'
        } else {
            '?'
        };
        format!("{path_and_query}{sep}vault={vault}")
    }

    /// GET and decode; bails with the HTTP status on a non-success answer.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        token: &str,
        path_and_query: &str,
    ) -> Result<T> {
        let resp = self
            .client
            .get(self.url(path_and_query))
            .bearer_auth(token)
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("GET {} returned {}", path_and_query, resp.status());
        }
        Ok(resp.json().await?)
    }

    /// GET as an untyped value; `None` when the endpoint is unavailable or
    /// non-success (for report-only counters that must never fail a sweep).
    pub async fn try_get_value(
        &self,
        token: &str,
        path_and_query: &str,
    ) -> Option<serde_json::Value> {
        let resp = self
            .client
            .get(self.url(path_and_query))
            .bearer_auth(token)
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json().await.ok()
    }

    /// POST a JSON body; returns the status so callers can distinguish
    /// "refused" (non-success answer) from transport failure (`Err`).
    pub async fn post_json(
        &self,
        token: &str,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<reqwest::StatusCode> {
        let resp = self
            .client
            .post(self.url(path))
            .bearer_auth(token)
            .json(body)
            .send()
            .await?;
        Ok(resp.status())
    }

    /// DELETE; returns the status (see `post_json` on refused vs failed).
    pub async fn delete(&self, token: &str, path_and_query: &str) -> Result<reqwest::StatusCode> {
        let resp = self
            .client
            .delete(self.url(path_and_query))
            .bearer_auth(token)
            .send()
            .await?;
        Ok(resp.status())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_is_trimmed_and_vault_joins_queries() {
        let c = AdminClient::new("http://localhost:8750/").unwrap();
        assert_eq!(c.base_url(), "http://localhost:8750");
        assert_eq!(
            AdminClient::with_vault("/api/contradictions", "default"),
            "/api/contradictions?vault=default"
        );
        assert_eq!(
            AdminClient::with_vault("/api/deleted?limit=100", "default"),
            "/api/deleted?limit=100&vault=default"
        );
    }
}
