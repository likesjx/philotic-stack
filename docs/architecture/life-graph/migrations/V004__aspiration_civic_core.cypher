// V004: Life Graph OS — Aspiration node type + civic-core ownership indexes
// Target:  Memgraph 3.10.1+
// Host:    vps-jane Tailscale 100.64.212.8:7687
// Depends: V001__life_graph_schema.cypher, V003__vector_index_768d.cypher
//
// Adds Aspiration — "an identity you're growing into" — as a first-class civic node type.
// Aspiration is the pivot of the civic core: shaped from above by Role and from below by Goal.
// It lives in the role_person_semantic space (identity), alongside Role/Person/Value.
//
// Also indexes source_membrane on the civic node types. Agent-ownership of a civic node
// (e.g. "open loops owned by agent:beacon") is expressed via source_membrane, not an edge —
// so these indexes back the chief-of-staff recall/brief queries.
//
// Part of the Beacon civic-core slice (proposal:beacon-civic-core).
// Apply each statement individually — Bolt does not support multi-statement batches.
// After applying, run: SHOW VECTOR INDEX INFO; to confirm role_person_semantic__Aspiration at 768.

// ============================================================
// Uniqueness
// ============================================================

CREATE CONSTRAINT ON (n:Aspiration) ASSERT n.id IS UNIQUE;

// ============================================================
// Property Indexes — Aspiration (fields written by life.observe)
// ============================================================

CREATE INDEX ON :Aspiration(validation_state);
CREATE INDEX ON :Aspiration(observed_at);

// ============================================================
// Property Indexes — source_membrane (civic-core agent-ownership filter)
// ============================================================

CREATE INDEX ON :Aspiration(source_membrane);
CREATE INDEX ON :Role(source_membrane);
CREATE INDEX ON :Goal(source_membrane);
CREATE INDEX ON :OpenLoop(source_membrane);

// ============================================================
// Vector Index — role_person_semantic (identity space), 768d
// ============================================================

CREATE VECTOR INDEX role_person_semantic__Aspiration ON :Aspiration(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
