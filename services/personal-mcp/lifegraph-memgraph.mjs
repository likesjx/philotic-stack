// Read-only projection of the existing LifeGraph ontology. The server-owned
// catalog is an exact ID/content/source binding, not a graph namespace property
// or an ACL supplied by the graph. Missing/legacy bindings fail closed.
import { createHash } from 'node:crypto';
const kindLabels = Object.freeze({ Goal: ['Goal'], Idea: ['GrowthHypothesis'],
  OpenLoop: ['OpenLoop'], Person: ['Person'], Place: ['Place'], Thing: ['Asset', 'CreativeWork'] });
export const LIFEGRAPH_NODE_QUERY = 'MATCH (n) WHERE n.id IN $ids RETURN n.id AS id, labels(n) AS labels, n.claim_summary AS summary, n.source_policy_manifest AS sources LIMIT 2049';
export const LIFEGRAPH_EDGE_QUERY = 'MATCH (a {id:$from})-[r]->(b {id:$to}) WHERE type(r)=$relation RETURN a.id AS from, b.id AS to, type(r) AS relation, r.source_policy_manifest AS sources LIMIT 2';
export const LIFEGRAPH_ROOT_QUERY = 'MATCH (b:LifeRootBinding {key:$key}) RETURN b.root_id AS root LIMIT 2';
export const LIFEGRAPH_ALIAS_QUERY = 'MATCH (b:LifeRootAlias {key:$key}) WHERE b.approved=true RETURN b.root_id AS root LIMIT 2';
const fail = () => { throw new Error('LifeGraph graph unavailable'); };
const text = v => typeof v === 'string' && v.trim() && v.length <= 128;
const digest = v => createHash('sha256').update(JSON.stringify(v)).digest('hex');
export const lifeGraphContentDigest = ({ id, labels, summary, sources }) => digest({ id,
  labels: [...labels].sort(), summary, sources: [...sources].sort() });
const manifest = value => {
  if (typeof value === 'string' && value.length > 65536) fail();
  const sources = typeof value === 'string' ? JSON.parse(value) : value;
  if (!Array.isArray(sources) || sources.length > 128 || !sources.every(text) || new Set(sources).size !== sources.length) fail();
  return sources;
};
export function createLifeGraphMemgraphReader({ transport, catalogs }) {
  if (typeof transport?.readTransaction !== 'function' || !Array.isArray(catalogs) || catalogs.length < 1 || catalogs.length > 32) fail();
  const catalog = new Map();
  for (const value of structuredClone(catalogs)) {
    if (!/^synthetic_[a-z0-9_]{1,64}$/.test(value.namespace) || catalog.has(value.namespace) ||
        !Array.isArray(value.nodes) || value.nodes.length > 2048 || !Array.isArray(value.edges) || value.edges.length > 4096) fail();
    const ids = new Set();
    for (const n of value.nodes) {
      if (!text(n.id) || ids.has(n.id) || !kindLabels[n.kind]?.includes(n.label) ||
          !/^[a-f0-9]{64}$/.test(n.contentDigest ?? '') || !Array.isArray(n.sources) || n.sources.length > 128 ||
          !n.sources.every(text) || new Set(n.sources).size !== n.sources.length ||
          (n.canonicalId !== undefined && (!text(n.canonicalId) || !text(n.rootBinding?.key) ||
            !['verifiedKey', 'approvedAlias'].includes(n.rootBinding?.type)))) fail();
      ids.add(n.id);
    }
    for (const e of value.edges) {
      if (!text(e.id) || ids.has(e.id) || !ids.has(e.from) || !ids.has(e.to) || !text(e.relation) ||
          !Array.isArray(e.sources) || e.sources.length > 128 || !e.sources.every(text) || new Set(e.sources).size !== e.sources.length) fail();
      ids.add(e.id);
    }
    for (const n of value.nodes) if (n.canonicalId !== undefined && !value.nodes.some(root => root.id === n.canonicalId && root.canonicalId === undefined)) fail();
    catalog.set(value.namespace, value);
  }
  return Object.freeze({ async readSnapshot(context) {
    try {
      context.signal?.throwIfAborted();
      const approved = catalog.get(context.namespace); if (!approved) fail();
      return await transport.readTransaction({ signal: context.signal }, async tx => {
        // The transport must use a real read transaction with snapshot isolation;
        // it must also bound rows/bytes/time and rollback on any callback error.
        const rows = await tx.query(LIFEGRAPH_NODE_QUERY, { ids: approved.nodes.map(n => n.id) });
        if (!Array.isArray(rows) || rows.length !== approved.nodes.length) fail();
        const byId = new Map();
        for (const row of rows) {
          if (!text(row.id) || byId.has(row.id) || !Array.isArray(row.labels) ||
              !row.labels.every(text) || typeof row.summary !== 'string' || !row.summary.trim() || row.summary.length > 4096) fail();
          byId.set(row.id, { ...row, sources: manifest(row.sources) });
        }
        const nodes = [], edges = [];
        for (const n of approved.nodes) {
          context.signal?.throwIfAborted();
          const row = byId.get(n.id);
          if (!row || !row.labels.includes(n.label) || (n.kind === 'Idea' && !n.id.startsWith('idea:')) ||
              lifeGraphContentDigest(row) !== n.contentDigest ||
              digest([...row.sources].sort()) !== digest([...n.sources].sort())) fail();
          if (n.canonicalId !== undefined) {
            const roots = await tx.query(n.rootBinding.type === 'verifiedKey' ? LIFEGRAPH_ROOT_QUERY : LIFEGRAPH_ALIAS_QUERY,
              { key: n.rootBinding.key });
            if (roots.length !== 1 || roots[0].root !== n.canonicalId) fail();
          }
          nodes.push({ id: n.id, namespace: context.namespace, kind: n.kind, summary: row.summary, sourcePolicyManifest: row.sources,
            ...(n.canonicalId === undefined ? {} : { canonicalId: n.canonicalId }) });
        }
        for (const e of approved.edges) {
          context.signal?.throwIfAborted();
          const rows = await tx.query(LIFEGRAPH_EDGE_QUERY, { from: e.from, to: e.to, relation: e.relation });
          if (rows.length !== 1 || rows[0].from !== e.from || rows[0].to !== e.to || rows[0].relation !== e.relation ||
              digest(manifest(rows[0].sources).sort()) !== digest([...e.sources].sort())) fail();
          edges.push({ id: e.id, namespace: context.namespace, from: e.from, to: e.to, relation: e.relation, sourcePolicyManifest: [...e.sources] });
        }
        nodes.sort((a,b) => a.id.localeCompare(b.id, 'en')); edges.sort((a,b) => a.id.localeCompare(b.id, 'en'));
        context.signal?.throwIfAborted();
        // Content-bound revision avoids inventing a production revision node.
        // Mutations remain coordinated by the owner gate, not by this hash.
        return { namespace: context.namespace, revision: digest({ nodes, edges }), nodes, edges };
      });
    } catch { fail(); }
  } });
}
