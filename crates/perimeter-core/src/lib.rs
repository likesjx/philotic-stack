pub mod classifier;
pub mod egress;
pub mod fence;
pub mod service;

pub use ansible_mesh_core::{ExposureTier, ListenerProfile, PerimeterSnapshot};
pub use classifier::{classify_bind_addr, ClassifyResult};
pub use egress::{
    decide_egress_placement, evaluate_egress_policy, host_from_url, EgressCredential,
    EgressDecision, EgressDefaultAction, EgressFallback, EgressGateway, EgressPlacementDecision,
    EgressPlacementPolicy, EgressPolicy, EgressRequest, EgressTrafficClass,
};
pub use fence::{check_ingress, IngressDecision};
pub use service::{PerimeterEvent, PerimeterService};
