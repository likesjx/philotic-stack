#!/usr/bin/env python3
import json
import sqlite3
import sys


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: mesh_seed_hotels.py <db_path> <records_json_path>", file=sys.stderr)
        return 2

    db_path, records_path = sys.argv[1], sys.argv[2]

    with open(records_path, "r", encoding="utf-8") as fh:
        records = json.load(fh)

    conn = sqlite3.connect(db_path)
    try:
        for rec in records:
            hotel_name = rec["hotel_name"]
            conn.execute(
                """
                INSERT INTO graph_nodes (node_key, kind, label, data_json, updated_at)
                VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
                ON CONFLICT(node_key) DO UPDATE SET
                  kind=excluded.kind,
                  label=excluded.label,
                  data_json=excluded.data_json,
                  updated_at=CURRENT_TIMESTAMP
                """,
                (
                    f"hotel:{hotel_name}",
                    "hotel",
                    hotel_name,
                    json.dumps(rec, separators=(",", ":")),
                ),
            )
        conn.commit()
    finally:
        conn.close()

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
