use axum::{
    extract::{DefaultBodyLimit, Multipart},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::post,
    Router,
};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{PathBuf};
use tokio::io::AsyncWriteExt;
use tower_http::services::ServeDir;
use tracing::{error, info};
use anyhow::Result;

#[derive(Clone)]
pub struct BlobService {
    storage_dir: PathBuf,
}

impl BlobService {
    pub fn new(storage_dir: impl Into<PathBuf>) -> Self {
        let storage_dir = storage_dir.into();
        fs::create_dir_all(&storage_dir).unwrap_or_else(|e| {
            error!("Failed to create blob storage directory {:?}: {}", storage_dir, e);
        });
        Self { storage_dir }
    }

    pub fn router(&self) -> Router {
        // We serve the blobs directory statically for GET requests (downloads by remote nodes)
        // And we provide a POST endpoint for local guests to upload new blobs
        Router::new()
            .route("/upload", post(upload_blob))
            .nest_service("/download", ServeDir::new(&self.storage_dir))
            .with_state(self.clone())
            .layer(DefaultBodyLimit::max(1024 * 1024 * 100)) // 100MB limit for blob uploads
    }

    /// Spin up the Blob HTTP server on the given address
    pub async fn serve(self, addr: &str) -> Result<()> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        info!("Blob HTTP Server listening on {}", listener.local_addr()?);
        axum::serve(listener, self.router()).await?;
        Ok(())
    }
}

pub async fn upload_blob(
    axum::extract::State(state): axum::extract::State<BlobService>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, StatusCode> {
    let mut uploaded_blobs = Vec::new();
    let storage_dir = state.storage_dir.clone();
    
    while let Some(mut field) = multipart.next_field().await.map_err(|_| StatusCode::BAD_REQUEST)? {
        let _file_name = field.file_name().unwrap_or("unknown").to_string();
        let _content_type = field.content_type().unwrap_or("application/octet-stream").to_string();
        
        let mut hasher = Sha256::new();
        let temp_path = storage_dir.join(format!("temp_{}", uuid::Uuid::new_v4()));
        
        let mut file = tokio::fs::File::create(&temp_path)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        while let Some(chunk) = field.chunk().await.map_err(|_| StatusCode::BAD_REQUEST)? {
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }

        let hash_result = hasher.finalize();
        let blob_id = format!("sha256-{}", hex::encode(hash_result));
        
        let final_path = storage_dir.join(&blob_id);
        
        if final_path.exists() {
            // Already exists, just remove temp
            let _ = tokio::fs::remove_file(temp_path).await;
        } else {
            tokio::fs::rename(temp_path, final_path)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }

        uploaded_blobs.push(blob_id);
    }

    if uploaded_blobs.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    Ok(Json(serde_json::json!({
        "blob_ids": uploaded_blobs,
    })))
}
