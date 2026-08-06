use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// C4 Model architecture levels for drill-down navigation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum C4Level {
    Context,   // C1: Systems and external actors
    Container, // C2: Crates/runtimes/processes
    Component, // C3: Modules/layers within crates
    Code,      // C4: Types, functions, traits
}

impl C4Level {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Context => "c1_context",
            Self::Container => "c2_container",
            Self::Component => "c3_component",
            Self::Code => "c4_code",
        }
    }

    /// Deliberately NOT `std::str::FromStr`: this is a lenient parser over
    /// values written by hand in docs and frontmatter, where an unrecognised
    /// string is simply "not one of these" rather than an error. `FromStr`
    /// would force a `Result` and an error type every caller would discard.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "c1_context" | "context" => Some(Self::Context),
            "c2_container" | "container" => Some(Self::Container),
            "c3_component" | "component" => Some(Self::Component),
            "c4_code" | "code" => Some(Self::Code),
            _ => None,
        }
    }

    /// Map NodeKind to default C4 level
    pub fn from_node_kind(kind: NodeKind) -> Option<Self> {
        match kind {
            // C1: Context level
            NodeKind::Domain
            | NodeKind::Agent
            | NodeKind::Skill
            | NodeKind::Harness
            | NodeKind::HarnessSkill
            | NodeKind::HarnessProfile
            | NodeKind::CanonicalProfile
            | NodeKind::CanonicalWorkflow
            // Profile projection tracking
            | NodeKind::ProfileDefinition
            | NodeKind::RoleCharter
            | NodeKind::AgentWorkFocus => Some(Self::Context),

            // C2: Container level
            NodeKind::Crate | NodeKind::Worktree | NodeKind::Workstream | NodeKind::Component => {
                Some(Self::Container)
            }

            // C3: Component level
            NodeKind::Module | NodeKind::Proposal | NodeKind::Seam | NodeKind::Slice => {
                Some(Self::Component)
            }

            // C4: Code level
            NodeKind::Type
            | NodeKind::Function
            | NodeKind::ImplBlock
            | NodeKind::Test
            | NodeKind::Commit => Some(Self::Code),

            // Process/metadata
            NodeKind::Task
            | NodeKind::Decision
            | NodeKind::HarnessProjection
            | NodeKind::HarnessObservation
            | NodeKind::HarnessDrift
            | NodeKind::HarnessRollout => Some(Self::Component),
            NodeKind::Branch => Some(Self::Container),
            NodeKind::Session => Some(Self::Context),
            NodeKind::Document => Some(Self::Context),
            // Verification tracking - these are process nodes, not code
            NodeKind::TestRun | NodeKind::SmokeRun | NodeKind::UatRun => None,
            // SVER - process documentation
            NodeKind::Sver => Some(Self::Context),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub name: String,
    pub properties: serde_json::Value,
    pub file_path: Option<String>,
    pub worktree: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    // Embedding fields
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub embedding: Option<Vec<f32>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub embedding_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub embedding_dims: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub embedding_updated: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub embedding_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Proposal,
    Seam,
    Task,
    Slice,
    Domain,
    Crate,
    Module,
    Type,
    Function,
    ImplBlock,
    Commit,
    Branch,
    Workstream,
    Worktree,
    Test,
    Component,
    Skill,
    Harness,
    HarnessSkill,
    HarnessProfile,
    CanonicalProfile,
    CanonicalWorkflow,
    HarnessProjection,
    HarnessObservation,
    HarnessDrift,
    HarnessRollout,
    // Profile projection tracking
    ProfileDefinition,
    RoleCharter,
    Agent,
    AgentWorkFocus,
    Session,
    Decision,
    // Process documents (AGENTS.md, guides, lifecycle docs)
    Document,
    // Verification tracking nodes
    TestRun,
    SmokeRun,
    UatRun,
    // SVER (Software Versioning and Evaluation Record)
    Sver,
}

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Proposal => "proposal",
            Self::Seam => "seam",
            Self::Task => "task",
            Self::Slice => "slice",
            Self::Domain => "domain",
            Self::Crate => "crate",
            Self::Module => "module",
            Self::Type => "type",
            Self::Function => "function",
            Self::ImplBlock => "impl_block",
            Self::Commit => "commit",
            Self::Branch => "branch",
            Self::Workstream => "workstream",
            Self::Worktree => "worktree",
            Self::Test => "test",
            Self::Component => "component",
            Self::Skill => "skill",
            Self::Harness => "harness",
            Self::HarnessSkill => "harness_skill",
            Self::HarnessProfile => "harness_profile",
            Self::CanonicalProfile => "canonical_profile",
            Self::CanonicalWorkflow => "canonical_workflow",
            Self::HarnessProjection => "harness_projection",
            Self::HarnessObservation => "harness_observation",
            Self::HarnessDrift => "harness_drift",
            Self::HarnessRollout => "harness_rollout",
            // Profile projection tracking
            Self::ProfileDefinition => "profile_definition",
            Self::RoleCharter => "role_charter",
            Self::Agent => "agent",
            Self::AgentWorkFocus => "agent_work_focus",
            Self::Session => "session",
            Self::Decision => "decision",
            Self::Document => "document",
            // Verification tracking
            Self::TestRun => "test_run",
            Self::SmokeRun => "smoke_run",
            Self::UatRun => "uat_run",
            // SVER
            Self::Sver => "sver",
        }
    }

    /// Deliberately NOT `std::str::FromStr`: this is a lenient parser over
    /// values written by hand in docs and frontmatter, where an unrecognised
    /// string is simply "not one of these" rather than an error. `FromStr`
    /// would force a `Result` and an error type every caller would discard.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "proposal" => Some(Self::Proposal),
            "seam" => Some(Self::Seam),
            "task" => Some(Self::Task),
            "slice" => Some(Self::Slice),
            "domain" => Some(Self::Domain),
            "crate" => Some(Self::Crate),
            "module" => Some(Self::Module),
            "type" => Some(Self::Type),
            "function" => Some(Self::Function),
            "impl_block" => Some(Self::ImplBlock),
            "commit" => Some(Self::Commit),
            "branch" => Some(Self::Branch),
            "workstream" => Some(Self::Workstream),
            "worktree" => Some(Self::Worktree),
            "test" => Some(Self::Test),
            "component" => Some(Self::Component),
            "skill" => Some(Self::Skill),
            "harness" => Some(Self::Harness),
            "harness_skill" => Some(Self::HarnessSkill),
            "harness_profile" => Some(Self::HarnessProfile),
            "canonical_profile" => Some(Self::CanonicalProfile),
            "canonical_workflow" => Some(Self::CanonicalWorkflow),
            "harness_projection" => Some(Self::HarnessProjection),
            "harness_observation" => Some(Self::HarnessObservation),
            "harness_drift" => Some(Self::HarnessDrift),
            "harness_rollout" => Some(Self::HarnessRollout),
            // Profile projection tracking
            "profile_definition" => Some(Self::ProfileDefinition),
            "role_charter" => Some(Self::RoleCharter),
            "agent" => Some(Self::Agent),
            "agent_work_focus" => Some(Self::AgentWorkFocus),
            "session" => Some(Self::Session),
            "decision" => Some(Self::Decision),
            "document" => Some(Self::Document),
            // Verification tracking
            "test_run" => Some(Self::TestRun),
            "smoke_run" => Some(Self::SmokeRun),
            "uat_run" => Some(Self::UatRun),
            // SVER
            "sver" => Some(Self::Sver),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub source_id: String,
    pub target_id: String,
    pub relation: EdgeRelation,
    pub properties: serde_json::Value,
    pub worktree: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeRelation {
    Implements,
    ImplementedBy,
    DependsOn,
    Contains,
    Governs,
    References,
    Tests,
    Blocks,
    DecidedBy,
    AppliesTo,
    Imports,
    TraitImpl,
    Overlaps,
    /// Function-level call edge, imported from the graphify tree-sitter bridge
    Calls,
    // Verification tracking
    TestedBy,
    SmokedBy,
    UatBy,
    Validates,
    RequiresVerification,
    // SVER relationships
    UsesSver,
    // Session tracking
    WorkingOn,
    Created,
    PartOf,
    // Workstream tracking
    Drives,
}

