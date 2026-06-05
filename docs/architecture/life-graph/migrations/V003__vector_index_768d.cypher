// V003: Life Graph OS — Vector indexes migrated from 1536d to 768d
// Target:  Memgraph 3.10.1+
// Host:    vps-jane Tailscale 100.64.212.8:7687
// Depends: V001__life_graph_schema.cypher
//
// Canonical embedding model: Xenova/all-mpnet-base-v2 (768d, cosine)
// Model is fine-tunable via sentence-transformers on HuggingFace.
// embedding_model_gen = "{repo}@{sha8}" — bump triggers reindex via paracrine signal.
//
// Apply each statement individually — Bolt does not support multi-statement batches.
// After applying run: SHOW VECTOR INDEX INFO; to confirm dimension = 768.

// ============================================================
// Drop 1536d indexes
// ============================================================

DROP VECTOR INDEX life_event_semantic__Event;
DROP VECTOR INDEX life_event_semantic__Signal;
DROP VECTOR INDEX life_event_semantic__OpenLoop;

DROP VECTOR INDEX goal_system_semantic__Goal;
DROP VECTOR INDEX goal_system_semantic__System;
DROP VECTOR INDEX goal_system_semantic__Habit;
DROP VECTOR INDEX goal_system_semantic__Project;
DROP VECTOR INDEX goal_system_semantic__Routine;
DROP VECTOR INDEX goal_system_semantic__NextAction;

DROP VECTOR INDEX skill_tool_semantic__GrowthHypothesis;
DROP VECTOR INDEX skill_tool_semantic__GrowthExperiment;
DROP VECTOR INDEX skill_tool_semantic__DriftFinding;
DROP VECTOR INDEX skill_tool_semantic__CapabilityPatch;
DROP VECTOR INDEX skill_tool_semantic__SkillPatch;
DROP VECTOR INDEX skill_tool_semantic__ToolPatch;
DROP VECTOR INDEX skill_tool_semantic__SchemaPatch;
DROP VECTOR INDEX skill_tool_semantic__AttentionPatch;
DROP VECTOR INDEX skill_tool_semantic__SystemPatch;

DROP VECTOR INDEX role_person_semantic__Role;
DROP VECTOR INDEX role_person_semantic__Person;
DROP VECTOR INDEX role_person_semantic__Value;
DROP VECTOR INDEX role_person_semantic__Preference;
DROP VECTOR INDEX role_person_semantic__Concern;

DROP VECTOR INDEX memory_bridge_semantic__Commitment;
DROP VECTOR INDEX memory_bridge_semantic__Decision;

// ============================================================
// Recreate at 768d — life_event_semantic
// ============================================================

CREATE VECTOR INDEX life_event_semantic__Event ON :Event(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX life_event_semantic__Signal ON :Signal(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX life_event_semantic__OpenLoop ON :OpenLoop(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};

// ============================================================
// goal_system_semantic
// ============================================================

CREATE VECTOR INDEX goal_system_semantic__Goal ON :Goal(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX goal_system_semantic__System ON :System(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX goal_system_semantic__Habit ON :Habit(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX goal_system_semantic__Project ON :Project(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX goal_system_semantic__Routine ON :Routine(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX goal_system_semantic__NextAction ON :NextAction(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};

// ============================================================
// skill_tool_semantic
// ============================================================

CREATE VECTOR INDEX skill_tool_semantic__GrowthHypothesis ON :GrowthHypothesis(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX skill_tool_semantic__GrowthExperiment ON :GrowthExperiment(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX skill_tool_semantic__DriftFinding ON :DriftFinding(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX skill_tool_semantic__CapabilityPatch ON :CapabilityPatch(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX skill_tool_semantic__SkillPatch ON :SkillPatch(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX skill_tool_semantic__ToolPatch ON :ToolPatch(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX skill_tool_semantic__SchemaPatch ON :SchemaPatch(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX skill_tool_semantic__AttentionPatch ON :AttentionPatch(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX skill_tool_semantic__SystemPatch ON :SystemPatch(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};

// ============================================================
// role_person_semantic
// ============================================================

CREATE VECTOR INDEX role_person_semantic__Role ON :Role(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX role_person_semantic__Person ON :Person(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX role_person_semantic__Value ON :Value(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX role_person_semantic__Preference ON :Preference(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX role_person_semantic__Concern ON :Concern(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};

// ============================================================
// memory_bridge_semantic
// ============================================================

CREATE VECTOR INDEX memory_bridge_semantic__Commitment ON :Commitment(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX memory_bridge_semantic__Decision ON :Decision(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
