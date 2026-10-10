# Owner-reviewed metadata-only probe commands

Proposal only; none of these commands has been run against production. Each variable below must be supplied by the owner from its existing installation inventory. Do not infer hosts, paths, credentials, units or connection arguments. Use an already authorized operator channel, not a new SSH connection or credential. Parent/Muninn owner reviews the exact substituted command list and its output projection before execution.

## Supervisor and artifact metadata

For each explicitly reviewed unit and binary, only:

```sh
systemctl show "$APPROVED_UNIT" --property=Id,LoadState,ActiveState,SubState,MainPID,FragmentPath
sha256sum -- "$APPROVED_BINARY"
docker inspect --format '{{.Config.Image}} {{.Image}}' "$APPROVED_MEMGRAPH_CONTAINER"
```

Do not run `systemctl cat`, inspect Environment/ExecStart, dump `/proc/*/environ`, or print full Docker configuration. The owner separately records the allowlisted nonsecret coordination flag value from its reviewed deployment manifest. These commands reveal unit identity, running status, artifact identity and image reference/digest only. MainPID is supervisor metadata, never accepted as client enrollment authority.

## Existing canonical SQLite metadata

The following script receives only the owner-reviewed existing path. It opens read-only with no CREATE, never selects policy/resource rows, and emits only file identity/ownership/mode, schema version, journal mode and allowlisted column definitions. Do not initialize/migrate/backfill the database. Do not include schema SQL text or row counts.

```sh
python3 - "$APPROVED_CANONICAL_POLICY_PATH" <<'PY'
import json, os, sqlite3, stat, sys
from urllib.parse import quote
path = os.path.abspath(sys.argv[1])
s = os.lstat(path)
if not stat.S_ISREG(s.st_mode): raise SystemExit('Metadata probe denied')
db = sqlite3.connect('file:' + quote(path, safe='/') + '?mode=ro', uri=True)
db.execute('PRAGMA query_only=ON')
columns = {}
for table in ('privacy_revision','privacy_policy','local_authority_receipt','capture_inbox'):
    columns[table] = [{'name': r[1], 'type': r[2], 'notnull': bool(r[3]), 'pk': bool(r[5])}
                      for r in db.execute('PRAGMA table_info(' + table + ')')]
print(json.dumps({'device': str(s.st_dev), 'inode': str(s.st_ino), 'uid': s.st_uid,
                  'mode': oct(stat.S_IMODE(s.st_mode)),
                  'user_version': db.execute('PRAGMA user_version').fetchone()[0],
                  'journal_mode': db.execute('PRAGMA journal_mode').fetchone()[0], 'columns': columns}))
db.close()
PY
```

Expected source requirement is version2 with the named columns and DELETE journal mode. An incompatible/missing store keeps admission disabled; it does not authorize changing the store.

## Memgraph metadata through the existing approved adapter

Run precisely these statements through the owner's existing authorized driver/session; no credential or connection invocation is specified or requested here:

```cypher
SHOW STORAGE INFO;
SHOW CONSTRAINT INFO;
SHOW INDEX INFO;
```

Before any output reaches this thread, the owner adapter projects `SHOW STORAGE INFO` to only `storage_mode`, `global_isolation_level`, `session_isolation_level`. It projects constraints to only constraint type, label and property-name list; indexes to only index type, label and property-name list. Retain only labels explicitly reviewed as LifeGraph schema. Omit all other fields, including node/edge counts, index estimates, IDs and values. If the installed version uses different output keys, stop and review the metadata mapping rather than emitting raw output. No `MATCH`, `RETURN` of records, custom Cypher, setting changes, transaction isolation changes or credential inspection is included. Version/edition is reported from the reviewed image/build inventory; do not guess a version-discovery query.

## Issuer and writer manifest

Use source/deployment manifests to report only: repository+commit, deployed artifact digest, enabled/disabled feature flags, revocation entrypoint names, database transaction capability class, and each writer's supervised/enrolled/unmanaged status under a redacted capability label. Do not query issuer admission/users/token collections, enumerate accounts, print role/grant records or expose credential hashes. Record third-party/manual mgconsole and migration capabilities as unmanaged until independently accounted for. This manifest is required evidence, not evidence derivable from a graph count or a source grep.
