//! In-memory resource registry for the `demand-derived-materialization` seam.
//!
//! # Transitional note
//!
//! This is Seam 2 of the agent-resource-broker workstream. The registry is wired at boot
//! time to compute the demand-derived guest set from agent identity records, but the existing
//! `materialize_all` path (driven by `materialized_guests`) still runs in parallel. Seam 3
//! will replace that path once the registry is proven stable.
//!
//! # Responsibilities
//!
//! - Accept `ResourceRequest`s from agents (or the boot reconciler) and decide
//!   grant / materializing / deny outcomes
//! - Track tenant counts per resource instance
//! - Track resources-per-agent for lifecycle management
//! - Provide a `boot_reconcile` entry point that replays agent static declarations

use ansible_mesh_core::resources::{
    ResourceDeclaration, ResourceDenied, ResourceGranted, ResourceMaterializing, ResourceRequest,
    ResourceRevoked, ResourceType,
};
use ansible_mesh_core::storage::AgentIdentityRecord;
use std::collections::{HashMap, HashSet};
use tracing::{info, warn};
use uuid::Uuid;

// ──────────────────────────────────────────────────────────────────────────────
// Registry outcome
// ──────────────────────────────────────────────────────────────────────────────

/// The result of submitting a `ResourceRequest` to the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryOutcome {
    /// An existing instance had capacity; agent is now a tenant.
    Granted(ResourceGranted),
    /// No instance exists yet; one is being created. A `ResourceGranted`
    /// will be pushed once the materializer confirms readiness.
    Materializing(ResourceMaterializing),
    /// The request was refused (rights violation, capacity limit, etc.).
    Denied(ResourceDenied),
}

// ──────────────────────────────────────────────────────────────────────────────
// Resource instance tracking
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ResourceInstance {
    instance_id: String,
    resource_type: ResourceType,
    /// Agents currently holding a grant for this instance.
    tenants: HashSet<String>,
    /// True once the materializer has confirmed the instance is live.
    is_ready: bool,
}

impl ResourceInstance {
    fn new(resource_type: ResourceType) -> Self {
        Self {
            instance_id: Uuid::new_v4().to_string(),
            resource_type,
            tenants: HashSet::new(),
            is_ready: false,
        }
    }

