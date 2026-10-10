// Read-only adapter to the existing canonical privacy SQLite schema. The graph
// export and coordinated release barrier remain server-owned interfaces; no
// Memgraph address, credentials, unreviewed schema mapping or grant is inferred.
import { constants, openSync, closeSync, fstatSync, lstatSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { isAbsolute } from 'node:path';
import { createHash } from 'node:crypto';
const fail = () => { throw new Error('LifeGraph storage unavailable'); };
const boundedText = value => typeof value === 'string' && value.trim() && value.length <= 128;
const ownKeys = (value, keys) => value && typeof value === 'object' && !Array.isArray(value) && Object.keys(value).every(k => keys.includes(k));
const revisionFor = (graph, policy) => createHash('sha256').update(JSON.stringify([graph, policy])).digest('hex');
export function createLifeGraphStorageAuthority({ policyDatabase, graphReader, releaseBarrier, storeIdentity }) {
  if (typeof policyDatabase !== 'string' || !isAbsolute(policyDatabase) || policyDatabase.includes('\0') ||
      typeof graphReader?.readSnapshot !== 'function' || typeof releaseBarrier?.admit !== 'function') fail();
  let db, fd;
  const expected = storeIdentity && { device: storeIdentity.device, inode: storeIdentity.inode };
  const verifyIdentity = () => {
    if (!expected) return;
    for (const stat of [fstatSync(fd), lstatSync(policyDatabase)]) {
      if (!stat.isFile() || stat.uid !== process.getuid() || (stat.mode & 0o077) !== 0 ||
          String(stat.dev) !== expected.device || String(stat.ino) !== expected.inode) fail();
    }
  };
  try {
    // readOnly refuses a missing database; no init/migrate/create side effects.
    if (expected) {
      if (process.platform !== 'linux' || typeof expected.device !== 'string' || typeof expected.inode !== 'string') fail();
      fd = openSync(policyDatabase, constants.O_RDONLY | constants.O_NOFOLLOW);
      verifyIdentity();
    }
    db = new DatabaseSync(expected ? `/proc/self/fd/${fd}` : policyDatabase, { readOnly: true, timeout: 1000 });
    verifyIdentity();
    // FD aliases cannot safely locate canonical WAL sidecars. Deny rather than
    // silently opening another journal authority; production mode is surveyed.
    if (expected && db.prepare('PRAGMA journal_mode').get().journal_mode !== 'delete') fail();
    db.exec('PRAGMA query_only = ON');
    const tables = db.prepare("SELECT name FROM sqlite_master WHERE type='table'").all().map(r => r.name);
    if (!tables.includes('privacy_revision') || !tables.includes('privacy_policy')) fail();
    db.prepare('SELECT singleton, revision FROM privacy_revision WHERE singleton=1').get();
    db.prepare('SELECT resource, policy_json FROM privacy_policy LIMIT 0').all();
  } catch { try { db?.close(); } finally { if (fd !== undefined) closeSync(fd); } fail(); }
  const liveRevision = () => {
    verifyIdentity();
    const rows = db.prepare('SELECT singleton, revision FROM privacy_revision').all();
    if (rows.length !== 1 || rows[0].singleton !== 1 || !Number.isSafeInteger(rows[0].revision) || rows[0].revision < 1) fail();
    return String(rows[0].revision);
  };
  function policiesFor(ids, signal) {
    const found = new Map(), pending = [...ids];
    const get = db.prepare('SELECT policy_json FROM privacy_policy WHERE resource = ?');
    while (pending.length) {
      signal?.throwIfAborted();
      const id = pending.pop();
      if (found.has(id)) continue;
      if (!boundedText(id) || found.size >= 4096) fail();
      const row = get.get(id);
      if (!row || typeof row.policy_json !== 'string' || row.policy_json.length > 65536) fail();
      const p = JSON.parse(row.policy_json);
      if (!ownKeys(p, ['owner', 'creator', 'creator_read_grant', 'read_roles', 'private', 'external_operations', 'sources']) ||
          !boundedText(p.owner) || !boundedText(p.creator) || typeof p.private !== 'boolean' || typeof p.creator_read_grant !== 'boolean' ||
          !Array.isArray(p.sources) || p.sources.length > 128 || !p.sources.every(boundedText) ||
          !Array.isArray(p.read_roles) || p.read_roles.length > 32 || !p.read_roles.every(boundedText) ||
          !Array.isArray(p.external_operations) || p.external_operations.length > 32 || !p.external_operations.every(boundedText)) fail();
      found.set(id, p); pending.push(...p.sources);
    }
    return found;
  }
  return Object.freeze({
    bindResponse(request, response) { releaseBarrier.bindResponse?.(request, response); },
    verifyResponse(request, result) { try { verifyIdentity(); return typeof releaseBarrier.verifyResponse !== 'function' || releaseBarrier.verifyResponse(request, result) === true; } catch { return false; } },
    async snapshot(context) {
      try {
        context.signal?.throwIfAborted();
        const raw = structuredClone(await graphReader.readSnapshot(context));
        context.signal?.throwIfAborted();
        if (!ownKeys(raw, ['namespace', 'revision', 'nodes', 'edges']) || raw.namespace !== context.namespace ||
            !boundedText(raw.revision) || !Array.isArray(raw.nodes) || raw.nodes.length > 2048 ||
            !Array.isArray(raw.edges) || raw.edges.length > 4096) fail();
        for (const [items, edge] of [[raw.nodes, false], [raw.edges, true]]) for (const item of items) {
          // The graph cannot supply or override the canonical privacy ACL.
          if (!ownKeys(item, edge ? ['id', 'namespace', 'from', 'to', 'relation', 'sourcePolicyManifest'] : ['id', 'namespace', 'kind', 'summary', 'canonicalId', 'sourcePolicyManifest']) ||
              item.namespace !== context.namespace || !boundedText(item.id)) fail();
        }
        db.exec('BEGIN');
        try {
          const policyRevision = liveRevision();
          const ids = [...raw.nodes, ...raw.edges].map(r => r.id);
          if (new Set(ids).size !== ids.length) fail();
          const policies = policiesFor(ids, context.signal);
          const withPolicy = item => {
            const { sourcePolicyManifest, ...record } = item;
            const policy = policies.get(item.id);
            // A content binding's lineage must agree with canonical immutable
            // ancestry. It cannot substitute a permissive graph manifest.
            if (sourcePolicyManifest !== undefined && (!Array.isArray(sourcePolicyManifest) ||
                JSON.stringify([...sourcePolicyManifest].sort()) !== JSON.stringify([...policy.sources].sort()))) fail();
            return { ...record, policy };
          };
          verifyIdentity();
          const selected = new Set(ids);
          return { namespace: context.namespace, revision: revisionFor(raw.revision, policyRevision),
            nodes: raw.nodes.map(withPolicy),
            edges: raw.edges.map(withPolicy),
            sourcePolicies: [...policies].filter(([id]) => !selected.has(id)).map(([id, policy]) => ({ id, namespace: context.namespace, policy })) };
        } finally { db.exec('ROLLBACK'); }
      } catch { fail(); }
    },
    async authorizeRelease(admission) {
      try {
        admission.signal?.throwIfAborted();
        // The injected barrier owns graph + identity/grant serialization and
        // must invoke this synchronous canonical-policy check inside its final
        // admission critical section. No read-only SQLite snapshot alone can
        // serialize distributed graph and issuer revocations.
        let checked = false;
        const admitted = await releaseBarrier.admit(admission, ({ graphRevision, actor }) => {
          checked = false;
          admission.signal?.throwIfAborted();
          if (!boundedText(graphRevision) || JSON.stringify(actor) !== JSON.stringify(admission.actor) ||
              revisionFor(graphRevision, liveRevision()) !== admission.revision) return false;
          checked = true; return true;
        });
        admission.signal?.throwIfAborted();
        return checked && admitted === true;
      } catch { return false; }
    },
    close() { try { db.close(); } finally { if (fd !== undefined) closeSync(fd); } },
  });
}
