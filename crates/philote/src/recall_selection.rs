//! Request-local recall admission. The caller supplies the existing privacy
//! authority adapter; vault names, tags and model-written metadata are never
//! treated as authorization. No adapter or no authenticated principal = deny.
use crate::session::RecalledMemoryRecord;

pub trait RecallAuthority {
    /// Must resolve current policy against the authenticated actor. Adapters
    /// must also check destination/provider processing rights, not just reads.
    fn permits(
        &self,
        principal: &str,
        agent: &str,
        session: &str,
        record: &RecalledMemoryRecord,
    ) -> bool;
    fn permits_agent_graph(&self, _principal: &str, _agent: &str, _session: &str) -> bool {
        false
    }
}
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RecallCounts {
    pub considered: usize,
    pub admitted: usize,
    pub denied: usize,
    pub duplicates: usize,
    pub over_budget: usize,
}
/// Selection never changes durable memories. De-duplicate by identity and
/// normalized content, keeping the first authority-approved item. Oversized
/// records are omitted whole, rather than silently slicing away provenance.
pub fn select<'a>(
    principal: Option<&str>,
    agent: &str,
    session: &str,
    authority: Option<&dyn RecallAuthority>,
    records: &'a [RecalledMemoryRecord],
    max_items: usize,
    max_bytes: usize,
) -> (Vec<&'a RecalledMemoryRecord>, RecallCounts) {
    let mut out = Vec::new();
    let mut counts = RecallCounts::default();
    let mut ids = std::collections::BTreeSet::new();
    let mut content = std::collections::BTreeSet::new();
    let mut used = 0usize;
    for r in records {
        counts.considered += 1;
        let allowed = principal
            .filter(|p| !p.trim().is_empty())
            .zip(authority)
            .is_some_and(|(p, a)| a.permits(p, agent, session, r));
        if !allowed {
            counts.denied += 1;
            continue;
        }
        let key = r.content.split_whitespace().collect::<Vec<_>>().join(" ");
        let identity = r.id.as_ref().map(|id| (r.vault_id.clone(), id.clone()));
        if identity.as_ref().is_some_and(|k| ids.contains(k)) || content.contains(&key) {
            counts.duplicates += 1;
            continue;
        }
        let cost = serde_json::to_vec(r).map(|v| v.len()).unwrap_or(usize::MAX);
        if out.len() >= max_items || cost > max_bytes.saturating_sub(used) {
            counts.over_budget += 1;
            continue;
        }
        if let Some(k) = identity {
            ids.insert(k);
        }
        content.insert(key);
        used += cost;
        out.push(r);
        counts.admitted += 1;
    }
    (out, counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Allowed;
    impl RecallAuthority for Allowed {
        fn permits(&self, principal: &str, _: &str, _: &str, r: &RecalledMemoryRecord) -> bool {
            principal == "trusted-human" && r.id.as_deref() != Some("denied")
        }
    }
    fn memory(id: &str, content: &str) -> RecalledMemoryRecord {
        serde_json::from_value(serde_json::json!({"id":id,"concept":"synthetic","content":content}))
            .unwrap()
    }
    #[test]
    fn missing_identity_or_authority_fails_closed() {
        let rs = vec![memory("1", "synthetic")];
        assert!(
            select(None, "agent", "session", Some(&Allowed), &rs, 12, 3000)
                .0
                .is_empty()
        );
        assert!(
            select(
                Some("trusted-human"),
                "agent",
                "session",
                None,
                &rs,
                12,
                3000
            )
            .0
            .is_empty()
        );
        assert!(
            select(
                Some("other-human"),
                "agent",
                "session",
                Some(&Allowed),
                &rs,
                12,
                3000
            )
            .0
            .is_empty()
        );
    }
    #[test]
    fn recall_deduplication_and_limits_do_not_change_original_records() {
        let rs = vec![
            memory("denied", "private"),
            memory("1", "same  fact"),
            memory("2", "same fact"),
            memory("3", &"x".repeat(4000)),
            memory("4", "another"),
        ];
        let (selected, c) = select(
            Some("trusted-human"),
            "agent",
            "session",
            Some(&Allowed),
            &rs,
            1,
            1000,
        );
        assert_eq!(selected.len(), 1);
        assert_eq!(c.denied, 1);
        assert_eq!(c.duplicates, 1);
        assert_eq!(c.over_budget, 2);
        assert_eq!(rs.len(), 5);
        let (again, d) = select(
            Some("trusted-human"),
            "agent",
            "session",
            Some(&Allowed),
            &rs,
            1,
            1000,
        );
        assert_eq!(selected, again);
        assert_eq!(c, d);
    }
}
