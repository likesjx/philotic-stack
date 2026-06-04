// V001: Life Graph OS — Initial Schema
// Target:  Memgraph 3.10.1+
// Host:    vps-jane Tailscale 100.64.212.8:7687
// Database: default (single-database deployment; partition by label)
//
// Apply each statement individually — Bolt does not support multi-statement batches.
// After applying run: SHOW INDEX INFO; and SHOW VECTOR INDEX INFO;
//
// See: docs/architecture/life-graph/LIFE_GRAPH_SCHEMA.md

// ============================================================
// Uniqueness Constraints
// ============================================================

CREATE CONSTRAINT ON (n:Person) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:Role) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:Goal) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:System) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:Habit) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:Project) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:Commitment) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:OpenLoop) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:NextAction) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:Routine) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:Decision) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:Preference) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:Value) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:Concern) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:Event) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:Signal) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:GrowthHypothesis) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:GrowthExperiment) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:DriftFinding) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:CapabilityPatch) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:SkillPatch) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:ToolPatch) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:SchemaPatch) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:AttentionPatch) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:SystemPatch) ASSERT n.id IS UNIQUE;

// ============================================================
// Property Indexes — validation_state (common filter)
// ============================================================

CREATE INDEX ON :Goal(validation_state);
CREATE INDEX ON :Commitment(validation_state);
CREATE INDEX ON :OpenLoop(validation_state);
CREATE INDEX ON :Habit(validation_state);
CREATE INDEX ON :Signal(validation_state);

// ============================================================
// Property Indexes — observed_at (recency queries)
// ============================================================

CREATE INDEX ON :Event(observed_at);
CREATE INDEX ON :Signal(observed_at);
CREATE INDEX ON :Commitment(observed_at);
CREATE INDEX ON :OpenLoop(observed_at);

// ============================================================
// Property Indexes — source_membrane (agent-source filtering)
// ============================================================

CREATE INDEX ON :Signal(source_membrane);
CREATE INDEX ON :Event(source_membrane);

// ============================================================
// Property Indexes — name/title lookup
// ============================================================

CREATE INDEX ON :Person(name);
CREATE INDEX ON :Role(name);
CREATE INDEX ON :Goal(title);
CREATE INDEX ON :Project(title);
CREATE INDEX ON :Habit(title);
CREATE INDEX ON :OpenLoop(title);
CREATE INDEX ON :Commitment(title);

// ============================================================
// Vector Indexes — life_event_semantic
// dimension: 1536  metric: cos (cosine)  capacity: 10000
// ============================================================

CREATE VECTOR INDEX life_event_semantic__Event ON :Event(embedding) WITH CONFIG {"dimension": 1536, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX life_event_semantic__Signal ON :Signal(embedding) WITH CONFIG {"dimension": 1536, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX life_event_semantic__OpenLoop ON :OpenLoop(embedding) WITH CONFIG {"dimension": 1536, "capacity": 10000, "metric": "cos"};

// ============================================================
// Vector Indexes — goal_system_semantic
// dimension: 1536  metric: cos  capacity: 10000
// ============================================================

CREATE VECTOR INDEX goal_system_semantic__Goal ON :Goal(embedding) WITH CONFIG {"dimension": 1536, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX goal_system_semantic__System ON :System(embedding) WITH CONFIG {"dimension": 1536, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX goal_system_semantic__Habit ON :Habit(embedding) WITH CONFIG {"dimension": 1536, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX goal_system_semantic__Project ON :Project(embedding) WITH CONFIG {"dimension": 1536, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX goal_system_semantic__Routine ON :Routine(embedding) WITH CONFIG {"dimension": 1536, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX goal_system_semantic__NextAction ON :NextAction(embedding) WITH CONFIG {"dimension": 1536, "capacity": 10000, "metric": "cos"};

// ============================================================
// Vector Indexes — skill_tool_semantic
// dimension: 1536  metric: cos  capacity: 5000
// ============================================================

CREATE VECTOR INDEX skill_tool_semantic__GrowthHypothesis ON :GrowthHypothesis(embedding) WITH CONFIG {"dimension": 1536, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX skill_tool_semantic__GrowthExperiment ON :GrowthExperiment(embedding) WITH CONFIG {"dimension": 1536, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX skill_tool_semantic__DriftFinding ON :DriftFinding(embedding) WITH CONFIG {"dimension": 1536, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX skill_tool_semantic__CapabilityPatch ON :CapabilityPatch(embedding) WITH CONFIG {"dimension": 1536, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX skill_tool_semantic__SkillPatch ON :SkillPatch(embedding) WITH CONFIG {"dimension": 1536, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX skill_tool_semantic__ToolPatch ON :ToolPatch(embedding) WITH CONFIG {"dimension": 1536, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX skill_tool_semantic__SchemaPatch ON :SchemaPatch(embedding) WITH CONFIG {"dimension": 1536, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX skill_tool_semantic__AttentionPatch ON :AttentionPatch(embedding) WITH CONFIG {"dimension": 1536, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX skill_tool_semantic__SystemPatch ON :SystemPatch(embedding) WITH CONFIG {"dimension": 1536, "capacity": 5000, "metric": "cos"};

// ============================================================
// Vector Indexes — role_person_semantic
// dimension: 1536  metric: cos  capacity: 5000
// ============================================================

CREATE VECTOR INDEX role_person_semantic__Role ON :Role(embedding) WITH CONFIG {"dimension": 1536, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX role_person_semantic__Person ON :Person(embedding) WITH CONFIG {"dimension": 1536, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX role_person_semantic__Value ON :Value(embedding) WITH CONFIG {"dimension": 1536, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX role_person_semantic__Preference ON :Preference(embedding) WITH CONFIG {"dimension": 1536, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX role_person_semantic__Concern ON :Concern(embedding) WITH CONFIG {"dimension": 1536, "capacity": 5000, "metric": "cos"};

// ============================================================
// Vector Indexes — memory_bridge_semantic
// dimension: 1536  metric: cos  capacity: 5000
// ============================================================

CREATE VECTOR INDEX memory_bridge_semantic__Commitment ON :Commitment(embedding) WITH CONFIG {"dimension": 1536, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX memory_bridge_semantic__Decision ON :Decision(embedding) WITH CONFIG {"dimension": 1536, "capacity": 5000, "metric": "cos"};
