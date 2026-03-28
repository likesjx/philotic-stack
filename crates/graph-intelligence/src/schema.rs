use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
    Agent,
    Session,
    Decision,
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
            Self::Agent => "agent",
            Self::Session => "session",
            Self::Decision => "decision",
        }
    }

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
            "agent" => Some(Self::Agent),
            "session" => Some(Self::Session),
            "decision" => Some(Self::Decision),
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
        }
    }

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
