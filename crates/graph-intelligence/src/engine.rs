use anyhow::{Context, Result};
use rusqlite::{params, Connection, Transaction};

use crate::schema::*;

pub struct GraphEngine {
    conn: Connection,
}

impl GraphEngine {
    /// Open or create a SQLite database at the given path.
    /// Use ":memory:" for an in-memory database.
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path).context("Failed to open graph database")?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let engine = Self { conn };
        engine.init_schema()?;
        Ok(engine)
    }

    pub fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS nodes (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                name TEXT NOT NULL,
                properties TEXT NOT NULL DEFAULT '{}',
                file_path TEXT,
                worktree TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                embedding BLOB,              -- Serialized f32 array
                embedding_model TEXT,        -- Model name, e.g. nomic_embed
                embedding_dims INTEGER,      -- Dimensionality
                embedding_updated TEXT,    -- ISO timestamp
                embedding_hash TEXT          -- Hash of source text
            );

            CREATE TABLE IF NOT EXISTS edges (
                source_id TEXT NOT NULL,
                target_id TEXT NOT NULL,
                relation TEXT NOT NULL,
                properties TEXT NOT NULL DEFAULT '{}',
                worktree TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (source_id, target_id, relation)
            );

            CREATE TABLE IF NOT EXISTS snippets (
                id TEXT PRIMARY KEY,
                node_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                signature TEXT NOT NULL,
                doc_comment TEXT,
                body TEXT,
                body_hash TEXT NOT NULL,
                file_path TEXT NOT NULL,
                line_start INTEGER NOT NULL,
                line_end INTEGER NOT NULL,
                language TEXT NOT NULL DEFAULT 'rust'
            );

            CREATE TABLE IF NOT EXISTS mutations (
                id TEXT PRIMARY KEY,
                timestamp TEXT NOT NULL,
                agent TEXT,
                session TEXT,
                action TEXT NOT NULL,
                target_node TEXT,
                from_value TEXT,
                to_value TEXT,
                reason TEXT,
                details TEXT NOT NULL DEFAULT '{}'
            );

            CREATE TABLE IF NOT EXISTS snapshots (
                id TEXT PRIMARY KEY,
                scan_time TEXT NOT NULL,
                commit_sha TEXT,
                worktree TEXT,
                metrics TEXT NOT NULL DEFAULT '{}'
            );

            CREATE INDEX IF NOT EXISTS idx_nodes_kind ON nodes(kind);
            CREATE INDEX IF NOT EXISTS idx_nodes_worktree ON nodes(worktree);
            CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_id);
            CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_id);
            CREATE INDEX IF NOT EXISTS idx_snippets_node ON snippets(node_id);
            CREATE INDEX IF NOT EXISTS idx_mutations_target ON mutations(target_node);

            CREATE VIRTUAL TABLE IF NOT EXISTS snippets_fts USING fts5(
                signature, doc_comment, body, content=snippets, content_rowid=rowid
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
                name, content=nodes, content_rowid=rowid
            );
            ",
        )?;
        self.migrate_legacy_schema()?;
        Ok(())
    }

    fn migrate_legacy_schema(&self) -> Result<()> {
        self.ensure_column("nodes", "embedding", "BLOB")?;
        self.ensure_column("nodes", "embedding_model", "TEXT")?;
        self.ensure_column("nodes", "embedding_dims", "INTEGER")?;
        self.ensure_column("nodes", "embedding_updated", "TEXT")?;
        self.ensure_column("nodes", "embedding_hash", "TEXT")?;
        Ok(())
    }

    fn ensure_column(&self, table: &str, column: &str, decl: &str) -> Result<()> {
        let found: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
                params![table, column],
                |row| row.get(0),
            )
            .with_context(|| format!("Failed to inspect schema for table {table}"))?;
        if found == 0 {
            self.conn
                .execute(
                    &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
                    [],
                )
                .with_context(|| format!("Failed to add column {column} to table {table}"))?;
        }
        Ok(())
    }

    // ── Node CRUD ──

    pub fn upsert_node(&self, node: &Node) -> Result<()> {
        let embedding_blob = node
            .embedding
            .as_ref()
            .map(|v| crate::schema::serialize_embedding(v));
        self.conn.execute(
            "INSERT INTO nodes (id, kind, name, properties, file_path, worktree, created_at, updated_at,
                              embedding, embedding_model, embedding_dims, embedding_updated, embedding_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(id) DO UPDATE SET
                kind=excluded.kind,
                name=excluded.name,
                properties=excluded.properties,
                file_path=excluded.file_path,
                worktree=excluded.worktree,
                updated_at=excluded.updated_at,
                embedding=COALESCE(excluded.embedding, nodes.embedding),
                embedding_model=COALESCE(excluded.embedding_model, nodes.embedding_model),
                embedding_dims=COALESCE(excluded.embedding_dims, nodes.embedding_dims),
                embedding_updated=COALESCE(excluded.embedding_updated, nodes.embedding_updated),
                embedding_hash=COALESCE(excluded.embedding_hash, nodes.embedding_hash)",
            params![
                node.id,
                node.kind.as_str(),
                node.name,
                node.properties.to_string(),
                node.file_path,
                node.worktree,
                node.created_at.to_rfc3339(),
                node.updated_at.to_rfc3339(),
                embedding_blob,
                node.embedding_model,
                node.embedding_dims,
                node.embedding_updated.map(|dt| dt.to_rfc3339()),
                node.embedding_hash,
            ],
        )?;
        // Update FTS
        self.conn
            .execute(
                "INSERT INTO nodes_fts(rowid, name)
             SELECT rowid, name FROM nodes WHERE id = ?1
             ON CONFLICT DO NOTHING",
                params![node.id],
            )
            .ok(); // FTS sync is best-effort
        Ok(())
    }

    pub fn insert_node_if_absent(&self, node: &Node) -> Result<bool> {
        let embedding_blob = node
            .embedding
            .as_ref()
            .map(|v| crate::schema::serialize_embedding(v));
        let changed = self.conn.execute(
            "INSERT INTO nodes (id, kind, name, properties, file_path, worktree, created_at, updated_at,
                              embedding, embedding_model, embedding_dims, embedding_updated, embedding_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(id) DO NOTHING",
            params![
                node.id,
                node.kind.as_str(),
                node.name,
                node.properties.to_string(),
                node.file_path,
                node.worktree,
                node.created_at.to_rfc3339(),
                node.updated_at.to_rfc3339(),
                embedding_blob,
                node.embedding_model,
                node.embedding_dims,
                node.embedding_updated.map(|dt| dt.to_rfc3339()),
                node.embedding_hash,
            ],
        )? > 0;
        if changed {
            self.conn
                .execute(
                    "INSERT INTO nodes_fts(rowid, name)
                 SELECT rowid, name FROM nodes WHERE id = ?1
                 ON CONFLICT DO NOTHING",
                    params![node.id],
                )
                .ok();
        }
        Ok(changed)
    }

    pub fn compare_and_swap_node(
        &self,
        node: &Node,
        expected_updated_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        let embedding_blob = node
            .embedding
            .as_ref()
            .map(|v| crate::schema::serialize_embedding(v));
        let changed = self.conn.execute(
            "UPDATE nodes
             SET kind = ?2,
                 name = ?3,
                 properties = ?4,
                 file_path = ?5,
                 worktree = ?6,
                 updated_at = ?7,
                 embedding = COALESCE(?8, embedding),
                 embedding_model = COALESCE(?9, embedding_model),
                 embedding_dims = COALESCE(?10, embedding_dims),
                 embedding_updated = COALESCE(?11, embedding_updated),
                 embedding_hash = COALESCE(?12, embedding_hash)
             WHERE id = ?1 AND updated_at = ?13",
            params![
                node.id,
                node.kind.as_str(),
                node.name,
                node.properties.to_string(),
                node.file_path,
                node.worktree,
                node.updated_at.to_rfc3339(),
                embedding_blob,
                node.embedding_model,
                node.embedding_dims,
                node.embedding_updated.map(|dt| dt.to_rfc3339()),
                node.embedding_hash,
                expected_updated_at.to_rfc3339(),
            ],
        )? > 0;
        if changed {
            self.conn
                .execute(
                    "INSERT INTO nodes_fts(rowid, name)
                 SELECT rowid, name FROM nodes WHERE id = ?1
                 ON CONFLICT DO NOTHING",
                    params![node.id],
                )
                .ok();
        }
        Ok(changed)
    }

    pub fn get_node(&self, id: &str) -> Result<Option<Node>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, properties, file_path, worktree, created_at, updated_at,
                    embedding, embedding_model, embedding_dims, embedding_updated, embedding_hash
             FROM nodes WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |row| Ok(row_to_node(row)))?;
        match rows.next() {
            Some(Ok(node)) => Ok(Some(node?)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn query_nodes(&self, kind: Option<NodeKind>, worktree: Option<&str>) -> Result<Vec<Node>> {
        let (sql, params_vec) = match (kind, worktree) {
            (Some(k), Some(w)) => (
                "SELECT id, kind, name, properties, file_path, worktree, created_at, updated_at,
                        embedding, embedding_model, embedding_dims, embedding_updated, embedding_hash
                 FROM nodes WHERE kind = ?1 AND worktree = ?2",
                vec![k.as_str().to_string(), w.to_string()],
            ),
            (Some(k), None) => (
                "SELECT id, kind, name, properties, file_path, worktree, created_at, updated_at,
                        embedding, embedding_model, embedding_dims, embedding_updated, embedding_hash
                 FROM nodes WHERE kind = ?1",
                vec![k.as_str().to_string()],
            ),
            (None, Some(w)) => (
                "SELECT id, kind, name, properties, file_path, worktree, created_at, updated_at,
                        embedding, embedding_model, embedding_dims, embedding_updated, embedding_hash
                 FROM nodes WHERE worktree = ?1",
                vec![w.to_string()],
            ),
            (None, None) => (
                "SELECT id, kind, name, properties, file_path, worktree, created_at, updated_at,
                        embedding, embedding_model, embedding_dims, embedding_updated, embedding_hash
                 FROM nodes",
                vec![],
            ),
        };
        let mut stmt = self.conn.prepare(sql)?;
        let params_refs: Vec<&dyn rusqlite::types::ToSql> = params_vec
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt.query_map(params_refs.as_slice(), |row| Ok(row_to_node(row)))?;
        let mut nodes = Vec::new();
        for row in rows {
            nodes.push(row??);
        }
        Ok(nodes)
    }

    pub fn delete_node(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM nodes WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ── Edge CRUD ──

    pub fn upsert_edge(&self, edge: &Edge) -> Result<()> {
        self.conn.execute(
            "INSERT INTO edges (source_id, target_id, relation, properties, worktree)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(source_id, target_id, relation) DO UPDATE SET
                properties=excluded.properties,
                worktree=excluded.worktree",
            params![
                edge.source_id,
                edge.target_id,
                edge.relation.as_str(),
                edge.properties.to_string(),
                edge.worktree,
            ],
        )?;
        Ok(())
    }

    pub fn get_edges_from(&self, node_id: &str) -> Result<Vec<Edge>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_id, target_id, relation, properties, worktree
             FROM edges WHERE source_id = ?1",
        )?;
        let rows = stmt.query_map(params![node_id], |row| Ok(row_to_edge(row)))?;
        let mut edges = Vec::new();
        for row in rows {
            edges.push(row??);
        }
        Ok(edges)
    }

    pub fn get_edges_to(&self, node_id: &str) -> Result<Vec<Edge>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_id, target_id, relation, properties, worktree
             FROM edges WHERE target_id = ?1",
        )?;
        let rows = stmt.query_map(params![node_id], |row| Ok(row_to_edge(row)))?;
        let mut edges = Vec::new();
        for row in rows {
            edges.push(row??);
        }
        Ok(edges)
    }

    pub fn delete_edges_for_node(&self, node_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM edges WHERE source_id = ?1 OR target_id = ?1",
            params![node_id],
        )?;
        Ok(())
    }

    // ── Snippet CRUD ──

    pub fn upsert_snippet(&self, snippet: &Snippet) -> Result<()> {
        self.conn.execute(
            "INSERT INTO snippets (id, node_id, kind, signature, doc_comment, body, body_hash, file_path, line_start, line_end, language)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
                node_id=excluded.node_id,
                kind=excluded.kind,
                signature=excluded.signature,
                doc_comment=excluded.doc_comment,
                body=excluded.body,
                body_hash=excluded.body_hash,
                file_path=excluded.file_path,
                line_start=excluded.line_start,
                line_end=excluded.line_end,
                language=excluded.language",
            params![
                snippet.id,
                snippet.node_id,
                snippet.kind.as_str(),
                snippet.signature,
                snippet.doc_comment,
                snippet.body,
                snippet.body_hash,
                snippet.file_path,
                snippet.line_start,
                snippet.line_end,
                snippet.language,
            ],
        )?;
        Ok(())
    }

    pub fn get_snippets_for_node(&self, node_id: &str) -> Result<Vec<Snippet>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, node_id, kind, signature, doc_comment, body, body_hash, file_path, line_start, line_end, language
             FROM snippets WHERE node_id = ?1",
        )?;
        let rows = stmt.query_map(params![node_id], |row| Ok(row_to_snippet(row)))?;
        let mut snippets = Vec::new();
        for row in rows {
            snippets.push(row??);
        }
        Ok(snippets)
    }

    pub fn search_snippets(&self, query: &str) -> Result<Vec<Snippet>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.node_id, s.kind, s.signature, s.doc_comment, s.body, s.body_hash, s.file_path, s.line_start, s.line_end, s.language
             FROM snippets s
             JOIN snippets_fts fts ON s.rowid = fts.rowid
             WHERE snippets_fts MATCH ?1
             LIMIT 100",
        )?;
        let rows = stmt.query_map(params![query], |row| Ok(row_to_snippet(row)))?;
        let mut snippets = Vec::new();
        for row in rows {
            snippets.push(row??);
        }
        Ok(snippets)
    }

    // ── Mutations ──

    pub fn record_mutation(&self, mutation: &Mutation) -> Result<()> {
        self.conn.execute(
            "INSERT INTO mutations (id, timestamp, agent, session, action, target_node, from_value, to_value, reason, details)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                mutation.id,
                mutation.timestamp.to_rfc3339(),
                mutation.agent,
                mutation.session,
                mutation.action,
                mutation.target_node,
                mutation.from_value,
                mutation.to_value,
                mutation.reason,
                mutation.details.to_string(),
            ],
        )?;
        Ok(())
    }

    pub fn get_mutations(&self, target_node: Option<&str>, limit: usize) -> Result<Vec<Mutation>> {
        let (sql, params_vec): (&str, Vec<String>) = match target_node {
            Some(t) => (
                "SELECT id, timestamp, agent, session, action, target_node, from_value, to_value, reason, details
                 FROM mutations WHERE target_node = ?1 ORDER BY timestamp DESC LIMIT ?2",
                vec![t.to_string(), limit.to_string()],
            ),
            None => (
                "SELECT id, timestamp, agent, session, action, target_node, from_value, to_value, reason, details
                 FROM mutations ORDER BY timestamp DESC LIMIT ?1",
                vec![limit.to_string()],
            ),
        };
        let mut stmt = self.conn.prepare(sql)?;
        let params_refs: Vec<&dyn rusqlite::types::ToSql> = params_vec
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt.query_map(params_refs.as_slice(), |row| Ok(row_to_mutation(row)))?;
        let mut mutations = Vec::new();
        for row in rows {
            mutations.push(row??);
        }
        Ok(mutations)
    }

    // ── Snapshots ──

    pub fn record_snapshot(&self, snapshot: &ScanSnapshot) -> Result<()> {
        self.conn.execute(
            "INSERT INTO snapshots (id, scan_time, commit_sha, worktree, metrics)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                snapshot.id,
                snapshot.scan_time.to_rfc3339(),
                snapshot.commit_sha,
                snapshot.worktree,
                snapshot.metrics.to_string(),
            ],
        )?;
        Ok(())
    }

    // ── Bulk operations ──

    pub fn clear_worktree(&self, worktree: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM snippets WHERE node_id IN (SELECT id FROM nodes WHERE worktree = ?1)",
            params![worktree],
        )?;
        self.conn
            .execute("DELETE FROM edges WHERE worktree = ?1", params![worktree])?;
        self.conn
            .execute("DELETE FROM nodes WHERE worktree = ?1", params![worktree])?;
        Ok(())
    }

    /// Clear all worktree/branch nodes before a git rescan so pruned checkouts
    /// don't persist as ghost nodes across scans.
    pub fn clear_git_topology_nodes(&self) -> Result<()> {
        let kinds = "'worktree','branch'";
        self.conn.execute(
            &format!(
                "DELETE FROM edges WHERE source_id IN (SELECT id FROM nodes WHERE kind IN ({kinds})) \
                 OR target_id IN (SELECT id FROM nodes WHERE kind IN ({kinds}))"
            ),
            [],
        )?;
        self.conn
            .execute(&format!("DELETE FROM nodes WHERE kind IN ({kinds})"), [])?;
        Ok(())
    }

    /// Clear all scanner-created doc nodes (worktree='', doc-like kinds).
    /// Called before doc rescan to prevent ghost nodes from renamed/deleted files.
    /// Preserves agent-created nodes (decisions, sessions, workstreams, test runs).
    pub fn clear_scanned_doc_nodes(&self) -> Result<()> {
        let doc_kinds = "'proposal','domain','seam','task','document','skill','sver'";
        self.conn.execute(
            &format!(
                "DELETE FROM edges WHERE source_id IN (SELECT id FROM nodes WHERE worktree = '' AND kind IN ({doc_kinds})) \
                 OR target_id IN (SELECT id FROM nodes WHERE worktree = '' AND kind IN ({doc_kinds}))"
            ),
            [],
        )?;
        self.conn.execute(
            &format!("DELETE FROM nodes WHERE worktree = '' AND kind IN ({doc_kinds})"),
            [],
        )?;
        Ok(())
    }

    pub fn transaction<F, T>(&mut self, f: F) -> Result<T>
    where
        F: FnOnce(&Transaction) -> Result<T>,
    {
        let tx = self.conn.transaction()?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    /// Count nodes of a given kind.
    pub fn count_nodes(&self, kind: Option<NodeKind>) -> Result<usize> {
        let count: i64 = match kind {
            Some(k) => self.conn.query_row(
                "SELECT COUNT(*) FROM nodes WHERE kind = ?1",
                params![k.as_str()],
                |row| row.get(0),
            )?,
            None => self
                .conn
                .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))?,
        };
        Ok(count as usize)
    }

    /// Count all edges.
    pub fn count_edges(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Full-text search over nodes by name.
    pub fn search_nodes(&self, query: &str) -> Result<Vec<Node>> {
        let mut stmt = self.conn.prepare(
            "SELECT n.id, n.kind, n.name, n.properties, n.file_path, n.worktree, n.created_at, n.updated_at,
                    n.embedding, n.embedding_model, n.embedding_dims, n.embedding_updated, n.embedding_hash
             FROM nodes n
             JOIN nodes_fts fts ON n.rowid = fts.rowid
             WHERE nodes_fts MATCH ?1
             LIMIT 100",
        )?;
        let rows = stmt.query_map(params![query], |row| Ok(row_to_node(row)))?;
        let mut nodes = Vec::new();
        for row in rows {
            nodes.push(row??);
        }
        Ok(nodes)
    }

    /// Count all snippets.
    pub fn count_snippets(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM snippets", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Find active sessions older than `max_age` and close them as timed-out.
    /// Returns the list of session IDs that were cleaned up.
    pub fn cleanup_stale_sessions(&self, max_age: chrono::Duration) -> Result<Vec<String>> {
        let cutoff = (chrono::Utc::now() - max_age).to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, properties, file_path, worktree, created_at, updated_at,
                    embedding, embedding_model, embedding_dims, embedding_updated, embedding_hash
             FROM nodes
             WHERE kind = 'session'
               AND json_extract(properties, '$.status') = 'active'
               AND updated_at < ?1",
        )?;
        let rows = stmt.query_map(params![cutoff], |row| Ok(row_to_node(row)))?;
        let mut cleaned = Vec::new();
        let mut stale_sessions = Vec::new();
        for row in rows {
            stale_sessions.push(row??);
        }
        for mut session in stale_sessions {
            let mut props = session.properties.as_object().cloned().unwrap_or_default();
            props.insert("status".to_string(), serde_json::json!("timed_out"));
            props.insert(
                "end_time".to_string(),
                serde_json::json!(chrono::Utc::now().to_rfc3339()),
            );
            props.insert(
                "close_reason".to_string(),
                serde_json::json!("auto_cleanup: exceeded max session age"),
            );
            session.properties = serde_json::Value::Object(props);
            session.updated_at = chrono::Utc::now();
            self.upsert_node(&session)?;
            self.close_linked_workstreams(
                &session,
                "timed_out",
                Some("auto_cleanup: exceeded max session age"),
            )?;
            cleaned.push(session.id.clone());
        }
        Ok(cleaned)
    }

    /// Find workstreams associated with a session via explicit edges or seam fallback.
    pub fn linked_workstream_ids(&self, session: &Node) -> Result<Vec<String>> {
        let mut workstream_ids: Vec<String> = self
            .get_edges_from(&session.id)?
            .into_iter()
            .filter(|edge| edge.relation == EdgeRelation::WorkingOn)
            .map(|edge| edge.target_id)
            .collect();

        if workstream_ids.is_empty() {
            if let Some(seam_id) = session
                .properties
                .get("seam_id")
                .and_then(|value| value.as_str())
            {
                workstream_ids.push(format!("workstream:{}", seam_id.replace("seam:", "")));
            }
        }

        workstream_ids.sort();
        workstream_ids.dedup();
        Ok(workstream_ids)
    }

    /// Close linked workstreams when the owning session leaves the active state.
    pub fn close_linked_workstreams(
        &self,
        session: &Node,
        status: &str,
        summary: Option<&str>,
    ) -> Result<Vec<String>> {
        let now = chrono::Utc::now();
        let mut updated_ids = Vec::new();

        for workstream_id in self.linked_workstream_ids(session)? {
            let Some(mut workstream) = self.get_node(&workstream_id)? else {
                continue;
            };

            let mut props = workstream
                .properties
                .as_object()
                .cloned()
                .unwrap_or_default();
            props.insert("status".to_string(), serde_json::json!(status));
            props.insert("end_time".to_string(), serde_json::json!(now.to_rfc3339()));
            props.insert(
                "closed_by_session".to_string(),
                serde_json::json!(session.id.clone()),
            );
            if let Some(phase) = session
                .properties
                .get("phase")
                .and_then(|value| value.as_str())
            {
                props.insert("phase".to_string(), serde_json::json!(phase));
            }
            if let Some(summary) = summary.filter(|value| !value.is_empty()) {
                props.insert("summary".to_string(), serde_json::json!(summary));
            }

            workstream.properties = serde_json::Value::Object(props);
            workstream.updated_at = now;
            self.upsert_node(&workstream)?;
            updated_ids.push(workstream_id);
        }

        Ok(updated_ids)
    }

    /// Reconcile active workstreams that no longer have an active backing session.
    pub fn cleanup_orphaned_workstreams(&self) -> Result<Vec<String>> {
        let active_session_seam_ids: std::collections::HashSet<String> = self
            .query_nodes(Some(NodeKind::Session), None)?
            .into_iter()
            .filter(|session| {
                session
                    .properties
                    .get("status")
                    .and_then(|value| value.as_str())
                    == Some("active")
            })
            .filter_map(|session| {
                session
                    .properties
                    .get("seam_id")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string())
            })
            .collect();

        let now = chrono::Utc::now();
        let mut cleaned = Vec::new();
        for mut workstream in self.query_nodes(Some(NodeKind::Workstream), None)? {
            let status = workstream
                .properties
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown");
            let seam_id = workstream
                .properties
                .get("seam_id")
                .and_then(|value| value.as_str())
                .unwrap_or("");

            if status != "active" || active_session_seam_ids.contains(seam_id) {
                continue;
            }

            let mut props = workstream
                .properties
                .as_object()
                .cloned()
                .unwrap_or_default();
            props.insert("status".to_string(), serde_json::json!("orphaned"));
            props.insert("end_time".to_string(), serde_json::json!(now.to_rfc3339()));
            props.insert(
                "close_reason".to_string(),
                serde_json::json!("auto_cleanup: no active session"),
            );

            workstream.properties = serde_json::Value::Object(props);
            workstream.updated_at = now;
            self.upsert_node(&workstream)?;
            cleaned.push(workstream.id);
        }

        Ok(cleaned)
    }

    /// Query all nodes of a given kind with a specific property value.
    pub fn query_nodes_by_property(
        &self,
        kind: NodeKind,
        property_key: &str,
        property_value: &str,
    ) -> Result<Vec<Node>> {
        let json_path = format!("$.{}", property_key);
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, properties, file_path, worktree, created_at, updated_at,
                    embedding, embedding_model, embedding_dims, embedding_updated, embedding_hash
             FROM nodes
             WHERE kind = ?1
               AND json_extract(properties, ?2) = ?3",
        )?;
        let rows = stmt.query_map(params![kind.as_str(), json_path, property_value], |row| {
            Ok(row_to_node(row))
        })?;
        let mut nodes = Vec::new();
        for row in rows {
            nodes.push(row??);
        }
        Ok(nodes)
    }
}

