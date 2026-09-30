// LifeGraph true-up after Beacon's 2026-09-11 session (DEF-113..117).
// APPLIED on vps-jane 2026-09-11 by the operator, in three passes; this is
// the consolidated, idempotent form of what actually ran.
//
//   ssh deploy@jane-vps 'docker exec -i philotic-memgraph mgconsole' < scripts/lifegraph-patches/2026-09-11-beacon-trueup.cypher
//
// mgconsole on stdin stopped silently after the third statement when the
// statements were multi-line with comment blocks between them — every
// statement below is therefore ONE line and returns a row so a silent stop
// is visible as a missing table.

// 1. Stray node manufactured by the old MERGE-style life.commit (DEF-117).
MATCH (n:Commitment {id: "557"}) DETACH DELETE n;

// 2. The real MRI commitment: text already corrected, promote it.
MATCH (n:Commitment {id: "life:commitment:mei_due_date_update_20260909"}) SET n.validation_state = "confirmed", n.last_confirmed_at = "2026-09-11T13:28:11Z", n.resolution_note = "operator corrected MEI->MRI 2026-09-11; confirmed by steward true-up" RETURN n.id, n.validation_state;

// 3. Duplicate events re-observed by the morning sweep (DEF-113 loop + DEF-116 blind recall).
MATCH (n:Event) WHERE n.id IN ["life:event:school_drive_daxton_20260910", "life:event:daxton_gsu_reinstatement_status_20260910"] SET n.validation_state = "retired", n.resolution_note = "retired 2026-09-11: duplicate of an earlier event re-observed by the morning distillation sweep (DEF-113/DEF-116)" RETURN n.id, n.validation_state;

// 4. Daxton's GSU reinstatement closes the investigation loop and the monitoring commitment.
MATCH (n) WHERE n.id IN ["life:open_loop:daxton_gsu_dropped_class_investigation_20260906", "life:commitment:monitor_daxton_gsu_dropped_class_20260908"] SET n.validation_state = "confirmed", n.status = "resolved", n.resolved_at = "2026-09-11T13:26:16+00:00", n.resolution_note = "operator reported 2026-09-11: Daxton reinstated into the GSU class by his teacher (life:event:daxton_gsu_reinstated_20260911)" RETURN n.id, n.status;
MATCH (e:Event {id: "life:event:daxton_gsu_reinstated_20260911"}) SET e.validation_state = "confirmed" RETURN e.id, e.validation_state;
MATCH (e:Event {id: "life:event:daxton_gsu_reinstated_20260911"}), (l) WHERE l.id IN ["life:open_loop:daxton_gsu_dropped_class_investigation_20260906", "life:commitment:monitor_daxton_gsu_dropped_class_20260908"] MERGE (e)-[r:RESOLVES]->(l) RETURN e.id, type(r), l.id;

// 5. Operator-confirmed facts from the 13:26 UTC message: promote them.
MATCH (n) WHERE n.id IN ["life:event:toastmasters_icebreaker_practice_20260911", "life:commitment:delta_utah_trip_20260928"] SET n.validation_state = "confirmed" RETURN n.id, n.validation_state;

// 6. Historical strays from the same MERGE bug (ids "66","108","33","11","12"): each duplicated a real
//    node that was already confirmed+resolved, except "66" whose resolution (haircut/dry cleaners done
//    2026-08-09) never reached the real loop — recover it, then delete the strays.
MATCH (r {id: "life:open-loop:df43b019a480aac9"}) SET r.validation_state = "confirmed", r.status = "resolved", r.resolved_at = "2026-08-09T00:00:00Z", r.claim_summary = "On August 9, 2026, the operator completed his haircut and beard trim, and took his clothes to the dry cleaners.", r.resolution_note = "resolution recovered 2026-09-11 from stray commit node id=66 (DEF-117)" RETURN r.id, r.status;
MATCH (s) WHERE s.id IN ["66", "108", "33", "11", "12"] DETACH DELETE s;

// 7. Verify: nothing says MEI and no node has a bare numeric id (expect 0 rows / 0).
MATCH (n) WHERE n.claim_summary IS NOT NULL AND n.claim_summary CONTAINS "MEI" RETURN id(n), n.id, n.claim_summary;
MATCH (n) WHERE n.id IS NOT NULL AND n.id =~ "^[0-9]+$" RETURN count(n) AS strays_remaining;
