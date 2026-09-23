//! Attachments that cross a hotel boundary (DEF-200).
//!
//! A Telegram seat stores a voice note / photo in ITS hotel's blob store and
//! tells the agent where to fetch it: `blob_download_url` =
//! `http://127.0.0.1:<that hotel's blob port>/download/<blob_id>`. That URL is
//! only meaningful on the hotel that wrote it. Since a transport home is
//! independent of its agent's hotel (DEF-178), the seat's hotel and the agent's
//! hotel can differ — and then the agent's model-router dialled ITS OWN
//! loopback, found nothing, and the turn died `MODEL_EMPTY_RESPONSE: error
//! sending request for url (http://127.0.0.1:16371/download/sha256-…)`. Text
//! crossed the mesh fine; every voice note from a seat on another hotel was lost
//! (mac-jane seat -> Beacon on vps-jane, 2026-09-22/23).
//!
//! The fix rides the task itself. On egress to a peer the sending hotel embeds
//! each small local attachment as `blob_inline_b64`; on ingress the receiving
//! hotel verifies it against the content address, files it in ITS blob store
//! under the same id, and rewrites `blob_download_url` to its own loopback. The
//! task then reads exactly as if the seat had been local. Blobs are
//! content-addressed (`sha256-<hex>`), so the id doubles as the integrity check:
//! a peer cannot make this hotel store bytes under an id they do not hash to,
//! and the id is validated before it is ever used as a file name.
//!
//! Hotels that predate this ignore the extra field, so a mixed-version mesh is
//! unaffected (the attachment just stays as broken as it was).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::{info, warn};

/// Largest attachment carried inline with a task. A voice note is tens to a few
/// hundred KB; the task rides the store-and-forward ledger (one SQLite row) and
/// a single execution-plane frame, so this stays well under both limits.
pub(crate) const MAX_INLINE_BLOB_BYTES: usize = 4 * 1024 * 1024;

/// Field carrying the attachment bytes between hotels.
const INLINE_FIELD: &str = "blob_inline_b64";

struct LocalBlobStore {
    dir: PathBuf,
    port: u16,
}

static STORE: OnceLock<LocalBlobStore> = OnceLock::new();

/// Tell this module where the hotel's blob store lives. Called once when the
/// blob server starts; until then egress/ingress leave attachments untouched.
pub(crate) fn register_local_store(dir: impl Into<PathBuf>, port: u16) {
    let _ = STORE.set(LocalBlobStore {
        dir: dir.into(),
        port,
    });
}

/// `sha256-` + 64 lowercase hex. The only shape ever used as a file name here.
pub(crate) fn is_blob_id(id: &str) -> bool {
    id.strip_prefix("sha256-").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Apply `f` to every JSON object anywhere in `value` that names a well-formed
/// `blob_id`; true when `f` changed anything.
fn walk_attachments(
    value: &mut Value,
    f: &mut impl FnMut(&mut serde_json::Map<String, Value>) -> bool,
) -> bool {
    match value {
        Value::Object(map) => {
            let mut changed = false;
            if map
                .get("blob_id")
                .and_then(Value::as_str)
                .is_some_and(is_blob_id)
            {
                changed |= f(map);
            }
            for child in map.values_mut() {
                changed |= walk_attachments(child, f);
            }
            changed
        }
        Value::Array(items) => items
            .iter_mut()
            .fold(false, |changed, item| walk_attachments(item, f) | changed),
        _ => false,
    }
}

/// Rewrite each attachment in `task_json`; `None` when the text is not JSON or
/// nothing changed.
fn rewrite_attachments(
    task_json: &str,
    mut f: impl FnMut(&mut serde_json::Map<String, Value>) -> bool,
) -> Option<String> {
    let mut root: Value = serde_json::from_str(task_json).ok()?;
    walk_attachments(&mut root, &mut f)
        .then(|| serde_json::to_string(&root).unwrap_or_else(|_| task_json.to_string()))
}

fn read_blob(dir: &Path, blob_id: &str) -> Option<Vec<u8>> {
    let path = dir.join(blob_id);
    let len = std::fs::metadata(&path).ok()?.len();
    if len as usize > MAX_INLINE_BLOB_BYTES {
        return None;
    }
    std::fs::read(path).ok()
}

/// Egress: embed this hotel's small local attachments so the receiving hotel
/// does not need to reach back into this one. Returns `task_json` unchanged when
/// there is nothing to embed.
pub(crate) async fn embed_local_blobs(task_json: &str) -> String {
    let Some(store) = STORE.get() else {
        return task_json.to_string();
    };
    // Cheap pre-check: most tasks carry no attachment at all.
    if !task_json.contains("\"blob_id\"") {
        return task_json.to_string();
    }
    let dir = store.dir.clone();
    let text = task_json.to_string();
    let embedded = tokio::task::spawn_blocking(move || {
        rewrite_attachments(&text, |att| {
            if att.contains_key(INLINE_FIELD) {
                return false;
            }
            let Some(id) = att
                .get("blob_id")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                return false;
            };
            match read_blob(&dir, &id) {
                Some(bytes) => {
                    att.insert(INLINE_FIELD.into(), Value::String(STANDARD.encode(bytes)));
                    true
                }
                None => false,
            }
        })
    })
    .await
    .ok()
    .flatten();
    embedded.unwrap_or_else(|| task_json.to_string())
}

