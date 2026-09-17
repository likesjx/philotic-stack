#!/usr/bin/env python3
"""Per-node probe for the philote memory UAT (scripts/uat-philote-memory.sh).

Runs ON the node it reports about and prints one JSON object. Read-only: it never
writes to Muninn or to a hotel database. Credentials are read locally (the node's
own `~/.muninn/mcp.token`, or MUNINN_MCP_TOKEN for the Cortex) and never printed.

Modes
  muninn-health                  health + version of the local Muninn
  muninn-recent <n>              ids of the n most recently created memories per vault
  muninn-have                    stdin {vault: [ids]} -> which are missing/soft-deleted here
  hotel <context.db> <aiua.log>  recall telemetry, rejected writes, tool grants, credential state
  sleep-run <context.db>         last memory-sleep run summary (Cortex hotel)
"""
import datetime, json, os, re, sqlite3, sys, urllib.error, urllib.request

REST = "http://127.0.0.1:8475"
MCP = "http://127.0.0.1:8750/mcp"
# Deliberately tool-free: the companion profile serves models with no tool-use
# endpoint, so granting tools would break their calls.
EXPECTED_TOOL_FREE_PROFILES = {"companion"}
MEMORY_VAULT_RE = re.compile(r"^(default|fleet_knowledge|user_[\w.-]+|self_[\w.-]+)$")


def mcp_token():
    tok = os.environ.get("MUNINN_MCP_TOKEN")
    if tok:
        return tok.strip()
    path = os.path.expanduser("~/.muninn/mcp.token")
    return open(path).read().strip() if os.path.exists(path) else ""


def mcp(name, args, timeout=60):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": {"name": name, "arguments": args}}).encode()
    req = urllib.request.Request(MCP, data=body, method="POST")
    req.add_header("Authorization", "Bearer " + mcp_token())
    req.add_header("Content-Type", "application/json")
    req.add_header("Accept", "application/json, text/event-stream")
    raw = urllib.request.urlopen(req, timeout=timeout).read().decode()
    if "data:" in raw[:40]:
        raw = [l[5:] for l in raw.splitlines() if l.startswith("data:")][-1]
    parsed = json.loads(raw)
    if "error" in parsed:
        raise RuntimeError(parsed["error"].get("message", "mcp error"))
    return json.loads(parsed["result"]["content"][0]["text"])


def muninn_health():
    try:
        with urllib.request.urlopen(REST + "/api/health", timeout=10) as r:
            return json.loads(r.read().decode())
    except Exception as e:  # unreachable is a finding, not a crash
        return {"status": "unreachable", "error": str(e)[:120]}


def vaults_with_memories():
    """Vault names this node can enumerate, memory vaults only."""
    names = set()
    try:
        page = mcp("muninn_get_enrichment_candidates", {"vault": "default", "limit": 1})
        if page is not None:
            names.add("default")
    except Exception:
        pass
    for guess in os.environ.get("UAT_MEMORY_VAULTS", "").split(","):
        if guess.strip():
            names.add(guess.strip())
    return sorted(n for n in names if MEMORY_VAULT_RE.match(n))


def recent_ids(per_vault):
    """Most recently created memory ids per vault — the ones replication lag would miss."""
    out = {}
    for vault in vaults_with_memories():
        items, cursor, pages = [], "", 0
        while pages < 20:
            args = {"vault": vault, "limit": 200}
            if cursor:
                args["cursor"] = cursor
            page = mcp("muninn_get_enrichment_candidates", args)
            items += page.get("items") or []
            cursor, pages = page.get("next_cursor") or "", pages + 1
            if not cursor:
                break
        items.sort(key=lambda r: r.get("created_at") or "", reverse=True)
        out[vault] = [r["id"] for r in items[:per_vault]]
    return out


def have(wanted):
    missing = {}
    for vault, ids in wanted.items():
        gone = []
        for eid in ids:
            try:
                rec = mcp("muninn_read", {"vault": vault, "id": eid})
            except Exception:
                rec = None
            if not rec or rec.get("id") != eid or rec.get("state") == "soft_deleted":
                gone.append(eid)
        if gone:
            missing[vault] = gone
    return missing


