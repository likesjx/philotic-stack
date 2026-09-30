// V007: Life Graph OS — health & fitness domain labels (2026-09-30)
// Target:  Memgraph 3.10.1+
// Host:    vps-jane Tailscale 100.64.212.8:7687
// Depends: V003__vector_index_768d.cypher
//
// New labels: Workout, Measurement (life_event_semantic) — structured lived
// records for the "human" domain (steward: coach): a tracked workout session
// (Hevy import: scripts/lifegraph-ingest-hevy.py) and a point-in-time
// health/body measurement (weight, HRV, resting HR, ...).
//
// ORDER MATTERS: apply this BEFORE deploying a runner built with these labels
// in its sweep set — every label swept by vector recall MUST have its index
// or the sweep errors ("Vector index <space>__<Label> does not exist", the
// recurring Aspiration failure V006 backfilled).
//
// Apply each statement individually — Bolt does not support multi-statement
// batches. After applying run: SHOW VECTOR INDEX INFO; to confirm.

CREATE VECTOR INDEX life_event_semantic__Workout ON :Workout(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};
CREATE VECTOR INDEX life_event_semantic__Measurement ON :Measurement(embedding) WITH CONFIG {"dimension": 768, "capacity": 10000, "metric": "cos"};
