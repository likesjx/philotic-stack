//! Direct operator-only OpenLoop lifecycle actions, never model-projected.
//! Compare the complete displayed text/lifecycle snapshot and atomically append
//! a receipt. Retry IDs are node-and-actor scoped; replay returns the original audit.
use anyhow::Result;
use neo4rs::{BoltMap, BoltType, Graph, query};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const FIELDS: &[&str] = &[
    "title",
    "claim_summary",
    "description",
    "status",
    "loop_status",
    "validation_state",
    "last_confirmed_at",
    "resolved_at",
    "resolution_note",
    "loop_action_revision",
];

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Confirm,
    Close,
    Reopen,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LoopAction {
    pub id: String,
    pub actor: String,
    pub request_id: String,
    pub action: Action,
    pub before: BTreeMap<String, Value>,
    pub note: String,
}

impl LoopAction {
    pub fn changes(&self, now: &str) -> Result<BTreeMap<String, Value>, &'static str> {
        if !self.id.starts_with("life:")
            || self.id.len() <= 5
            || self.id.len() > 512
            || self.actor.trim().is_empty()
            || self.actor.len() > 512
            || !(32..=64).contains(&self.request_id.len())
            || !self
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || self.note.len() > 4096
        {
            return Err("invalid identity, retry id or note");
        }
        if self.before.len() != FIELDS.len() || FIELDS.iter().any(|k| !self.before.contains_key(*k))
        {
            return Err("complete original text and lifecycle snapshot required");
        }
        for (key, value) in &self.before {
            if key == "loop_action_revision" {
                if !value.is_null() && value.as_i64().is_none_or(|n| n < 0 || n == i64::MAX) {
                    return Err("invalid revision");
                }
            } else if !value.is_null() && value.as_str().is_none_or(|s| s.len() > 16384) {
                return Err("original properties must be text or null");
            }
        }
        let state = self.before["validation_state"]
            .as_str()
            .unwrap_or("inferred");
        if !["proposed", "inferred", "confirmed", "conflicted"].contains(&state) {
            return Err("retired or unknown validation state cannot be changed here");
        }
        let status = self.before["status"]
            .as_str()
            .or(self.before["loop_status"].as_str())
            .unwrap_or("open");
        let aliases = [
            self.before["status"].as_str(),
            self.before["loop_status"].as_str(),
        ];
        if aliases.contains(&Some("retired")) {
            return Err("retired loops cannot be reopened here");
        }
        let closed = aliases
            .iter()
            .flatten()
            .any(|s| ["resolved", "done", "fulfilled", "abandoned"].contains(s));
        let active =
            !closed && ["open", "active", "pending", "blocked", "in_progress"].contains(&status);
        let mut changes = BTreeMap::from([(
            "loop_action_revision".into(),
            json!(self.before["loop_action_revision"].as_i64().unwrap_or(0) + 1),
        )]);
        match self.action {
            Action::Confirm if active && ["proposed", "inferred"].contains(&state) => {
                changes.insert("validation_state".into(), json!("confirmed"));
                changes.insert("last_confirmed_at".into(), json!(now));
            }
            Action::Close if active => {
                changes.insert("status".into(), json!("resolved"));
                changes.insert("resolved_at".into(), json!(now));
                changes.insert("resolution_note".into(), json!(self.note.trim()));
            }
            Action::Reopen if closed => {
                changes.insert("status".into(), json!("open"));
                // Historical terminal aliases are also consulted by recall.
                // Remove the alias; only status is written as lifecycle truth.
                changes.insert("loop_status".into(), Value::Null);
                changes.insert("resolved_at".into(), Value::Null);
                changes.insert("resolution_note".into(), Value::Null);
            }
            _ => return Err("action is not allowed for the original lifecycle state"),
        }
        Ok(changes)
    }

    pub fn audit_id(&self) -> String {
        // Length framing prevents delimiter collisions. Node scoping also
        // prevents independent node writes from racing on one audit identity.
        format!(
            "loop-action:{}:{}:{}:{}:{}",
            self.actor.len(),
            self.actor,
            self.id.len(),
            self.id,
            self.request_id
        )
    }
}

pub const ACTION_QUERY: &str = "MATCH (n {id: $id}) \
    WITH collect(n) AS candidates WHERE size(candidates) = 1 \
    UNWIND candidates AS n WITH n WHERE n:OpenLoop AND \
    all(k IN keys($before) WHERE coalesce(n[k] = $before[k], n[k] IS NULL AND $before[k] IS NULL)) \
    CREATE (a:LifeLoopAction {id: $audit_id, node_id: $id, actor: $actor, action: $action, \
      acted_at: $acted_at, request_json: $request_json, before_json: $before_json, after_json: $after_json, note: $note}) \
    SET n += $changes RETURN a.id AS audit_id";

fn bolt_map(values: &BTreeMap<String, Value>) -> BoltType {
    let mut map = BoltMap::new();
    for (key, value) in values {
        let value = match value {
            Value::String(s) => s.as_str().into(),
            Value::Number(n) => n.as_i64().expect("validated integer").into(),
            Value::Null => BoltType::Null(neo4rs::BoltNull),
            _ => unreachable!("validated scalar"),
        };
        map.put(key.as_str().into(), value);
    }
    BoltType::Map(map)
}

