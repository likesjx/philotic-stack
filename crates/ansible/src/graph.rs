use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;
use tracing::{debug, info};
use ansible_mesh_core::NodeCapabilities;

/// The concrete, disk-backed Context Graph database for the Ansible daemon.
/// Holds the universal materialization configuration and agent memory.
pub struct ContextGraph {
    pub conn: Connection,
}

impl ContextGraph {
    /// Opens or creates the SQLite Context Graph database at the given path.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let conn = Connection::open(path).context("Failed to open ContextGraph SQLite DB")?;
        
        let graph = Self { conn };
        graph.init_schema()?;
        
        Ok(graph)
    }

    /// Initializes the database schema if it doesn't exist.
    fn init_schema(&self) -> Result<()> {
        debug!("Initializing Context Graph schema");
        self.conn.execute_batch(
            "
            BEGIN;
            -- Holds the universal capability configuration for this node
            CREATE TABLE IF NOT EXISTS node_config (
                key TEXT PRIMARY KEY,
                value_json TEXT NOT NULL,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            -- Materialized Guests (Capabilities that need to be spawned)
            CREATE TABLE IF NOT EXISTS materialized_guests (
                guest_id TEXT PRIMARY KEY,
                role TEXT NOT NULL,
                config_json TEXT NOT NULL,
                is_active BOOLEAN DEFAULT 1,
                last_seen DATETIME DEFAULT CURRENT_TIMESTAMP,
                active_pid INTEGER
            );

            -- Agent Context & Identity (Soul, hooks, etc)
            CREATE TABLE IF NOT EXISTS agent_identities (
                agent_id TEXT PRIMARY KEY,
                persona_name TEXT NOT NULL,
                bundle_json TEXT NOT NULL
            );

            -- Memory Apartments (Short, Long, Episodic, Semantic)
            CREATE TABLE IF NOT EXISTS memory_apartments (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_id TEXT NOT NULL,
                memory_type TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY(agent_id) REFERENCES agent_identities(agent_id)
            );
            COMMIT;
            "
        ).context("Failed to initialize Context Graph schema")?;
        
        // Attempt to run migration for existing databases. Ignore failure if the column already exists.
        let _ = self.conn.execute("ALTER TABLE materialized_guests ADD COLUMN active_pid INTEGER", []);
        
        info!("Context Graph schema initialized successfully.");
        Ok(())
    }

    /// Loads the base node capabilities assigned to this Ansible from the config table.
    pub fn load_node_capabilities(&self) -> Result<Option<NodeCapabilities>> {
        let mut stmt = self.conn.prepare("SELECT value_json FROM node_config WHERE key = 'capabilities'")?;
        let mut rows = stmt.query([])?;

        if let Some(row) = rows.next()? {
            let json: String = row.get(0)?;
            let caps: NodeCapabilities = serde_json::from_str(&json)?;
            Ok(Some(caps))
        } else {
            Ok(None)
        }
    }

    /// Saves the base node capabilities to the config table.
    pub fn save_node_capabilities(&self, caps: &NodeCapabilities) -> Result<()> {
        let json = serde_json::to_string(caps)?;
        self.conn.execute(
            "INSERT INTO node_config (key, value_json, updated_at) 
             VALUES ('capabilities', ?1, CURRENT_TIMESTAMP)
             ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json, updated_at=CURRENT_TIMESTAMP",
            [&json],
        )?;
        Ok(())
    }
}