// ── Row mappers ──

fn row_to_node(row: &rusqlite::Row) -> Result<Node> {
    let kind_str: String = row.get(1)?;
    let props_str: String = row.get(3)?;
    let created_str: String = row.get(6)?;
    let updated_str: String = row.get(7)?;
    let embedding_blob: Option<Vec<u8>> = row.get(8)?;
    let embedding_updated: Option<String> = row.get(11)?;

    Ok(Node {
        id: row.get(0)?,
        kind: NodeKind::from_str(&kind_str).unwrap_or(NodeKind::Component),
        name: row.get(2)?,
        properties: serde_json::from_str(&props_str).unwrap_or_default(),
        file_path: row.get(4)?,
        worktree: row.get(5)?,
        created_at: chrono::DateTime::parse_from_rfc3339(&created_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now()),
        updated_at: chrono::DateTime::parse_from_rfc3339(&updated_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now()),
        embedding: embedding_blob.map(|b| crate::schema::deserialize_embedding(&b)),
        embedding_model: row.get(9)?,
        embedding_dims: row.get(10)?,
        embedding_updated: embedding_updated.and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&chrono::Utc))
        }),
        embedding_hash: row.get(12)?,
    })
}

fn row_to_edge(row: &rusqlite::Row) -> Result<Edge> {
    let rel_str: String = row.get(2)?;
    let props_str: String = row.get(3)?;
    Ok(Edge {
        source_id: row.get(0)?,
        target_id: row.get(1)?,
        relation: EdgeRelation::from_str(&rel_str).unwrap_or(EdgeRelation::References),
        properties: serde_json::from_str(&props_str).unwrap_or_default(),
        worktree: row.get(4)?,
    })
}