impl EdgeRelation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Implements => "implements",
            Self::ImplementedBy => "implemented_by",
            Self::DependsOn => "depends_on",
            Self::Contains => "contains",
            Self::Governs => "governs",
            Self::References => "references",
            Self::Tests => "tests",
            Self::Blocks => "blocks",
            Self::DecidedBy => "decided_by",
            Self::AppliesTo => "applies_to",
            Self::Imports => "imports",
            Self::TraitImpl => "trait_impl",
            Self::Overlaps => "overlaps",
            Self::Calls => "calls",
            // Verification tracking
            Self::TestedBy => "tested_by",
            Self::SmokedBy => "smoked_by",
            Self::UatBy => "uat_by",
            Self::Validates => "validates",
            Self::RequiresVerification => "requires_verification",
            // SVER
            Self::UsesSver => "uses_sver",
            // Session tracking
            Self::WorkingOn => "working_on",
            Self::Created => "created",
            Self::PartOf => "part_of",
            Self::Drives => "drives",
        }
    }

    /// Deliberately NOT `std::str::FromStr`: this is a lenient parser over
    /// values written by hand in docs and frontmatter, where an unrecognised
    /// string is simply "not one of these" rather than an error. `FromStr`
    /// would force a `Result` and an error type every caller would discard.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "implements" => Some(Self::Implements),
            "implemented_by" => Some(Self::ImplementedBy),
            "depends_on" => Some(Self::DependsOn),
            "contains" => Some(Self::Contains),
            "governs" => Some(Self::Governs),
            "references" => Some(Self::References),
            "tests" => Some(Self::Tests),
            "blocks" => Some(Self::Blocks),
            "decided_by" => Some(Self::DecidedBy),
            "applies_to" => Some(Self::AppliesTo),
            "imports" => Some(Self::Imports),
            "trait_impl" => Some(Self::TraitImpl),
            "overlaps" => Some(Self::Overlaps),
            "calls" => Some(Self::Calls),
            // Verification tracking
            "tested_by" => Some(Self::TestedBy),
            "smoked_by" => Some(Self::SmokedBy),
            "uat_by" => Some(Self::UatBy),
            "validates" => Some(Self::Validates),
            "requires_verification" => Some(Self::RequiresVerification),
            // SVER
            "uses_sver" => Some(Self::UsesSver),
            // Session tracking
            "working_on" => Some(Self::WorkingOn),
            "created" => Some(Self::Created),
            "part_of" => Some(Self::PartOf),
            "drives" => Some(Self::Drives),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snippet {
    pub id: String,
    pub node_id: String,
    pub kind: SnippetKind,
    pub signature: String,
    pub doc_comment: Option<String>,
    pub body: Option<String>,
    pub body_hash: String,
    pub file_path: String,
    pub line_start: u32,
    pub line_end: u32,
    pub language: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnippetKind {
    Function,
    Struct,
    Trait,
    Enum,
    ImplBlock,
    Test,
    Const,
    Static,
    TypeAlias,
    Macro,
}

impl SnippetKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Struct => "struct",
            Self::Trait => "trait",
            Self::Enum => "enum",
            Self::ImplBlock => "impl_block",
            Self::Test => "test",
            Self::Const => "const",
            Self::Static => "static",
            Self::TypeAlias => "type_alias",
            Self::Macro => "macro",
        }
    }

    /// Deliberately NOT `std::str::FromStr`: this is a lenient parser over
    /// values written by hand in docs and frontmatter, where an unrecognised
    /// string is simply "not one of these" rather than an error. `FromStr`
    /// would force a `Result` and an error type every caller would discard.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "function" => Some(Self::Function),
            "struct" => Some(Self::Struct),
            "trait" => Some(Self::Trait),
            "enum" => Some(Self::Enum),
            "impl_block" => Some(Self::ImplBlock),
            "test" => Some(Self::Test),
            "const" => Some(Self::Const),
            "static" => Some(Self::Static),
            "type_alias" => Some(Self::TypeAlias),
            "macro" => Some(Self::Macro),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mutation {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub agent: Option<String>,
    pub session: Option<String>,
    pub action: String,
    pub target_node: Option<String>,
    pub from_value: Option<String>,
    pub to_value: Option<String>,
    pub reason: Option<String>,
    pub details: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanSnapshot {
    pub id: String,
    pub scan_time: DateTime<Utc>,
    pub commit_sha: Option<String>,
    pub worktree: Option<String>,
    pub metrics: serde_json::Value,
}

// ── Embedding Utilities ──

/// Serialize a Vec<f32> to bytes for SQLite BLOB storage
pub fn serialize_embedding(vec: &[f32]) -> Vec<u8> {
    vec.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Deserialize bytes to Vec<f32>
pub fn deserialize_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Compute cosine similarity between two vectors
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a * norm_b)
}

/// Compute dot product similarity (faster, not normalized)
pub fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Check if a node kind should have embeddings generated
pub fn should_embed(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Proposal
            | NodeKind::Seam
            | NodeKind::Task
            | NodeKind::Function
            | NodeKind::Type
            | NodeKind::Module
            | NodeKind::Test
            | NodeKind::Harness
            | NodeKind::HarnessSkill
            | NodeKind::HarnessProfile
            | NodeKind::ProfileDefinition
            | NodeKind::RoleCharter
            | NodeKind::AgentWorkFocus
    )
}
