// V002: Life Graph OS — Attention Steward Schema
// Target:  Memgraph 3.10.1+
// Host:    vps-jane Tailscale 100.64.212.8:7687
// Depends: V001__life_graph_schema.cypher
//
// Adds: StewardshipInstruction node type, property indexes,
//       and three relationship type annotations (GOVERNS, EVALUATES, OUTCOME_OF).
//
// Apply each statement individually — Bolt does not support multi-statement batches.
// After applying run: SHOW INDEX INFO; to confirm.
//
// See: docs/architecture/life-graph/ATTENTION_STEWARD.md

// ============================================================
// Uniqueness Constraint
// ============================================================

CREATE CONSTRAINT ON (n:StewardshipInstruction) ASSERT n.id IS UNIQUE;

// ============================================================
// Property Indexes
// ============================================================

CREATE INDEX ON :StewardshipInstruction(status);
CREATE INDEX ON :StewardshipInstruction(owner);
CREATE INDEX ON :StewardshipInstruction(recommended_action);
CREATE INDEX ON :StewardshipInstruction(scope);