def hotel_report(db_path, log_path):
    con = sqlite3.connect(f"file:{os.path.expanduser(db_path)}?mode=ro", uri=True)
    con.execute("PRAGMA query_only=1")
    cutoff = int((datetime.datetime.now(datetime.UTC) - datetime.timedelta(hours=24)).timestamp())

    events = {"memory_auto_recall_completed": 0, "memory_auto_recall_skipped": 0,
              "memory_auto_recall_failed": 0}
    latencies, bands, gate_drops = [], {}, 0
    newest_failure = 0
    rows = con.execute(
        "SELECT data_json FROM graph_nodes WHERE kind='session_event' "
        "AND json_extract(data_json,'$.kind')='emit_task' "
        "ORDER BY rowid DESC LIMIT 5000"
    ).fetchall()
    for (raw,) in rows:
        try:
            payload = json.loads(raw)
        except Exception:
            continue
        if (payload.get("created_at") or 0) < cutoff:
            continue
        p = payload.get("payload_json") or {}
        if p.get("action") != "turn_event":
            continue
        event = p.get("event")
        if event in events:
            events[event] += 1
        if event == "memory_auto_recall_failed":
            newest_failure = max(newest_failure, payload.get("created_at") or 0)
        text = p.get("partial_content") or ""
        m = re.search(r"in (\d+)ms", text)
        if m:
            latencies.append(int(m.group(1)))
        m = re.search(r"dropped (\d+) below relevance", text)
        if m:
            gate_drops += int(m.group(1))
        for band, n in re.findall(r"(strong|moderate|weak|uncalibrated|filter_match|unknown)=(\d+)", text):
            bands[band] = bands.get(band, 0) + int(n)

    profiles_missing_memory = []
    for (key, data) in con.execute(
        "SELECT node_key, data_json FROM graph_nodes WHERE node_key LIKE 'toolset_profile:%'"
    ).fetchall():
        try:
            allowed = json.loads(data).get("allowed_tools") or []
            classes = json.loads(data).get("allowed_classes") or []
        except Exception:
            continue
        if "memory" in classes:
            continue
        name = key.split(":", 1)[1]
        if name in EXPECTED_TOOL_FREE_PROFILES:
            continue
        if not {"memory.recall", "memory.remember"} <= set(allowed):
            profiles_missing_memory.append(name)

    muninn_cfg = con.execute(
        "SELECT data_json FROM graph_nodes WHERE node_key='config:muninn'"
    ).fetchone()
    has_plaintext_password = bool(muninn_cfg and "admin_password" in muninn_cfg[0])
    has_vault_ref = bool(con.execute(
        "SELECT 1 FROM graph_nodes WHERE node_key='config:muninn_admin_secret_ref'"
    ).fetchone())

    rejected_writes = 0
    log_path = os.path.expanduser(log_path)
    if os.path.exists(log_path):
        day = (datetime.datetime.now(datetime.UTC) - datetime.timedelta(hours=24)).strftime("%Y-%m-%dT%H")
        with open(log_path, "rb") as fh:
            for line in fh.read().decode("utf-8", "replace").splitlines():
                if "421 Misdirected" in line and line[:13] >= day:
                    rejected_writes += 1

    latencies.sort()
    now = datetime.datetime.now(datetime.UTC).timestamp()
    return {
        "recall_events": events,
        "recall_newest_failure_age_h": round((now - newest_failure) / 3600, 1) if newest_failure else None,
        "recall_latency_ms": {
            "count": len(latencies),
            "median": latencies[len(latencies) // 2] if latencies else None,
            "p90": latencies[int(len(latencies) * 0.9)] if latencies else None,
        },
        "recall_bands": bands,
        "recall_gate_drops": gate_drops,
        "profiles_missing_memory_tools": profiles_missing_memory,
        "muninn_admin_password_in_config": has_plaintext_password,
        "muninn_admin_vault_ref": has_vault_ref,
        "rejected_writes_24h": rejected_writes,
    }


def sleep_run(db_path):
    con = sqlite3.connect(f"file:{os.path.expanduser(db_path)}?mode=ro", uri=True)
    row = con.execute(
        "SELECT data_json FROM graph_nodes WHERE node_key LIKE 'config:memory_sleep:last_run:%'"
    ).fetchone()
    if not row:
        return {"present": False}
    value = json.loads(row[0]).get("value")
    summary = json.loads(value) if isinstance(value, str) else value
    if isinstance(summary, str):
        summary = json.loads(summary)
    summary["present"] = True
    summary["age_hours"] = round(
        (datetime.datetime.now(datetime.UTC).timestamp() - (summary.get("finished_at") or 0)) / 3600, 1
    )
    return summary


mode = sys.argv[1]
if mode == "muninn-health":
    print(json.dumps(muninn_health()))
elif mode == "muninn-recent":
    print(json.dumps(recent_ids(int(sys.argv[2]))))
elif mode == "muninn-have":
    print(json.dumps(have(json.load(sys.stdin))))
elif mode == "hotel":
    print(json.dumps(hotel_report(sys.argv[2], sys.argv[3])))
elif mode == "sleep-run":
    print(json.dumps(sleep_run(sys.argv[2])))
else:
    raise SystemExit(f"unknown mode {mode}")
