//! HTTP client for the ONNX embeddings sidecar.
//!
//! Communicates with `onnx-runner` sidecar at `127.0.0.1:11435`
//! to generate embeddings for text using local ONNX models.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Default sidecar address for ONNX embeddings.
const DEFAULT_SIDECAR_URL: &str = "http://127.0.0.1:11435";

/// Request body for the `/api/embeddings` endpoint.
#[derive(Debug, Serialize)]
pub struct EmbedRequest {
    pub model: Option<String>,
    pub prompt: String,
}

/// Response body from the `/api/embeddings` endpoint.
#[derive(Debug, Deserialize)]
pub struct EmbedResponse {
    pub embedding: Vec<f32>,
    /// Model generation provenance token.
    /// The sidecar returns this as "model" for Ollama API compatibility.
    #[serde(rename = "model")]
    pub model_gen: String,
}

/// Client for the ONNX embeddings sidecar.
#[derive(Debug, Clone)]
pub struct EmbeddingsClient {
    http: reqwest::Client,
    base_url: String,
}

impl EmbeddingsClient {
    /// Create a new client with the default sidecar URL.
    pub fn new() -> Self {
        Self::with_url(DEFAULT_SIDECAR_URL)
    }

    /// Create a client with a custom base URL.
    pub fn with_url(base_url: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Check if the sidecar is healthy and reachable.
    pub async fn health_check(&self) -> Result<bool> {
        let url = format!("{}/api/health", self.base_url);
        match self.http.get(&url).timeout(std::time::Duration::from_secs(2)).send().await {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(_) => Ok(false),
        }
    }

    /// Generate an embedding vector for the given text.
    ///
    /// Returns the embedding vector and the model generation provenance.
    pub async fn embed(&self, text: &str) -> Result<EmbedResponse> {
        let url = format!("{}/api/embeddings", self.base_url);

        let req = EmbedRequest {
            model: None, // Use default model
            prompt: text.to_string(),
        };

        let resp = self
            .http
            .post(&url)
            .json(&req)
            .send()
            .await
            .context("failed to send embedding request to sidecar")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("sidecar returned error {}: {}", status, body);
        }

        let embed_resp: EmbedResponse = resp
            .json()
            .await
            .context("failed to parse embedding response")?;

        Ok(embed_resp)
    }

    /// Batch embed multiple texts efficiently.
    ///
    /// Note: This makes sequential requests. For true batching, the sidecar
    /// would need to support batch inputs.
    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<EmbedResponse>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }
}

impl Default for EmbeddingsClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_construction() {
        let client = EmbeddingsClient::new();
        assert_eq!(client.base_url, "http://127.0.0.1:11435");

        let custom = EmbeddingsClient::with_url("http://localhost:9999");
        assert_eq!(custom.base_url, "http://localhost:9999");
    }

    #[test]
    fn test_url_trimming() {
        let client = EmbeddingsClient::with_url("http://example.com/");
        assert_eq!(client.base_url, "http://example.com");
    }
}
