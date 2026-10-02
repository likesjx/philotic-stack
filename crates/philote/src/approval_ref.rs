//! Compact approval references for channel buttons (seam `approval-action-ids`).
//!
//! An approval card's buttons must say *which* approval they answer, or a tap on a
//! stale card resolves whatever happens to be pending now (and a stale "Trust for
//! session" tap grants session-wide pre-approval). Telegram caps `callback_data` at
//! 64 bytes and approval ids come in several shapes (uuid, `mcp-gate:<turn>`,
//! `scripted_gate:<gate>`), so buttons carry a short, stable hash of the full id
//! rather than a truncation, which would collide for prefixed ids.
//!
//! The hash is FNV-1a 64 so it is stable across processes and builds: a card shown
//! before a restart or deploy still matches the restored pending approval after it.

/// Hex characters of the FNV-1a hash carried on a button (40 bits).
const REF_LEN: usize = 10;

/// Approval verbs that may carry a reference in `callback_data`.
const VERBS: [&str; 3] = ["approve", "deny", "trust"];

/// Short, stable reference for an approval id, suitable for `callback_data`.
pub fn approval_ref(approval_id: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in approval_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")[..REF_LEN].to_string()
}

/// `callback_data` for an approval button: `verb:<ref>` when the approval has an id,
/// the bare verb otherwise (legacy cards, unchecked).
pub fn approval_callback(verb: &str, approval_id: Option<&str>) -> String {
    match approval_id.filter(|id| !id.is_empty()) {
        Some(id) => format!("{verb}:{}", approval_ref(id)),
        None => verb.to_string(),
    }
}

/// Reference carried by an approval button's `callback_data`, if any.
///
/// Returns `None` for bare legacy callbacks (`"approve"`), typed commands, and
/// anything that is not an approval callback, so those keep today's behaviour.
pub fn callback_approval_ref(callback_data: &str) -> Option<&str> {
    let (verb, reference) = callback_data.split_once(':')?;
    if !VERBS.contains(&verb) || reference.is_empty() {
        return None;
    }
    Some(reference)
}

/// True when the callback came from the "Trust for session" button, bare or referenced.
pub fn is_trust_callback(callback_data: &str) -> bool {
    callback_data == "trust" || callback_data.starts_with("trust:")
}

/// Whether a button reference answers the approval that is pending now.
pub fn ref_matches(reference: &str, pending_approval_id: Option<&str>) -> bool {
    pending_approval_id
        .filter(|id| !id.is_empty())
        .is_some_and(|id| approval_ref(id) == reference)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_is_stable_and_short() {
        let a = approval_ref("2f6c1d1e-6a43-4c1b-9d55-6f0a1f7c2b10");
        assert_eq!(a.len(), REF_LEN);
        assert_eq!(a, approval_ref("2f6c1d1e-6a43-4c1b-9d55-6f0a1f7c2b10"));
        // Pinned value: changing the hash would orphan every card already sent.
        assert_eq!(approval_ref("mcp-gate:turn-1"), "ce1ce1e0d6");
        assert_eq!(approval_ref(""), "cbf29ce484");
    }

    #[test]
    fn prefixed_ids_do_not_collide() {
        assert_ne!(
            approval_ref("scripted_gate:deploy"),
            approval_ref("scripted_gate:delete")
        );
        assert_ne!(
            approval_ref("mcp-gate:turn-1"),
            approval_ref("mcp-gate:turn-2")
        );
    }

    #[test]
    fn callbacks_fit_telegram_limit() {
        let long_id = "scripted_gate:".to_string() + &"x".repeat(500);
        for verb in VERBS {
            let data = approval_callback(verb, Some(&long_id));
            assert!(data.len() <= 64, "{data} exceeds 64 bytes");
        }
    }

    #[test]
    fn missing_id_keeps_bare_legacy_callback() {
        assert_eq!(approval_callback("approve", None), "approve");
        assert_eq!(approval_callback("trust", Some("")), "trust");
    }

    #[test]
    fn parses_referenced_callbacks_only() {
        let data = approval_callback("deny", Some("abc"));
        assert_eq!(
            callback_approval_ref(&data),
            Some(approval_ref("abc").as_str())
        );
        assert_eq!(callback_approval_ref("approve"), None);
        assert_eq!(callback_approval_ref("approve:"), None);
        assert_eq!(callback_approval_ref("/model gpt"), None);
        assert_eq!(callback_approval_ref("role:brain"), None);
    }

    #[test]
    fn matches_only_the_pending_approval() {
        let reference = approval_ref("approval-a");
        assert!(ref_matches(&reference, Some("approval-a")));
        assert!(!ref_matches(&reference, Some("approval-b")));
        assert!(!ref_matches(&reference, None));
        assert!(!ref_matches(&reference, Some("")));
    }

    #[test]
    fn trust_detection_covers_referenced_form() {
        assert!(is_trust_callback("trust"));
        assert!(is_trust_callback(&approval_callback("trust", Some("x"))));
        assert!(!is_trust_callback("approve"));
        assert!(!is_trust_callback("trustworthy"));
    }
}
