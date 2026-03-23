//! First-class IPC message types for the agent-resource-broker seam.
//!
//! These types model the full lifecycle of a resource request between an agent
//! (guest) and the hotel: request → grant/deny/materializing, followed by
//! release (agent-initiated) or revocation (hotel-initiated).
//!
//! All types are purely additive at this stage. No callers exist yet; they
//! will be wired by the `demand-derived-materialization` seam.

use serde::{Deserialize, Serialize};

/// The set of resource types the broker can manage.
///
/// Extend this enum as new resource types are registered with the broker.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceType {
    /// External protocol gateway (Telegram polling lease holder).
    TelegramMembrane,
    /// Model provider routing guest (Gemini / ElevenLabs etc.).
    ModelRouter,
    /// Tool execution guest (bash, workspace, memory tool surface).
    ToolRunner,
    /// Per-agent graph store and cognitive state tool-runner.
    AgentGraph,
}

/// A static resource declaration stored on an agent identity record.
///
/// These are replayed as synthetic `ResourceRequest`s by the hotel boot reconciliation
/// loop to derive the guest set on startup. Agents do not need to include their own
/// `agent_id` here — it is always the owning agent's ID.
///
/// Stored under `bundle_json["static_resource_declarations"]` on `AgentIdentityRecord`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceDeclaration {
    /// The type of resource this agent always needs when active.
    pub resource_type: ResourceType,
    /// Optional opaque config hint passed to the materializer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_hint: Option<String>,
}

/// Agent → hotel: declare a need for a resource.
///
/// The hotel broker will either grant immediately (`ResourceGranted`),
/// begin async materialisation (`ResourceMaterializing`), or deny (`ResourceDenied`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRequest {
    /// The agent issuing this request.
    pub agent_id: String,
    /// The type of resource being requested.
    pub resource_type: ResourceType,
    /// Optional opaque hint passed to the materializer (e.g. preferred port,
    /// config profile name). Ignored when a shared instance already exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_hint: Option<String>,
}

/// Hotel → agent: the requested resource is live and the binding is active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceGranted {
    /// The agent this grant is addressed to.
    pub agent_id: String,
    /// Opaque stable identifier for the resource instance.
    pub instance_id: String,
    /// The type of the granted resource (mirrors the original request).
    pub resource_type: ResourceType,
}

/// Hotel → agent: the request was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceDenied {
    /// The agent whose request was refused.
    pub agent_id: String,
    /// The type that was requested.
    pub resource_type: ResourceType,
    /// Human-readable reason for the denial (rights violation, capacity limit, etc.).
    pub reason: String,
}

/// Hotel → agent: the requested resource is being materialised asynchronously.
///
/// The hotel will deliver a `ResourceGranted` once the instance is ready.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceMaterializing {
    /// The agent whose request is being fulfilled.
    pub agent_id: String,
    /// Stable identifier reserved for this instance (usable for correlation).
    pub instance_id: String,
    /// The type being materialised.
    pub resource_type: ResourceType,
}

/// Agent → hotel: the agent no longer needs this resource instance.
///
/// The hotel decrements the tenant count; if it reaches zero the instance
/// may be torn down according to its teardown policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceReleased {
    /// The agent releasing the resource.
    pub agent_id: String,
    /// The instance being released.
    pub instance_id: String,
    /// The type of the released resource.
    pub resource_type: ResourceType,
}

/// Hotel → agent: the hotel is withdrawing a previously issued grant.
///
/// Agents must stop using the resource immediately. No agent consent is
/// required; the hotel enforces revocation unilaterally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRevoked {
    /// The agent whose grant is being revoked.
    pub agent_id: String,
    /// The instance being revoked.
    pub instance_id: String,
    /// The type of the revoked resource.
    pub resource_type: ResourceType,
    /// Human-readable reason (rights change, forced teardown, operator action, etc.).
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: Serialize + for<'de> Deserialize<'de> + std::fmt::Debug + PartialEq>(v: &T) {
        let json = serde_json::to_string(v).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        // Re-serialize to compare structural equality (PartialEq not always derived on enums)
        let json2 = serde_json::to_string(&back).expect("re-serialize");
        assert_eq!(json, json2, "round-trip mismatch for {:?}", v);
    }

    #[test]
    fn resource_type_round_trip() {
        for rt in [
            ResourceType::TelegramMembrane,
            ResourceType::ModelRouter,
            ResourceType::ToolRunner,
            ResourceType::AgentGraph,
        ] {
            round_trip(&rt);
        }
    }

    #[test]
    fn resource_request_round_trip() {
        round_trip(&ResourceRequest {
            agent_id: "agent-1".into(),
            resource_type: ResourceType::ModelRouter,
            config_hint: None,
        });
        round_trip(&ResourceRequest {
            agent_id: "agent-2".into(),
            resource_type: ResourceType::TelegramMembrane,
            config_hint: Some("profile-a".into()),
        });
    }

    #[test]
    fn resource_granted_round_trip() {
        round_trip(&ResourceGranted {
            agent_id: "agent-1".into(),
            instance_id: "inst-42".into(),
            resource_type: ResourceType::ToolRunner,
        });
    }

    #[test]
    fn resource_denied_round_trip() {
        round_trip(&ResourceDenied {
            agent_id: "agent-1".into(),
            resource_type: ResourceType::TelegramMembrane,
            reason: "insufficient rights".into(),
        });
    }

    #[test]
    fn resource_materializing_round_trip() {
        round_trip(&ResourceMaterializing {
            agent_id: "agent-1".into(),
            instance_id: "inst-99".into(),
            resource_type: ResourceType::AgentGraph,
        });
    }

    #[test]
    fn resource_released_round_trip() {
        round_trip(&ResourceReleased {
            agent_id: "agent-1".into(),
            instance_id: "inst-42".into(),
            resource_type: ResourceType::ModelRouter,
        });
    }

    #[test]
    fn resource_revoked_round_trip() {
        round_trip(&ResourceRevoked {
            agent_id: "agent-1".into(),
            instance_id: "inst-42".into(),
            resource_type: ResourceType::ModelRouter,
            reason: "operator teardown".into(),
        });
    }

    #[test]
    fn resource_type_serde_names() {
        assert_eq!(
            serde_json::to_string(&ResourceType::TelegramMembrane).unwrap(),
            r#""telegram_membrane""#
        );
        assert_eq!(
            serde_json::to_string(&ResourceType::ModelRouter).unwrap(),
            r#""model_router""#
        );
        assert_eq!(
            serde_json::to_string(&ResourceType::ToolRunner).unwrap(),
            r#""tool_runner""#
        );
        assert_eq!(
            serde_json::to_string(&ResourceType::AgentGraph).unwrap(),
            r#""agent_graph""#
        );
    }
}
