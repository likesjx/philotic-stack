---
name: ontology-evolution
description: How LifeGraph vocabulary evolves — the governed data lane Beacon drives herself, and the code lane for structural changes, with every lockstep surface enumerated. Use when adding nouns/verbs, changing named queries, or reviewing an ontology_extension patch.
---

# Ontology Evolution

The LifeGraph vocabulary has two lanes. Choosing the right one is the skill.

## Lane 1 — data lane (Beacon self-serve, no code, no deploy)

For **new nouns (labels) and verbs (endpoint-validated edges)**:

1. Beacon calls `life.patch.propose` with `patch_kind: "schema_patch"` and an
   `ontology_extension` payload:
   ```json
   {"labels": [{"name": "Pet", "space": "life_event_semantic", "guidance": "A companion animal."}],
    "edges": [{"rel_type": "CARES_FOR", "source_labels": ["Routine", "Person"], "target_labels": ["Pet"]}]}
   ```
   Validation at propose time: PascalCase label / SCREAMING_SNAKE rel identifiers,
   space ∈ the five semantic prefixes, endpoints must be known (core ∪ applied
   extensions ∪ this spec), no shadowing of compiled core vocabulary.
2. The operator approves in conversation.
3. Beacon calls `life.patch.apply {patch_id, decision: "confirm"}`. Apply
   re-validates, **creates the vector index for each new label**
   (`{space}__{Name}`, 768d cos), merges into the `OntologyExtension` graph
   node, and the vocabulary is live immediately — writable via `life.observe`,
   swept by recall, listable via `life.list`, documented by `life.ontology`.
4. Beacon verifies with `life.ontology` (the `extensions` section) before
   reporting — never claim an extension exists without reading it back.

## Lane 2 — code lane (structural changes)

Anything beyond labels/edges needs Rust: new named maintenance queries, date
semantics, new semantic spaces, lifecycle rules. The lockstep surfaces (all
guarded by tests — run `cargo test -p data-memorygraphrag -p philote` and the
failures enumerate what you missed):

1. `crates/data-memorygraphrag/src/cypher.rs` — `KNOWN_LABELS`,
   `AGENDA_EDGE_RULES`, doc-mirror test.
2. `crates/data-memorygraphrag/src/projection.rs` — `labels_for_space`,
   `embedding_space_for_label`, expansion vocabulary tests.
3. `crates/data-memorygraphrag/src/ontology.rs` — `NODE_LABELS`, named
   queries, document sections; bump `ONTOLOGY_VERSION`.
4. `crates/philote/src/catalog.rs` — `life.observe` schema label text.
5. `docs/architecture/life-graph/LIFE_GRAPH_SCHEMA.md` — relationship table.
6. **A `V00x` migration in `docs/architecture/life-graph/migrations/` creating
   the vector index for every newly swept label** — a swept label without an
   index errors on every recall (`Vector index <space>__<Label> does not
   exist`). Apply on vps via `mgconsole` (bare statements, no comments).
7. Deploy: merge → `just vps-deploy-ci` (main checkout) + mac deploy script →
   fire the gardening cron once and watch the dispatch.

## Review guidance for ontology_extension patches

Prefer few, concrete, lived-world nouns over abstractions; every noun should
have things that will point at it (`ABOUT`, `MAINTAINS`, edges). A verb needs
tight endpoints — reject `ABOUT` from-everything-to-everything shapes. Kept
history is `Moment`; never propose retire-able nouns for memories.
