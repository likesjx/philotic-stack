//! Database catalog — the registry of KNOWN databases the table runner may touch.
//!
//! Two sources of truth:
//! 1. **Catalog file** (`{base_dir}/db_catalog.json`, override with
//!    `PHILOTIC_TABLE_CATALOG`): operator-registered databases, including
//!    external files (e.g. the macOS iMessage store). Entries carry an enforced
//!    `mode` (`ro`/`rw`), optional per-agent grants, and optional
//!    snapshot-on-read for live-written WAL databases.
//! 2. **Profile databases**: the legacy implicit `{base_dir}/{name}.db` mapping.
//!    Names are now restricted to `[A-Za-z0-9_-]` — the old unsanitized join
//!    allowed `db: "../../…"` path traversal out of the profile directory.
//!
//! The catalog file is re-read whenever its mtime changes, so grants and new
//! databases are editable at runtime with no deploy.

use anyhow::{Result, bail};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbMode {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Read,
    Write,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CatalogEntry {
    pub name: String,
    pub path: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub description: String,
    /// Copy the database via the SQLite backup API before reading and query the
    /// copy. For databases actively written by another process (iMessage's
    /// chat.db is WAL-live) this avoids lock contention and torn reads.
    #[serde(default)]
    pub snapshot_on_read: bool,
    /// How long a snapshot stays fresh before it is re-copied (default 300s).
    #[serde(default = "default_snapshot_ttl")]
    pub snapshot_ttl_secs: u64,
    /// Per-agent grants: agent_id -> verbs ("read" / "write").
    /// When present, only listed agents may touch this database.
    /// When absent, the catalog entry itself is the grant (any agent, mode-bound).
    #[serde(default)]
    pub agents: Option<HashMap<String, Vec<String>>>,
}

fn default_mode() -> String {
    "ro".to_string()
}

fn default_snapshot_ttl() -> u64 {
    300
}

impl CatalogEntry {
    pub fn db_mode(&self) -> Result<DbMode> {
        match self.mode.as_str() {
            "ro" => Ok(DbMode::ReadOnly),
            "rw" => Ok(DbMode::ReadWrite),
            other => bail!(
                "catalog entry '{}': invalid mode {other:?} (ro|rw)",
                self.name
            ),
        }
    }

    /// Resolve `~/` and relative paths.
    pub fn resolved_path(&self, base_dir: &Path) -> PathBuf {
        let p = &self.path;
        if let Some(rest) = p.strip_prefix("~/") {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            return PathBuf::from(home).join(rest);
        }
        let path = PathBuf::from(p);
        if path.is_absolute() {
            path
        } else {
            base_dir.join(path)
        }
    }

    pub fn allows(&self, agent_id: &str, verb: Verb) -> bool {
        match &self.agents {
            None => true,
            Some(map) => match map.get(agent_id) {
                None => false,
                Some(verbs) => match verb {
                    // "write" implies "read".
                    Verb::Read => verbs.iter().any(|v| v == "read" || v == "write"),
                    Verb::Write => verbs.iter().any(|v| v == "write"),
                },
            },
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct CatalogFile {
    #[serde(default)]
    databases: Vec<CatalogEntry>,
}

/// Loaded catalog plus the file identity used for change detection.
pub struct Catalog {
    path: PathBuf,
    mtime: Option<SystemTime>,
    entries: HashMap<String, CatalogEntry>,
}

impl Catalog {
    pub fn load(catalog_path: PathBuf) -> Self {
        let mut catalog = Self {
            path: catalog_path,
            mtime: None,
            entries: HashMap::new(),
        };
        catalog.reload();
        catalog
    }

    pub fn default_path(base_dir: &Path) -> PathBuf {
        if let Ok(p) = std::env::var("PHILOTIC_TABLE_CATALOG") {
            return PathBuf::from(p);
        }
        base_dir.join("db_catalog.json")
    }

    fn reload(&mut self) {
        self.mtime = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();
        self.entries.clear();
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return;
        };
        match serde_json::from_str::<CatalogFile>(&raw) {
            Ok(file) => {
                for entry in file.databases {
                    self.entries.insert(entry.name.clone(), entry);
                }
            }
            Err(err) => {
                tracing::warn!(path = %self.path.display(), %err, "db_catalog.json unreadable — catalog empty");
            }
        }
    }

    /// Re-read the catalog file when it changed on disk. Returns true when a
    /// reload happened (callers drop pooled connections so mode/path changes
    /// take effect).
    pub fn refresh_if_changed(&mut self) -> bool {
        let current = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();
        if current != self.mtime {
            self.reload();
            return true;
        }
        false
    }

    pub fn get(&self, name: &str) -> Option<&CatalogEntry> {
        self.entries.get(name)
    }

    pub fn entries(&self) -> impl Iterator<Item = &CatalogEntry> {
        self.entries.values()
    }
}

/// Legacy profile database names: strictly `[A-Za-z0-9_-]+`. Anything else —
/// separators, dots, empty — is rejected, which closes the path-traversal hole
/// (`db: "../../Library/Messages/chat"` used to escape the profile directory).
pub fn validate_profile_db_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!(
            "unknown database {name:?}: not in the catalog and not a valid profile database name"
        );
    }
    Ok(())
}

/// Which verb a table task kind needs.
pub fn verb_for_kind(kind: &str) -> Result<Verb> {
    match kind {
        "table.query" | "table.build" | "table.stats" | "table.schema" | "table.list"
        | "table.catalog" => Ok(Verb::Read),
        "table.insert" | "table.upsert" | "table.update" | "table.delete" | "table.exec"
        | "table.configure" | "table.rolloff" => Ok(Verb::Write),
        other => bail!("unsupported table task kind: {other}"),
    }
}

/// Extract the calling agent from the task identity envelope.
pub fn agent_from_identity(identity: &serde_json::Value) -> String {
    identity
        .get("agent_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| identity.get("agent").and_then(serde_json::Value::as_str))
        .unwrap_or("anonymous")
        .to_string()
}
