// LifeGraph true-up after Beacon's 2026-09-11 session (DEF-113..117).
// Operator-run on vps-jane (remote DB writes are classifier-blocked from the
// Claude Code harness):
//
//   ssh deploy@jane-vps 'docker exec -i philotic-memgraph mgconsole' < scripts/lifegraph-patches/2026-09-11-beacon-trueup.cypher
//
// Every statement is idempotent. Internal ids (id(n)) were read live on
// 2026-09-11 13:40 UTC; the WHERE clauses also match on the stable `id`
// property so a re-numbered graph still applies safely.

// 1. Stray node manufactured by the old MERGE-style life.commit (DEF-117):
//    Commitment {id: "557"} — the model committed Memgraph's internal id.
MATCH (n:Commitment {id: "557"}) DETACH DELETE n;

// 2. The real MRI commitment: text was already corrected via graph.query,
//    but the commit that should have confirmed it landed on the stray node.
MATCH (n:Commitment {id: "life:commitment:mei_due_date_update_20260909"})
SET n.validation_state = "confirmed",
    n.last_confirmed_at = "2026-09-11T13:28:11Z",
    n.resolution_note = "operator corrected MEI->MRI 2026-09-11; confirmed by steward true-up"
RETURN n.id, n.claim_summary, n.validation_state;

// 3. Duplicate events re-observed by the morning distillation sweep
//    (DEF-113 loop + DEF-116 blind recall): 584 duplicates 580, 585 duplicates 562.
MATCH (n:Event)
WHERE n.id IN ["life:event:school_drive_daxton_20260910",
               "life:event:daxton_gsu_reinstatement_status_20260910"]
SET n.validation_state = "retired",
    n.resolution_note = "retired 2026-09-11: duplicate of an earlier event re-observed by the morning distillation sweep (DEF-113/DEF-116)"
RETURN n.id, n.validation_state;

// 4. Daxton's GSU reinstatement closes the investigation loop and the
//    monitoring commitment — the outcome the operator reported at 13:26 UTC.
MATCH (n)
WHERE n.id IN ["life:open_loop:daxton_gsu_dropped_class_investigation_20260906",
               "life:commitment:monitor_daxton_gsu_dropped_class_20260908"]
SET n.validation_state = "confirmed",
    n.status = "resolved",
    n.resolved_at = "2026-09-11T13:26:16+00:00",
    n.resolution_note = "operator reported 2026-09-11: Daxton reinstated into the GSU class by his teacher (life:event:daxton_gsu_reinstated_20260911)"
RETURN n.id, n.status;

MATCH (e:Event {id: "life:event:daxton_gsu_reinstated_20260911"})
SET e.validation_state = "confirmed"
WITH e
MATCH (l)
WHERE l.id IN ["life:open_loop:daxton_gsu_dropped_class_investigation_20260906",
               "life:commitment:monitor_daxton_gsu_dropped_class_20260908"]
MERGE (e)-[r:RESOLVES]->(l)
RETURN e.id, type(r), l.id;

// 5. Operator-confirmed facts from the 13:26 UTC message that landed as
//    `proposed` in the 13:27 turn: promote them.
MATCH (n)
WHERE n.id IN ["life:event:toastmasters_icebreaker_practice_20260911",
               "life:commitment:delta_utah_trip_20260928"]
SET n.validation_state = "confirmed"
RETURN n.id, n.validation_state;

// 6. Verify: nothing left that says MEI, and no node with a bare numeric id.
MATCH (n)
WHERE (n.claim_summary IS NOT NULL AND n.claim_summary CONTAINS "MEI")
   OR (n.id IS NOT NULL AND n.id =~ "^[0-9]+$")
RETURN id(n), labels(n), n.id, n.claim_summary;
