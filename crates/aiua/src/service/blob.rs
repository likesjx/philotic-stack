use anyhow::Result;
use axum::{
    Router,
    extract::{DefaultBodyLimit, Multipart},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::post,
};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::io::AsyncWriteExt;
use tower_http::services::ServeDir;
use tracing::{error, info};

#[derive(Clone)]
pub struct BlobService {
    storage_dir: PathBuf,
}

impl BlobService {
    pub fn new(storage_dir: impl Into<PathBuf>) -> Self {
        let storage_dir = storage_dir.into();
        fs::create_dir_all(&storage_dir).unwrap_or_else(|e| {
            error!(
                "Failed to create blob storage directory {:?}: {}",
                storage_dir, e
            );
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

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
    {
        let _file_name = field.file_name().unwrap_or("unknown").to_string();
        let _content_type = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();

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
            // Already exists, just remove temp. Content-addressed, so a re-upload
            // is the same bytes being needed again: refresh its age so the
            // retention sweep does not delete a blob that is still in use.
            let _ = tokio::fs::remove_file(temp_path).await;
            touch_blob(&final_path);
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

/// Days a blob is kept before the retention sweep deletes it. Override with
/// PHILOTIC_BLOB_RETENTION_DAYS; 0 disables the sweep.
///
/// Nothing ever deleted a blob: voice notes, photos and documents from May were
/// still on every hotel, and since DEF-200 a cross-hotel attachment is stored on
/// two hotels. Seven days matches session-history retention — once the turn that
/// referenced a blob is gone, nothing can ask for it again.
pub const BLOB_RETENTION_DAYS: u64 = 7;

/// An interrupted upload leaves `temp_*` behind; no upload legitimately takes an hour.
const STALE_TEMP_AFTER: Duration = Duration::from_secs(60 * 60);

/// Mark a blob as freshly needed (refresh its modification time).
pub(crate) fn touch_blob(path: &Path) {
    if let Ok(file) = fs::OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

/// What one retention sweep removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PruneStats {
    pub blobs: usize,
    pub temps: usize,
    pub bytes: u64,
}

fn is_content_address(name: &str) -> bool {
    name.strip_prefix("sha256-").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Delete blobs older than `max_age` and abandoned `temp_*` uploads.
///
/// Deliberately narrow: only regular files named exactly like a content-addressed
/// blob (`sha256-<64 hex>`) or an upload temp (`temp_*`) are ever touched, so
/// pointing this at the wrong directory cannot delete anything else.
pub fn prune_blobs(dir: &Path, max_age: Duration, now: SystemTime) -> std::io::Result<PruneStats> {
    let mut stats = PruneStats::default();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let is_blob = is_content_address(name);
        let is_temp = name.starts_with("temp_");
        if !(is_blob || is_temp) {
            continue;
        }
        let meta = entry.metadata()?;
        if !meta.is_file() {
            continue;
        }
        let age = now
            .duration_since(meta.modified().unwrap_or(now))
            .unwrap_or_default();
        let limit = if is_temp { STALE_TEMP_AFTER } else { max_age };
        if age > limit && fs::remove_file(entry.path()).is_ok() {
            stats.bytes += meta.len();
            if is_temp {
                stats.temps += 1;
            } else {
                stats.blobs += 1;
            }
        }
    }
    Ok(stats)
}

/// Sweep at startup and every 6 hours thereafter.
pub fn spawn_retention(dir: PathBuf) {
    let days = std::env::var("PHILOTIC_BLOB_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(BLOB_RETENTION_DAYS);
    if days == 0 {
        tracing::warn!("Blob retention disabled (PHILOTIC_BLOB_RETENTION_DAYS=0)");
        return;
    }
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(6 * 60 * 60));
        loop {
            ticker.tick().await;
            let dir = dir.clone();
            let max_age = Duration::from_secs(days * 24 * 60 * 60);
            let swept =
                tokio::task::spawn_blocking(move || prune_blobs(&dir, max_age, SystemTime::now()))
                    .await;
            match swept {
                Ok(Ok(stats)) if stats != PruneStats::default() => info!(
                    deleted_blobs = stats.blobs,
                    deleted_temps = stats.temps,
                    freed_bytes = stats.bytes,
                    retention_days = days,
                    "Blob retention sweep"
                ),
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!("Blob retention sweep failed: {e}"),
                Err(e) => tracing::warn!("Blob retention task panicked: {e}"),
            }
        }
    });
}

#[cfg(test)]
mod retention_tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("blob-retention-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn id(n: u8) -> String {
        format!("sha256-{}", format!("{n:02x}").repeat(32))
    }

    fn write_aged(dir: &Path, name: &str, age: Duration) {
        let path = dir.join(name);
        fs::write(&path, b"x").unwrap();
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::now() - age).unwrap();
    }

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    #[test]
    fn old_blobs_and_stale_temps_roll_off_and_fresh_ones_stay() {
        let dir = scratch();
        write_aged(&dir, &id(1), 8 * DAY); // expired
        write_aged(&dir, &id(2), 6 * DAY); // still kept
        write_aged(&dir, "temp_abandoned", 2 * Duration::from_secs(3600)); // interrupted upload
        write_aged(&dir, "temp_in_flight", Duration::from_secs(30)); // upload underway

        let stats = prune_blobs(&dir, 7 * DAY, SystemTime::now()).unwrap();
        assert_eq!(
            stats,
            PruneStats {
                blobs: 1,
                temps: 1,
                bytes: 2
            }
        );
        assert!(!dir.join(id(1)).exists());
        assert!(dir.join(id(2)).exists());
        assert!(!dir.join("temp_abandoned").exists());
        assert!(
            dir.join("temp_in_flight").exists(),
            "never delete an upload in progress"
        );
    }

    #[test]
    fn nothing_but_blobs_and_upload_temps_is_ever_deleted() {
        let dir = scratch();
        write_aged(&dir, "notes.txt", 400 * DAY);
        write_aged(&dir, "sha256-short", 400 * DAY);
        write_aged(&dir, &format!("sha256-{}", "G".repeat(64)), 400 * DAY);
        fs::create_dir_all(dir.join("sha256-".to_string() + &"a".repeat(64))).unwrap();
        let stats = prune_blobs(&dir, DAY, SystemTime::now()).unwrap();
        assert_eq!(stats, PruneStats::default());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 4);
    }

    #[test]
    fn touching_a_blob_keeps_it_out_of_the_next_sweep() {
        let dir = scratch();
        write_aged(&dir, &id(3), 30 * DAY);
        touch_blob(&dir.join(id(3)));
        assert_eq!(
            prune_blobs(&dir, 7 * DAY, SystemTime::now()).unwrap(),
            PruneStats::default()
        );
        assert!(dir.join(id(3)).exists());
    }
}