fn row_to_snippet(row: &rusqlite::Row) -> Result<Snippet> {
    let kind_str: String = row.get(2)?;
    Ok(Snippet {
        id: row.get(0)?,
        node_id: row.get(1)?,
        kind: SnippetKind::from_str(&kind_str).unwrap_or(SnippetKind::Function),
        signature: row.get(3)?,
        doc_comment: row.get(4)?,
        body: row.get(5)?,
        body_hash: row.get(6)?,
        file_path: row.get(7)?,
        line_start: row.get(8)?,
        line_end: row.get(9)?,
        language: row.get(10)?,
    })
}

fn row_to_mutation(row: &rusqlite::Row) -> Result<Mutation> {
    let ts_str: String = row.get(1)?;
    let details_str: String = row.get(9)?;
    Ok(Mutation {
        id: row.get(0)?,
        timestamp: chrono::DateTime::parse_from_rfc3339(&ts_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now()),
        agent: row.get(2)?,
        session: row.get(3)?,
        action: row.get(4)?,
        target_node: row.get(5)?,
        from_value: row.get(6)?,
        to_value: row.get(7)?,
        reason: row.get(8)?,
        details: serde_json::from_str(&details_str).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_engine_basic_crud() {
        let mut engine = GraphEngine::open(":memory:").unwrap();
        let now = chrono::Utc::now();

        let node = Node {
            id: "test-node-1".into(),
            kind: NodeKind::Crate,
            name: "test-crate".into(),
            properties: serde_json::json!({"version": "0.1.0"}),
            file_path: Some("Cargo.toml".into()),
            worktree: "main".into(),
            created_at: now,
            updated_at: now,
            embedding: None,
            embedding_model: None,
            embedding_dims: None,
            embedding_updated: None,
            embedding_hash: None,
        };

        engine.upsert_node(&node).unwrap();
        let fetched = engine.get_node("test-node-1").unwrap().unwrap();
        assert_eq!(fetched.name, "test-crate");
        assert_eq!(fetched.kind, NodeKind::Crate);

        let edge = Edge {
            source_id: "test-node-1".into(),
            target_id: "test-node-2".into(),
            relation: EdgeRelation::Contains,
            properties: serde_json::json!({}),
            worktree: "main".into(),
        };
        engine.upsert_edge(&edge).unwrap();
        let edges = engine.get_edges_from("test-node-1").unwrap();
        assert_eq!(edges.len(), 1);

        // Test transaction
        engine.transaction(|tx| {
            tx.execute(
                "INSERT INTO nodes (id, kind, name, properties, worktree, created_at, updated_at)
                 VALUES ('tx-node', 'module', 'tx-mod', '{}', 'main', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z')",
                [],
            )?;
            Ok(())
        }).unwrap();

        let tx_node = engine.get_node("tx-node").unwrap();
        assert!(tx_node.is_some());
    }
}