    fn tenant_count(&self) -> usize {
        self.tenants.len()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// ResourceRegistry
// ──────────────────────────────────────────────────────────────────────────────

/// In-memory resource registry. Owned by the hotel daemon.
///
/// Not persisted across restarts — rebuilt by `boot_reconcile` on each startup.
pub struct ResourceRegistry {
    /// All known resource instances keyed by instance_id.
    instances: HashMap<String, ResourceInstance>,
    /// Reverse index: ResourceType → instance_ids (allows multi-instance types later).
    by_type: HashMap<ResourceType, Vec<String>>,
    /// Per-agent index: agent_id → instance_ids the agent holds grants for.
    by_agent: HashMap<String, HashSet<String>>,
}

impl ResourceRegistry {
    pub fn new() -> Self {
        Self {
            instances: HashMap::new(),
            by_type: HashMap::new(),
            by_agent: HashMap::new(),
        }
    }

    // ── public API ────────────────────────────────────────────────────────────

    /// Submit a resource request. Returns the immediate outcome.
    ///
    /// Current policy:
    /// - One instance per resource type (singleton model).
    /// - If an instance exists and is ready → Granted.
    /// - If an instance exists but is not yet ready → Materializing.
    /// - If no instance exists → create one (not-ready) → Materializing.
    /// - Rights checks are stubbed permissive (enforced in a later seam).
    pub fn register_request(&mut self, req: ResourceRequest) -> RegistryOutcome {
        if let Some(instance_id) = self.find_instance_for_type(&req.resource_type) {
            let instance = self.instances.get_mut(&instance_id).unwrap();
            instance.tenants.insert(req.agent_id.clone());
            self.by_agent
                .entry(req.agent_id.clone())
                .or_default()
                .insert(instance_id.clone());

            if instance.is_ready {
                return RegistryOutcome::Granted(ResourceGranted {
                    agent_id: req.agent_id,
                    instance_id,
                    resource_type: req.resource_type,
                });
            } else {
                return RegistryOutcome::Materializing(ResourceMaterializing {
                    agent_id: req.agent_id,
                    instance_id,
                    resource_type: req.resource_type,
                });
            }
        }

        // No instance yet — create one.
        let mut instance = ResourceInstance::new(req.resource_type.clone());
        let instance_id = instance.instance_id.clone();
        instance.tenants.insert(req.agent_id.clone());
        self.instances.insert(instance_id.clone(), instance);
        self.by_type
            .entry(req.resource_type.clone())
            .or_default()
            .push(instance_id.clone());
        self.by_agent
            .entry(req.agent_id.clone())
            .or_default()
            .insert(instance_id.clone());

        RegistryOutcome::Materializing(ResourceMaterializing {
            agent_id: req.agent_id,
            instance_id,
            resource_type: req.resource_type,
        })
    }

    /// Mark an instance as ready (called after the materializer confirms spawn).
    /// Returns the list of tenants that should now receive `ResourceGranted`.
    pub fn mark_instance_ready(&mut self, instance_id: &str) -> Vec<String> {
        if let Some(instance) = self.instances.get_mut(instance_id) {
            instance.is_ready = true;
            instance.tenants.iter().cloned().collect()
        } else {
            vec![]
        }
    }

    /// Release a tenant's hold on a resource instance.
    ///
    /// Returns `true` if the instance now has zero tenants (teardown eligible).
    pub fn release_tenant(&mut self, agent_id: &str, instance_id: &str) -> bool {
        if let Some(instance) = self.instances.get_mut(instance_id) {
            instance.tenants.remove(agent_id);
        }
        if let Some(held) = self.by_agent.get_mut(agent_id) {
            held.remove(instance_id);
        }
        self.instances
            .get(instance_id)
            .map(|i| i.tenant_count() == 0)
            .unwrap_or(false)
    }

    /// Remove the resource instance entirely (after teardown).
    pub fn remove_instance(&mut self, instance_id: &str) {
        if let Some(instance) = self.instances.remove(instance_id) {
            if let Some(ids) = self.by_type.get_mut(&instance.resource_type) {
                ids.retain(|id| id != instance_id);
            }
        }
    }

    /// Build a `ResourceRevoked` for every tenant of an instance (for hotel-initiated revocation).
    pub fn revoke_instance(
        &mut self,
        instance_id: &str,
        reason: &str,
    ) -> Vec<ResourceRevoked> {
        let Some(instance) = self.instances.get(instance_id) else {
            return vec![];
        };
        let tenants: Vec<String> = instance.tenants.iter().cloned().collect();
        let resource_type = instance.resource_type.clone();
        let revocations = tenants
            .iter()
            .map(|agent_id| ResourceRevoked {
                agent_id: agent_id.clone(),
                instance_id: instance_id.to_string(),
                resource_type: resource_type.clone(),
                reason: reason.to_string(),
            })
            .collect();
        self.remove_instance(instance_id);
        for agent_id in &tenants {
            if let Some(held) = self.by_agent.get_mut(agent_id) {
                held.remove(instance_id);
            }
        }
        revocations
    }

    /// All instance_ids granted to an agent.
    pub fn grants_for_agent(&self, agent_id: &str) -> Vec<String> {
        self.by_agent
            .get(agent_id)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Current tenant count for an instance.
    pub fn tenant_count(&self, instance_id: &str) -> usize {
        self.instances
            .get(instance_id)
            .map(|i| i.tenant_count())
            .unwrap_or(0)
    }

    /// Count of distinct resource instances in the registry.
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }

    // ── private helpers ───────────────────────────────────────────────────────

    fn find_instance_for_type(&self, resource_type: &ResourceType) -> Option<String> {
        self.by_type
            .get(resource_type)
            .and_then(|ids| ids.first().cloned())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Boot reconciliation
// ──────────────────────────────────────────────────────────────────────────────

/// Summary of what a single agent contributed during boot reconciliation.
#[derive(Debug)]
pub struct AgentReconcileResult {
    pub agent_id: String,
    pub outcomes: Vec<RegistryOutcome>,
}

/// Parse `static_resource_declarations` from an agent's `bundle_json`.
///
/// Returns an empty Vec if the key is absent or malformed (graceful degradation).
pub fn declarations_from_bundle(record: &AgentIdentityRecord) -> Vec<ResourceDeclaration> {
    record
        .bundle_json
        .get("static_resource_declarations")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// Replay all agents' `static_resource_declarations` through `registry` and
/// return per-agent outcomes.
///
/// **Transitional**: this derives the *intended* guest set but does not yet replace
/// the `materialized_guests`-driven `materialize_all` path. That replacement lands
/// in the seam after the registry is proven.
pub fn boot_reconcile(
    registry: &mut ResourceRegistry,
    agents: &[AgentIdentityRecord],
) -> Vec<AgentReconcileResult> {
    info!(
        agents = agents.len(),
        "resource-registry: beginning boot reconciliation"
    );

    let mut results = Vec::with_capacity(agents.len());

    for agent in agents {
        let declarations = declarations_from_bundle(agent);

        if declarations.is_empty() {
            continue;
        }

        info!(
            agent_id = %agent.agent_id,
            declarations = declarations.len(),
            "resource-registry: replaying static declarations"
        );

        let mut outcomes = Vec::with_capacity(declarations.len());
        for decl in declarations {
            let req = ResourceRequest {
                agent_id: agent.agent_id.clone(),
                resource_type: decl.resource_type,
                config_hint: decl.config_hint,
            };
            let outcome = registry.register_request(req);
            match &outcome {
                RegistryOutcome::Granted(g) => info!(
                    agent_id = %agent.agent_id,
                    instance_id = %g.instance_id,
                    resource_type = ?g.resource_type,
                    "resource-registry: boot grant"
                ),
                RegistryOutcome::Materializing(m) => info!(
                    agent_id = %agent.agent_id,
                    instance_id = %m.instance_id,
                    resource_type = ?m.resource_type,
                    "resource-registry: boot materializing"
                ),
                RegistryOutcome::Denied(d) => warn!(
                    agent_id = %agent.agent_id,
                    resource_type = ?d.resource_type,
                    reason = %d.reason,
                    "resource-registry: boot denied"
                ),
            }
            outcomes.push(outcome);
        }

        results.push(AgentReconcileResult {
            agent_id: agent.agent_id.clone(),
            outcomes,
        });
    }

    info!(
        instances = registry.instance_count(),
        "resource-registry: boot reconciliation complete"
    );

    results
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::resources::ResourceType;

    fn make_agent(agent_id: &str, declarations: &[ResourceType]) -> AgentIdentityRecord {
        let decls: Vec<ResourceDeclaration> = declarations
            .iter()
            .map(|rt| ResourceDeclaration {
                resource_type: rt.clone(),
                config_hint: None,
            })
            .collect();
        AgentIdentityRecord {
            agent_id: agent_id.to_string(),
            persona_name: agent_id.to_string(),
            authority_hotel: "test-hotel".to_string(),
            bundle_json: serde_json::json!({
                "static_resource_declarations": decls,
            }),
        }
    }

    fn req(agent_id: &str, resource_type: ResourceType) -> ResourceRequest {
        ResourceRequest {
            agent_id: agent_id.to_string(),
            resource_type,
            config_hint: None,
        }
    }

    // ── registry unit tests ───────────────────────────────────────────────────

    #[test]
    fn first_request_is_materializing() {
        let mut reg = ResourceRegistry::new();
        let outcome = reg.register_request(req("a1", ResourceType::ModelRouter));
        assert!(matches!(outcome, RegistryOutcome::Materializing(_)));
    }

    #[test]
    fn second_agent_shares_not_yet_ready_instance() {
        let mut reg = ResourceRegistry::new();
        let o1 = reg.register_request(req("a1", ResourceType::ModelRouter));
        let o2 = reg.register_request(req("a2", ResourceType::ModelRouter));
        // Both get Materializing because the instance is not yet ready.
        assert!(matches!(o1, RegistryOutcome::Materializing(_)));
        assert!(matches!(o2, RegistryOutcome::Materializing(_)));
        // Both refer to the same instance.
        let id1 = match o1 {
            RegistryOutcome::Materializing(m) => m.instance_id,
            _ => panic!(),
        };
        let id2 = match o2 {
            RegistryOutcome::Materializing(m) => m.instance_id,
            _ => panic!(),
        };
        assert_eq!(id1, id2);
        assert_eq!(reg.tenant_count(&id1), 2);
    }

    #[test]
    fn mark_ready_then_new_tenant_gets_granted() {
        let mut reg = ResourceRegistry::new();
        let o1 = reg.register_request(req("a1", ResourceType::ModelRouter));
        let instance_id = match o1 {
            RegistryOutcome::Materializing(m) => m.instance_id,
            _ => panic!(),
        };
        let notified = reg.mark_instance_ready(&instance_id);
        assert_eq!(notified, vec!["a1"]);

        // New tenant after ready → Granted immediately.
        let o2 = reg.register_request(req("a2", ResourceType::ModelRouter));
        assert!(matches!(o2, RegistryOutcome::Granted(_)));
        assert_eq!(reg.tenant_count(&instance_id), 2);
    }

    #[test]
    fn release_decrements_tenant_count() {
        let mut reg = ResourceRegistry::new();
        let o = reg.register_request(req("a1", ResourceType::ToolRunner));
        let instance_id = match o {
            RegistryOutcome::Materializing(m) => m.instance_id,
            _ => panic!(),
        };
        reg.register_request(req("a2", ResourceType::ToolRunner));
        assert_eq!(reg.tenant_count(&instance_id), 2);

        let zero = reg.release_tenant("a1", &instance_id);
        assert!(!zero);
        assert_eq!(reg.tenant_count(&instance_id), 1);

        let zero = reg.release_tenant("a2", &instance_id);
        assert!(zero, "should report zero tenants after last release");
    }

    #[test]
    fn revoke_clears_instance_and_builds_revocations() {
        let mut reg = ResourceRegistry::new();
        let o = reg.register_request(req("a1", ResourceType::TelegramMembrane));
        reg.register_request(req("a2", ResourceType::TelegramMembrane));
        let instance_id = match o {
            RegistryOutcome::Materializing(m) => m.instance_id,
            _ => panic!(),
        };

        let revocations = reg.revoke_instance(&instance_id, "forced teardown");
        assert_eq!(revocations.len(), 2);
        assert_eq!(reg.instance_count(), 0);
        assert!(reg.grants_for_agent("a1").is_empty());
    }

    #[test]
    fn different_resource_types_get_separate_instances() {
        let mut reg = ResourceRegistry::new();
        reg.register_request(req("a1", ResourceType::ModelRouter));
        reg.register_request(req("a1", ResourceType::ToolRunner));
        assert_eq!(reg.instance_count(), 2);
        assert_eq!(reg.grants_for_agent("a1").len(), 2);
    }

    // ── boot_reconcile tests ──────────────────────────────────────────────────

    #[test]
    fn boot_reconcile_processes_agent_declarations() {
        let mut reg = ResourceRegistry::new();
        let agents = vec![
            make_agent("aria", &[ResourceType::ModelRouter, ResourceType::ToolRunner]),
            make_agent("bjork", &[ResourceType::ModelRouter]),
        ];
        let results = boot_reconcile(&mut reg, &agents);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].outcomes.len(), 2); // aria: model-router + tool-runner
        assert_eq!(results[1].outcomes.len(), 1); // bjork: model-router (shared instance)

        // Only 2 distinct instances (model-router shared, tool-runner only aria).
        assert_eq!(reg.instance_count(), 2);
        // model-router has 2 tenants.
        let mr_instance = reg
            .by_type
            .get(&ResourceType::ModelRouter)
            .and_then(|v| v.first())
            .unwrap();
        assert_eq!(reg.tenant_count(mr_instance), 2);
    }

    #[test]
    fn boot_reconcile_skips_agents_with_no_declarations() {
        let mut reg = ResourceRegistry::new();
        let agents = vec![AgentIdentityRecord {
            agent_id: "empty-agent".to_string(),
            persona_name: "empty".to_string(),
            authority_hotel: "h".to_string(),
            bundle_json: serde_json::json!({}),
        }];
        let results = boot_reconcile(&mut reg, &agents);
        assert!(results.is_empty());
        assert_eq!(reg.instance_count(), 0);
    }

    #[test]
    fn declarations_from_bundle_gracefully_handles_missing_key() {
        let record = AgentIdentityRecord {
            agent_id: "x".to_string(),
            persona_name: "x".to_string(),
            authority_hotel: "h".to_string(),
            bundle_json: serde_json::json!({"other": "stuff"}),
        };
        assert!(declarations_from_bundle(&record).is_empty());
    }

    #[test]
    fn declarations_from_bundle_parses_correctly() {
        let record = make_agent("x", &[ResourceType::AgentGraph, ResourceType::ModelRouter]);
        let decls = declarations_from_bundle(&record);
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[0].resource_type, ResourceType::AgentGraph);
        assert_eq!(decls[1].resource_type, ResourceType::ModelRouter);
    }
}
