// V006: Life Graph OS — lived-world nouns expansion (2026-08-25)
// Target:  Memgraph 3.10.1+
// Host:    vps-jane Tailscale 100.64.212.8:7687
// Depends: V003__vector_index_768d.cypher
//
// New labels: Place, Trip, Appointment, Moment (life_event_semantic);
//             Subscription, Asset, CreativeWork (goal_system_semantic).
// Every label swept by vector recall MUST have its index or the sweep errors
// with "Vector index <space>__<Label> does not exist" (the recurring
// Aspiration failure) — this migration also backfills that missing index.
//
// Apply each statement individually — Bolt does not support multi-statement batches.
// After applying run: SHOW VECTOR INDEX INFO; to confirm.

CREATE VECTOR INDEX life_event_semantic__Trip ON :Trip(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX life_event_semantic__Appointment ON :Appointment(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX life_event_semantic__Moment ON :Moment(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX life_event_semantic__Place ON :Place(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX goal_system_semantic__Subscription ON :Subscription(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX goal_system_semantic__Asset ON :Asset(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX goal_system_semantic__CreativeWork ON :CreativeWork(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};

// Backfill: role_person_semantic__Aspiration was never created on vps
// (recurring "Vector index role_person_semantic__Aspiration does not exist"
// ERROR on every autorecall sweep since at least 2026-08-22).
CREATE VECTOR INDEX role_person_semantic__Aspiration ON :Aspiration(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};