/// Why an inbound inline attachment was not filed.
#[derive(Debug, PartialEq, Eq)]
enum Rejected {
    NotBase64,
    TooLarge,
    HashMismatch,
    Io(String),
}

fn file_inline_blob(dir: &Path, blob_id: &str, b64: &str) -> Result<(), Rejected> {
    if b64.len() > MAX_INLINE_BLOB_BYTES.div_ceil(3) * 4 + 4 {
        return Err(Rejected::TooLarge);
    }
    let bytes = STANDARD.decode(b64).map_err(|_| Rejected::NotBase64)?;
    if bytes.len() > MAX_INLINE_BLOB_BYTES {
        return Err(Rejected::TooLarge);
    }
    let actual = format!("sha256-{}", hex::encode(Sha256::digest(&bytes)));
    if actual != blob_id {
        return Err(Rejected::HashMismatch);
    }
    let final_path = dir.join(blob_id);
    if final_path.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(|e| Rejected::Io(e.to_string()))?;
    let temp = dir.join(format!("temp_{}", uuid::Uuid::new_v4()));
    std::fs::write(&temp, &bytes).map_err(|e| Rejected::Io(e.to_string()))?;
    std::fs::rename(&temp, &final_path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        Rejected::Io(e.to_string())
    })
}

/// Ingress: file each embedded attachment in this hotel's blob store and point
/// `blob_download_url` at it. Returns `task_json` unchanged when nothing was
/// embedded.
pub(crate) async fn materialize_inline_blobs(task_json: &str) -> String {
    if !task_json.contains(INLINE_FIELD) {
        return task_json.to_string();
    }
    let Some(store) = STORE.get() else {
        return task_json.to_string();
    };
    let (dir, port) = (store.dir.clone(), store.port);
    let text = task_json.to_string();
    let filed = tokio::task::spawn_blocking(move || {
        rewrite_attachments(&text, |att| {
            let Some(Value::String(b64)) = att.remove(INLINE_FIELD) else {
                return false;
            };
            let id = att
                .get("blob_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            match file_inline_blob(&dir, &id, &b64) {
                Ok(()) => {
                    att.insert(
                        "blob_download_url".into(),
                        Value::String(format!("http://127.0.0.1:{port}/download/{id}")),
                    );
                    info!(blob_id = %id, "Filed an attachment that arrived inline from a peer hotel");
                }
                Err(reason) => {
                    warn!(blob_id = %id, ?reason, "Refused an inline attachment from a peer hotel");
                    att.insert(
                        "transport_error".into(),
                        Value::String(format!("inline attachment refused: {reason:?}")),
                    );
                }
            }
            true
        })
    })
    .await
    .ok()
    .flatten();
    filed.unwrap_or_else(|| task_json.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("blob-transfer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn blob_id_of(bytes: &[u8]) -> String {
        format!("sha256-{}", hex::encode(Sha256::digest(bytes)))
    }

    #[test]
    fn only_a_well_formed_content_address_is_a_blob_id() {
        assert!(is_blob_id(&blob_id_of(b"x")));
        for bad in [
            "",
            "sha256-",
            "sha256-abc",
            "../../etc/passwd",
            &format!("sha256-{}", "A".repeat(64)),
            &format!("sha256-{}/../x", "a".repeat(58)),
            &format!("md5-{}", "a".repeat(64)),
        ] {
            assert!(!is_blob_id(bad), "{bad:?} must not be accepted");
        }
    }

    /// The whole point: bytes written to hotel A's store arrive readable from
    /// hotel B's, and the task text points at B's own loopback.
    #[test]
    fn an_attachment_survives_a_hotel_hop_and_lands_on_the_local_loopback() {
        let (a, b) = (scratch(), scratch());
        let voice = b"OggS-not-really-but-bytes".to_vec();
        let id = blob_id_of(&voice);
        std::fs::write(a.join(&id), &voice).unwrap();

        let task = serde_json::json!({
            "text": "hi",
            "attachments": [{"kind": "voice", "blob_id": id,
                "blob_download_url": format!("http://127.0.0.1:16371/download/{id}")}]
        })
        .to_string();

        // A: embed.
        let sent = rewrite_attachments(&task, |att| {
            let bytes = read_blob(&a, att["blob_id"].as_str().unwrap()).unwrap();
            att.insert(INLINE_FIELD.into(), Value::String(STANDARD.encode(bytes)));
            true
        })
        .unwrap();
        assert!(sent.contains(INLINE_FIELD));

        // B: file.
        let mut v: Value = serde_json::from_str(&sent).unwrap();
        let att = &mut v["attachments"][0];
        let b64 = att[INLINE_FIELD].as_str().unwrap().to_string();
        file_inline_blob(&b, &id, &b64).unwrap();
        assert_eq!(std::fs::read(b.join(&id)).unwrap(), voice);
    }

    #[test]
    fn bytes_that_do_not_hash_to_the_claimed_id_are_never_stored() {
        let dir = scratch();
        let claimed = blob_id_of(b"what the peer says it is");
        let b64 = STANDARD.encode(b"something else entirely");
        assert_eq!(
            file_inline_blob(&dir, &claimed, &b64),
            Err(Rejected::HashMismatch)
        );
        assert!(!dir.join(&claimed).exists());
        assert_eq!(
            file_inline_blob(&dir, &claimed, "!!not base64!!"),
            Err(Rejected::NotBase64)
        );
        let huge = STANDARD.encode(vec![0u8; MAX_INLINE_BLOB_BYTES + 1]);
        assert_eq!(
            file_inline_blob(&dir, &claimed, &huge),
            Err(Rejected::TooLarge)
        );
    }

    #[test]
    fn nested_and_multiple_attachments_are_all_found() {
        let (x, y) = (blob_id_of(b"x"), blob_id_of(b"y"));
        let task = serde_json::json!({
            "attachments": [{"blob_id": x}],
            "message": {"attachments": [{"blob_id": y}, {"blob_id": "../bad"}]}
        })
        .to_string();
        let mut seen = Vec::new();
        rewrite_attachments(&task, |att| {
            seen.push(att["blob_id"].as_str().unwrap().to_string());
            false
        });
        seen.sort();
        let mut want = vec![x, y];
        want.sort();
        assert_eq!(seen, want, "the malformed id is not an attachment");
    }

    /// The real entry points, end to end. The store is process-global, so this
    /// is the only test that registers one; the "peer" is simulated by deleting
    /// the local copy between embed and materialize.
    #[tokio::test]
    async fn embed_then_materialize_restores_the_blob_and_rewrites_the_url() {
        let dir = scratch();
        register_local_store(dir.clone(), 16467);
        let voice = b"a voice note".to_vec();
        let id = blob_id_of(&voice);
        std::fs::write(dir.join(&id), &voice).unwrap();
        let task = serde_json::json!({"attachments": [{"kind": "voice", "blob_id": id,
            "blob_download_url": format!("http://127.0.0.1:16371/download/{id}")}]})
        .to_string();

        let wire = embed_local_blobs(&task).await;
        assert!(
            wire.contains(INLINE_FIELD),
            "small local blob must ride the task"
        );

        std::fs::remove_file(dir.join(&id)).unwrap(); // the receiving hotel has none
        let received = materialize_inline_blobs(&wire).await;
        let v: Value = serde_json::from_str(&received).unwrap();
        let att = &v["attachments"][0];
        assert_eq!(
            att["blob_download_url"],
            format!("http://127.0.0.1:16467/download/{id}"),
            "the agent must fetch from ITS OWN hotel's loopback"
        );
        assert!(
            att.get(INLINE_FIELD).is_none(),
            "bytes are not left in the task"
        );
        assert_eq!(std::fs::read(dir.join(&id)).unwrap(), voice);

        // A tampered attachment is refused, not filed.
        let forged = wire.replace(&STANDARD.encode(&voice), &STANDARD.encode(b"forged bytes"));
        let refused = materialize_inline_blobs(&forged).await;
        assert!(refused.contains("inline attachment refused"), "{refused}");
    }

    #[test]
    fn non_json_and_attachment_free_text_pass_through() {
        assert_eq!(rewrite_attachments("not json", |_| true), None);
        assert_eq!(rewrite_attachments(r#"{"text":"hi"}"#, |_| true), None);
    }
}
