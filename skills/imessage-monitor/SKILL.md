---
name: imessage-monitor
description: Reference skill definition for Hermes to monitor the local iMessage database and forward messages from specified automated senders. Intended to be registered by Hermes' orchestrator via skill.register.
catalog:
  skill_name: imessage-monitor
  implied_tools:
    - subagent.spawn
  validation_state: draft
  skill_markers:
    - shell_required
    - macOS_only
  field_sources:
    repo_skill_path: skills/imessage-monitor/SKILL.md
    platform: macOS
    db_path: "~/Library/Messages/chat.db"
    register_as: skill.register
---

# iMessage Monitor

Reference skill for Hermes to read messages from the local iMessage database and forward alerts from automated senders.

## How to register (Hermes orchestrator)

Hermes' orchestrator should call `skill.register` with this payload:

```json
{
  "skill_name": "imessage-monitor",
  "description": "Query the local iMessage database for recent messages from a specified sender handle. Returns messages from the last N hours with timestamps. Use for monitoring automated account alerts.",
  "subagent_kind": "philote-worker",
  "goal": "Use bash.exec to query the iMessage database for recent messages from sender handle {{handle}}. Run: sqlite3 ~/Library/Messages/chat.db \"SELECT datetime(date/1000000000 + 978307200, 'unixepoch', 'localtime') as sent_at, text FROM message m JOIN handle h ON m.handle_id = h.ROWID WHERE h.id = '{{handle}}' AND date > (strftime('%s', 'now', '-{{hours}} hours') - 978307200) * 1000000000 ORDER BY date DESC LIMIT 50\". Report all found messages verbatim with their timestamps. If no messages found, say so clearly.",
  "allowed_classes": ["shell"]
}
```

Then assign to Hermes' comm-specialist role:

```json
{
  "role_name": "hermes-comm-specialist",
  "skill_name": "imessage-monitor"
}
```

## Usage by Hermes (after assignment)

Once assigned, Hermes' comm-specialist role can call `subagent.spawn` with:

```json
{
  "skill_name": "imessage-monitor",
  "inputs": {
    "handle": "+15551234567",
    "hours": "24"
  }
}
```

The `handle` is the iMessage address — phone number (`+15551234567`), email, or short code — of the automated sender.

## Finding the sender handle

To find the correct handle for an automated account, query the handles table:

```sql
sqlite3 ~/Library/Messages/chat.db "SELECT ROWID, id, service FROM handle WHERE id LIKE '%keyword%';"
```

Replace `keyword` with part of the sender's phone number or address.

## Notes

- The iMessage DB path is `~/Library/Messages/chat.db` — this is the user's local Messages database on macOS
- Apple's epoch offset is 978307200 (Jan 1, 2001) — the query already accounts for this
- Full Disk Access must be granted to the terminal or the running process for the sqlite3 read to succeed
- This is macOS-only and requires Jane (mbp-jane) to be the execution host
