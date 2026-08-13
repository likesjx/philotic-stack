//! Single source of truth for which tool names execute *inside the agent
//! process* (`execution_mode == "local_agent"`).
//!
//! Before the aria-mesh-steward slice there were TWO diverging copies of this
//! allowlist — one in `philote::session` (route assembly on the agent side)
//! and one in `aiua::service::ipc` (`compose_tool_assembly` on the hotel
//! side). Divergence was a live bug class: the hotel could route a tool as
//! `local_agent` while philote's own fallback routed the same tool to a tool
//! runner (or vice versa), and tools present in neither list — e.g.
//! `memory.delta_digest`, granted by the architect profile — could not route
//! at all. Both call sites now delegate here; edit ONLY this list.
//!
//! Membership contract: a name belongs here iff philote's
//! `execute_local_agent_tool` has (or is expected to have) a dispatch arm for
//! it. Neither of the historical copies used prefix rules — membership is
//! exact-name, and this union preserves everything either side had.
pub const LOCAL_AGENT_TOOLS: &[&str] = &[
    // ── Session / hotel diagnostics ─────────────────────────────────────────
    "session.status",
    "hotel.status",
    "hotel.logs",
    "hotel.perimeter.status",
    "hotel.perimeter.refresh",
    "hotel.egress.check",
    "hotel.best_place_to_run",
    // ── Agent / role / skill governance ─────────────────────────────────────
    "agent.configure",
    "skill.register",
    "skill.list",
    "skill.assign",
    "skill.revoke",
    "skill.set_state",
    "skill.audit",
    "subagent.spawn",
    "role.configure",
    "role.create_or_update",
    "role.list",
    "role.set_home",
    "transport.set_home",
    "handoff.to_role",
    "handoff.back",
    "delegate.whisper",
    "delegate.to_peer",
    "delegate.to_external_cognitive_peer",
    "delegate.merge",
    "approval.request_standing",
    // ── Memory ──────────────────────────────────────────────────────────────
    "memory.recall",
    "memory.remember",
    "memory.cultivate",
    "memory.true_up",
    "memory.promote_candidate",
    "memory.fix",
    "memory.status",
    // Previously in NEITHER copy (live bug: the architect profile grants
    // memory.delta_digest but the hotel-side list couldn't route it).
    "memory.explain",
    "memory.delta_digest",
    // ── Routing / rules ─────────────────────────────────────────────────────
    "rule.propose",
    "routing.policy.propose",
    "routing.reflex.set",
    "routing.reflex.get",
    "routing.pipeline.set",
    "routing.pipeline.remove",
    "routing.pipeline.get",
    "router.stats",
    // ── MCP fabric ──────────────────────────────────────────────────────────
    "mcp.provision",
    "mcp.revoke",
    "mcp.status",
    "mcp.connect",
    "mcp.disconnect",
    "mcp.upstreams",
    "mcp.set_credential",
    // ── Integrations ────────────────────────────────────────────────────────
    "integration.bind_http",
    "integration.unbind",
    "integration.list",
    // ── Desktop / observability ─────────────────────────────────────────────
    "desktop.observe",
    "table.add_listener",
    // ── Shell (was hotel-side only) ─────────────────────────────────────────
    "bash.exec",
    // ── Training / ASR / vision (training + asr were hotel-side only) ───────
    "training.list",
    "training.correct",
    "training.export",
    "training.status",
    "asr.setup",
    "asr.status",
    "vision.setup",
    "vision.status",
    // ── Cron ────────────────────────────────────────────────────────────────
    "cron.register",
    "cron.list",
    "cron.enable",
    "cron.disable",
    "cron.remove",
    // ── Mesh steward (heal class, aria-mesh-steward slice 1) ────────────────
    "heal.list",
    "heal.resolve",
    "heal.close_work_item",
    "host.vitals",
    "session.repair_stale",
    "component.restart",
];

/// Returns `true` when `name` executes inside the agent process
/// (`execution_mode == "local_agent"`). Exact-name membership only — see the
/// module doc for the contract.
pub fn is_local_agent_tool(name: &str) -> bool {
    LOCAL_AGENT_TOOLS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_covers_both_historical_lists_and_the_gap_tools() {
        // Previously philote-side only:
        assert!(is_local_agent_tool("approval.request_standing"));
        // Previously hotel-side (aiua ipc) only:
        assert!(is_local_agent_tool("bash.exec"));
        // Previously in NEITHER list despite being granted by profiles:
        assert!(is_local_agent_tool("memory.delta_digest"));
        assert!(is_local_agent_tool("memory.explain"));
        // New mesh-steward surface:
        assert!(is_local_agent_tool("heal.list"));
        assert!(is_local_agent_tool("component.restart"));
    }

    #[test]
    fn non_local_tools_are_rejected() {
        assert!(!is_local_agent_tool("life.observe"));
        assert!(!is_local_agent_tool("workspace.read"));
        assert!(!is_local_agent_tool("mcp:github.search"));
        assert!(!is_local_agent_tool(""));
    }

    #[test]
    fn list_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for name in LOCAL_AGENT_TOOLS {
            assert!(seen.insert(*name), "duplicate entry: {name}");
        }
    }
}
