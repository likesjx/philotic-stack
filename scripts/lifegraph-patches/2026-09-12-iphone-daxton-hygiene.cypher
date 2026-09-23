// LifeGraph hygiene after Beacon's 2026-09-12 turns. Operator-run:
//   ssh deploy@jane-vps 'docker exec -i philotic-memgraph mgconsole' < scripts/lifegraph-patches/2026-09-12-iphone-daxton-hygiene.cypher
// One statement per line (mgconsole on stdin stops silently after a multi-line block). Nothing is deleted — retire/resolve only.

// 1. iPhone 18 Pro: the operator said "It's not a goal or even a commitment. It's more of a event." The Commitment stays as history but retires; the Event is the live record and is operator-confirmed.
MATCH (c:Commitment {id: "life:commitment:iphone_18_pro_delivery"}) SET c.validation_state = "retired", c.resolution_note = "retired 2026-09-12: operator reclassified as an Event (life:event:iphone_18_pro_delivery_20260918)" RETURN c.id, c.validation_state;
MATCH (e:Event {id: "life:event:iphone_18_pro_delivery_20260918"}) SET e.validation_state = "confirmed", e.claim_summary = "iPhone 18 Pro (pre-ordered 2026-09-12) delivers on September 18, 2026.", e.occurs_at = "2026-09-18" RETURN e.id, e.validation_state;

// 2. Person duplicates written by the morning sweep: life:person:daxton duplicates life:person:daxton_thomas_likes (2026-08-26). Retire the new one and point at the original.
MATCH (d:Person {id: "life:person:daxton"}) SET d.validation_state = "retired", d.resolution_note = "retired 2026-09-12: duplicate of life:person:daxton_thomas_likes" RETURN d.id, d.validation_state;
MATCH (n:Person {id: "life:person:daxton"}), (o:Person {id: "life:person:daxton_thomas_likes"}) MERGE (n)-[r:DUPLICATE_OF]->(o) RETURN n.id, type(r), o.id;

// 3. Verify.
MATCH (p:Person) WHERE p.id IN ["life:person:nadi", "life:person:daxton", "life:person:gabby", "life:person:brandon", "life:person:daxton_thomas_likes"] RETURN p.id, p.validation_state;
