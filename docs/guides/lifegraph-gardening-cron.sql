-- LifeGraph Gardening cron for Beacon (operator-approved in Telegram, 2026-08-20 03:29 UTC).
-- Beacon claimed she created this cron; she never did. This row creates it for real.
-- Apply on vps-jane:  ssh deploy@jane-vps "sqlite3 /opt/philotic/data/aiua_context.db" < this file
-- Then restart or wait for the scheduler tick. Fires daily 10:30 UTC (before the 11:00 briefs).
-- Remove with: DELETE FROM graph_nodes WHERE node_key='cron_job:lifegraph-gardening:vps-jane';
INSERT OR REPLACE INTO graph_nodes (node_key, kind, label, data_json) VALUES (
  'cron_job:lifegraph-gardening:vps-jane',
  'cron_job',
  'role:agent-beacon:orchestrator',
  '{"created_at":1787952000000,"created_by":"operator","enabled":true,"guaranteed":false,"id":"lifegraph-gardening:vps-jane","last_fired_epoch":0,"next_fire_at":1787308200000,"payload":"{\"chat_id\":\"7898847424\",\"source\":\"telegram\",\"message\":\"Run Jared''s daily LifeGraph gardening pass (operator-approved 2026-08-20). This is the maintenance sweep, not a brief. Steps: (1) life.recall the active graph: Events, OpenLoops, NextActions, Commitments. (2) For every PROPOSED Event whose date clearly refers to a day now in the past, retire it (validation_state=retired). (3) For every NextAction or OpenLoop whose claim names a specific past date and is still open, flag it for the operator. (4) Flag near-duplicate nodes (same claim, different ids). (5) Send ONE Telegram summary listing ONLY what you actually changed or flagged, citing the EXACT node ids returned by the tools — never invent ids, never claim a change the tool result does not confirm. If nothing needed gardening, say so in one line.\"}","schedule":"0 30 10 * * * *","session_target":"isolated","silent_ok":true,"target_node_id":null,"target_role":"role:agent-beacon:orchestrator"}'
);
