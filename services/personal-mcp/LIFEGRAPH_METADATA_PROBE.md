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

The initial probe opens no SQLite connection. Even `mode=ro` can create/update WAL auxiliary files; `query_only` does not prevent that. Do not substitute `immutable=1` for an actively changing database: it disables locking and change detection. Read only the 100-byte main-file header through an existing file descriptor opened with O_RDONLY/O_NOFOLLOW. These are **main-file metadata, potentially stale**, not current transactional authority. Obtain column definitions from the owner's reviewed migration manifest; live schema inspection requires a separately reviewed method.

```sh
python3 - "$APPROVED_CANONICAL_POLICY_PATH" <<'PY'
import json, os, stat, sys
path = os.path.abspath(sys.argv[1])
fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
try:
    before = os.fstat(fd)
    if not stat.S_ISREG(before.st_mode): raise SystemExit('Metadata probe denied')
    header = os.pread(fd, 100, 0)
    after = os.fstat(fd)
    current = os.lstat(path)
    if not stat.S_ISREG(current.st_mode): raise SystemExit('Metadata probe denied')
    identity = lambda s: (s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns, s.st_ctime_ns)
    if identity(before) != identity(after): raise SystemExit('Metadata probe denied')
    if (after.st_dev, after.st_ino) != (current.st_dev, current.st_ino):
        raise SystemExit('Metadata probe denied')
    if len(header) != 100 or header[:16] != b'SQLite format 3\x00':
        raise SystemExit('Metadata probe denied')
    read_format, write_format = header[19], header[18]
    print(json.dumps({'device': str(after.st_dev), 'inode': str(after.st_ino),
        'uid': after.st_uid, 'mode': oct(stat.S_IMODE(after.st_mode)),
        'main_file_user_version': int.from_bytes(header[60:64], 'big'),
        'read_format': read_format, 'write_format': write_format,
        'metadata_authority': 'main-file-only-potentially-stale'}))
    if (read_format, write_format) != (1, 1):
        raise SystemExit('Stop: WAL or unknown format; further probe requires review')
finally:
    os.close(fd)
PY
```

Format1/1 indicates rollback-journal format but does not distinguish DELETE from PERSIST/TRUNCATE or prove absence of an active transaction/hot journal. Format2/2 indicates WAL and stops this proposal. The source requires version2 and DELETE journal mode; header metadata does not authorize admission, initialization, migration or backfill. No records, schema SQL text, column values or counts are read.

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