async fn replay(graph: &Graph, input: &LoopAction, request: &str) -> Result<Option<Value>> {
    let audit_id = input.audit_id();
    let mut rows = graph.execute(query("MATCH (a:LifeLoopAction {id: $audit_id}) RETURN a.request_json AS request_json LIMIT 2")
        .param("audit_id", audit_id.as_str())).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let same = row.get::<String>("request_json")? == request;
    let duplicate = rows.next().await?.is_some();
    while rows.next().await?.is_some() {}
    Ok(Some(if same && !duplicate {
        json!({"status":"saved", "node_id":input.id, "audit_id":audit_id, "request_id":input.request_id, "action":input.action, "replayed":true})
    } else {
        json!({"status":"conflict", "error":"Retry id already used for a different action"})
    }))
}

pub async fn apply(graph: &Graph, input: &LoopAction, now: &str) -> Result<Value> {
    let changes = match input.changes(now) {
        Ok(changes) => changes,
        Err(error) => return Ok(json!({"status":"invalid_request", "error":error})),
    };
    let request = serde_json::to_string(input)?;
    if let Some(receipt) = replay(graph, input, &request).await? {
        return Ok(receipt);
    }
    let audit_id = input.audit_id();
    let action = serde_json::to_value(input.action)?;
    let mut after = input.before.clone();
    after.extend(changes.clone());
    let mut rows = graph
        .execute(
            query(ACTION_QUERY)
                .param("id", input.id.as_str())
                .param("actor", input.actor.as_str())
                .param("audit_id", audit_id.as_str())
                .param("action", action.as_str().unwrap())
                .param("acted_at", now)
                .param("request_json", request.as_str())
                .param("before_json", serde_json::to_string(&input.before)?)
                .param("after_json", serde_json::to_string(&after)?)
                .param("note", input.note.as_str())
                .param("before", bolt_map(&input.before))
                .param("changes", bolt_map(&changes)),
        )
        .await?;
    let saved = rows.next().await?.is_some();
    while rows.next().await?.is_some() {} // commit completion precedes receipt
    if saved {
        return Ok(
            json!({"status":"saved", "node_id":input.id, "audit_id":audit_id, "request_id":input.request_id, "action":input.action, "replayed":false}),
        );
    }
    // Another identical retry may have won while this request was waiting.
    if let Some(receipt) = replay(graph, input, &request).await? {
        return Ok(receipt);
    }
    Ok(
        json!({"status":"conflict", "error":"Loop missing, ambiguous, wrong type or changed. Reload before acting."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn input(action: Action) -> LoopAction {
        let mut before: BTreeMap<String, Value> =
            FIELDS.iter().map(|k| ((*k).into(), Value::Null)).collect();
        before.insert("validation_state".into(), json!("proposed"));
        LoopAction {
            id: "life:loop:test".into(),
            actor: "edge:test".into(),
            request_id: "a".repeat(32),
            action,
            before,
            note: "Done".into(),
        }
    }
    #[test]
    fn confirm_and_close_are_orthogonal() {
        let confirm = input(Action::Confirm).changes("now").unwrap();
        assert_eq!(confirm["validation_state"], "confirmed");
        assert!(!confirm.contains_key("status"));
        let close = input(Action::Close).changes("now").unwrap();
        assert_eq!(close["status"], "resolved");
        assert!(!close.contains_key("validation_state"));
    }
    #[test]
    fn reopen_clears_legacy_terminal_alias_and_keeps_validation() {
        let mut input = input(Action::Reopen);
        input.before.insert("loop_status".into(), json!("resolved"));
        input.before.insert("status".into(), json!("open"));
        let changes = input.changes("now").unwrap();
        assert_eq!(changes["status"], "open");
        assert_eq!(changes["loop_status"], Value::Null);
        assert!(!changes.contains_key("validation_state"));
    }
    #[test]
    fn protected_and_invalid_transitions_fail_closed() {
        let mut e = input(Action::Confirm);
        e.before
            .insert("validation_state".into(), json!("conflicted"));
        assert!(e.changes("now").is_err());
        e.before.insert("validation_state".into(), json!("retired"));
        assert!(e.changes("now").is_err());
        let mut e = input(Action::Close);
        e.before.insert("status".into(), json!("resolved"));
        assert!(e.changes("now").is_err());
        e.before.remove("status");
        assert!(e.changes("now").is_err());
    }
    #[test]
    fn full_snapshot_and_retry_id_are_required() {
        let mut e = input(Action::Close);
        e.request_id.clear();
        assert!(e.changes("now").is_err());
        let mut e = input(Action::Close);
        e.before.insert("confidence".into(), json!(1));
        assert!(e.changes("now").is_err());
        let mut e = input(Action::Close);
        e.before.insert("loop_action_revision".into(), json!(-1));
        assert!(e.changes("now").is_err());
    }
    #[test]
    fn retry_identity_is_node_scoped_and_unambiguous() {
        let a = input(Action::Close);
        let mut b = input(Action::Close);
        b.id.push_str("-other");
        assert_ne!(a.audit_id(), b.audit_id());
        let mut retired = input(Action::Reopen);
        retired.before.insert("status".into(), json!("retired"));
        retired
            .before
            .insert("loop_status".into(), json!("resolved"));
        assert!(retired.changes("now").is_err());
    }
    #[test]
    fn query_is_bounded_atomic_and_no_implicit_node_creation() {
        assert!(ACTION_QUERY.contains("size(candidates) = 1"));
        assert!(ACTION_QUERY.contains("n:OpenLoop"));
        assert!(ACTION_QUERY.contains("CREATE (a:LifeLoopAction"));
        assert!(!ACTION_QUERY.contains("MERGE"));
    }
}
