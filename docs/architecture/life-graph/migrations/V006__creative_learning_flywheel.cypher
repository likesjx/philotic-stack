// V006: Life Graph OS — Creative learning flywheel
// Target:  Memgraph 3.10.1+
// Depends: V003__vector_index_768d.cypher
//
// Adds the smallest governed vocabulary needed to carry a creative thread
// from curiosity through reusable learning. Apply each statement separately.

CREATE CONSTRAINT ON (n:Question) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:Idea) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:Experiment) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:Artifact) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:Learning) ASSERT n.id IS UNIQUE;
CREATE CONSTRAINT ON (n:Source) ASSERT n.id IS UNIQUE;

CREATE INDEX ON :Question(validation_state);
CREATE INDEX ON :Idea(validation_state);
CREATE INDEX ON :Experiment(validation_state);
CREATE INDEX ON :Artifact(validation_state);
CREATE INDEX ON :Learning(validation_state);
CREATE INDEX ON :Source(validation_state);

CREATE INDEX ON :Question(observed_at);
CREATE INDEX ON :Idea(observed_at);
CREATE INDEX ON :Experiment(observed_at);
CREATE INDEX ON :Artifact(observed_at);
CREATE INDEX ON :Learning(observed_at);
CREATE INDEX ON :Source(observed_at);

CREATE INDEX ON :Question(pilot_domain);
CREATE INDEX ON :Idea(pilot_domain);
CREATE INDEX ON :Experiment(pilot_domain);
CREATE INDEX ON :Artifact(pilot_domain);
CREATE INDEX ON :Learning(pilot_domain);
CREATE INDEX ON :Source(pilot_domain);
CREATE INDEX ON :Signal(capture_kind);
CREATE INDEX ON :Signal(inbox_state);

CREATE VECTOR INDEX creative_learning_semantic__Question ON :Question(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX creative_learning_semantic__Idea ON :Idea(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX creative_learning_semantic__Experiment ON :Experiment(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX creative_learning_semantic__Artifact ON :Artifact(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX creative_learning_semantic__Learning ON :Learning(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
CREATE VECTOR INDEX creative_learning_semantic__Source ON :Source(embedding) WITH CONFIG {"dimension": 768, "capacity": 5000, "metric": "cos"};
