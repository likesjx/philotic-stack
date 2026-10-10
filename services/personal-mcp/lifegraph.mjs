// Source-only synthetic LifeGraph admission boundary. No database, credential,
// HTTP, embedding, model, write or grant implementation belongs in this module.
const kinds = new Set(['Goal', 'Idea', 'OpenLoop', 'Person', 'Place', 'Thing']);
const text = (v, max = 128) => typeof v === 'string' && v.trim().length > 0 && v.length <= max;
const strings = (v, max = 32) => Array.isArray(v) && v.length <= max && v.every(x => text(x)) && new Set(v).size === v.length;
const fail = () => { throw new Error('LifeGraph unavailable'); };
const exact = (v, keys) => v && typeof v === 'object' && !Array.isArray(v) && Object.keys(v).every(k => keys.includes(k));
const checkSignal = signal => signal?.throwIfAborted();
async function bounded(work, signal) {
  checkSignal(signal);
  if (!signal) return work();
  return new Promise((resolve, reject) => {
    const stop = () => reject(new Error('LifeGraph unavailable'));
    signal.addEventListener('abort', stop, { once: true });
    Promise.resolve().then(() => { checkSignal(signal); return work(); }).then(resolve, reject)
      .finally(() => signal.removeEventListener('abort', stop));
  });
}
export const lifeGraphDescriptor = Object.freeze({
  name: 'life.recall', description: 'Read approved synthetic LifeGraph goals, ideas, open loops, people, places and things. No writes or semantic deduplication.',
  inputSchema: { type: 'object', additionalProperties: false, required: ['query_text'], properties: {
    query_text: { type: 'string', minLength: 1, maxLength: 4096 },
    max_context_packets: { type: 'integer', minimum: 1, maximum: 12, default: 6 },
  } }, annotations: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
});

function indexSnapshot(snapshot, namespace, check = () => {}) {
  check();
  if (!exact(snapshot, ['namespace', 'revision', 'nodes', 'edges']) || snapshot.namespace !== namespace ||
      !text(snapshot.revision) || !Array.isArray(snapshot.nodes) || snapshot.nodes.length > 2048 ||
      !Array.isArray(snapshot.edges) || snapshot.edges.length > 4096) fail();
  const records = new Map();
  for (const [items, edge] of [[snapshot.nodes, false], [snapshot.edges, true]]) for (const r of items) {
    check();
    if (!exact(r, edge ? ['id', 'namespace', 'from', 'to', 'relation', 'policy'] :
      ['id', 'namespace', 'kind', 'summary', 'canonicalId', 'policy']) || !text(r.id) || records.has(r.id) || r.namespace !== namespace) fail();
    if (edge ? !text(r.from) || !text(r.to) || !text(r.relation) :
      !kinds.has(r.kind) || !text(r.summary, 4096) || (r.canonicalId !== undefined && !text(r.canonicalId))) fail();
    const p = r.policy;
    if (!exact(p, ['owner', 'creator', 'creator_read_grant', 'read_roles', 'private', 'external_operations', 'sources']) ||
        !text(p.owner) || !text(p.creator) || typeof p.creator_read_grant !== 'boolean' ||
        typeof p.private !== 'boolean' || !strings(p.read_roles) || !strings(p.external_operations) || !strings(p.sources, 128)) fail();
    records.set(r.id, r);
  }
  const nodeIds = new Set(snapshot.nodes.map(n => n.id));
  if (snapshot.edges.some(e => !nodeIds.has(e.from) || !nodeIds.has(e.to))) fail();
  return records;
}

// Mirrors ansible-mesh-core/privacy.rs: creator privilege is revocable; read
// permission never implies external processing. Every source remains binding.
function permitted(records, actor, id, check, path = new Set(), budget = { remaining: 4096 }) {
  check();
  if (path.size >= 128 || budget.remaining-- <= 0 || path.has(id)) return false;
  const record = records.get(id), p = record?.policy;
  if (!p || p.private || !p.external_operations.includes('inference') ||
      !(actor.agentId === p.owner || (actor.agentId === p.creator && p.creator_read_grant) ||
        actor.roles.some(role => p.read_roles.includes(role)))) return false;
  path.add(id);
  const dependencies = [...p.sources];
  if (record.canonicalId !== undefined && record.canonicalId !== id) dependencies.push(record.canonicalId);
  if (record.from !== undefined) dependencies.push(record.from, record.to);
  const ok = dependencies.every(source => permitted(records, actor, source, check, path, budget));
  path.delete(id);
  return ok;
}

function project(snapshot, records, actor, args, check) {
  const nodes = new Map(snapshot.nodes.map(n => [n.id, n]));
  const visible = new Map();
  // Canonical resolution precedes matching. Every variant/root in the chain
  // needs authorization. Hidden roots never leak their identity or existence.
  for (const node of nodes.values()) {
    check();
    let root = node; const seen = new Set(); let ok = true;
    while (true) {
      if (seen.size >= 128 || seen.has(root.id) || !permitted(records, actor, root.id, check)) { ok = false; break; }
      seen.add(root.id);
      if (root.canonicalId === undefined || root.canonicalId === root.id) break;
      root = nodes.get(root.canonicalId);
      if (!root) { ok = false; break; }
    }
    if (ok) visible.set(node.id, root);
  }
  const query = args.query_text.trim().toLocaleLowerCase('en-US');
  const roots = new Map();
  for (const [id, root] of visible) { check(); if (nodes.get(id).summary.toLocaleLowerCase('en-US').includes(query)) roots.set(root.id, root); }
  const selected = [...roots.values()].sort((a, b) => { check(); return a.id.localeCompare(b.id, 'en'); }).slice(0, args.max_context_packets ?? 6);
  const selectedIds = new Set(selected.map(n => n.id));
  // Only induced edges: no hidden hops, degrees, scores, counts or diagnostics.
  const edges = snapshot.edges.filter(e => permitted(records, actor, e.id, check) && visible.has(e.from) && visible.has(e.to))
    .map(e => ({ from: visible.get(e.from).id, to: visible.get(e.to).id, relation: e.relation }))
    .filter(e => selectedIds.has(e.from) && selectedIds.has(e.to));
  const uniqueEdges = [...new Map(edges.map(e => [JSON.stringify(e), e])).values()]
    .sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b), 'en'));
  return { packets: selected.map(n => ({ id: n.id, kind: n.kind, summary: n.summary })), edges: uniqueEdges };
}

// authenticate(request) MUST be server-owned OAuth introspection + identity/RBAC
// mapping, not parsed arguments or model hints. snapshot MUST be an immutable,
// transactionally consistent synthetic namespace export with monotonic revision.
export function createLifeGraphAdapter({ profile, resource, clientPolicies, authenticate, snapshot, authorizeRelease, requestDeadlineMs = 5000 }) {
  const url = new URL(resource);
  if (profile !== 'synthetic-read-only-v1' || url.protocol !== 'https:' || url.username || url.password || url.search || url.hash ||
      !url.pathname.endsWith('/mcp') || typeof authenticate !== 'function' || typeof snapshot !== 'function' || typeof authorizeRelease !== 'function' ||
      !Number.isInteger(requestDeadlineMs) || requestDeadlineMs < 1 || requestDeadlineMs > 45000 ||
      !Array.isArray(clientPolicies) || !clientPolicies.length || clientPolicies.length > 16) fail();
  const policies = structuredClone(clientPolicies);
  const clients = new Set(), namespaces = new Set();
  for (const p of policies) {
    if (!exact(p, ['clientId', 'subjects', 'namespace', 'scopes']) || !text(p.clientId) || clients.has(p.clientId) ||
        !strings(p.subjects, 16) || !p.subjects.length || !/^synthetic_[A-Za-z0-9_-]{1,100}$/.test(p.namespace) ||
        namespaces.has(p.namespace) || !Array.isArray(p.scopes) || p.scopes.length !== 1 || p.scopes[0] !== 'life:recall') fail();
    clients.add(p.clientId); namespaces.add(p.namespace);
  }
  async function actorFor(request, signal) {
    const a = structuredClone(await bounded(() => authenticate(request, { signal }), signal));
    const p = policies.find(p => p.clientId === a?.clientId);
    if (!a || a.active !== true || a.audience !== resource || !p?.subjects.includes(a.subject) ||
        a.scope !== 'life:recall' || !text(a.agentId) || !strings(a.roles) || !text(a.grantVersion)) fail();
    return { actor: a, policy: p };
  }
  return Object.freeze({
    descriptor: () => structuredClone(lifeGraphDescriptor),
    async recall(request, args, { signal: parentSignal } = {}) {
      const deadline = performance.now() + requestDeadlineMs;
      const signal = parentSignal ? AbortSignal.any([parentSignal, AbortSignal.timeout(requestDeadlineMs)]) : AbortSignal.timeout(requestDeadlineMs);
      const check = () => { checkSignal(signal); if (performance.now() >= deadline) fail(); };
      try {
        check();
        if (!exact(args, ['query_text', 'max_context_packets']) || !text(args.query_text, 4096) ||
            (args.max_context_packets !== undefined && (!Number.isInteger(args.max_context_packets) || args.max_context_packets < 1 || args.max_context_packets > 12))) fail();
        args = structuredClone(args);
        const initial = await actorFor(request, signal);
        check();
        const context = { namespace: initial.policy.namespace, clientId: initial.actor.clientId, subject: initial.actor.subject, signal };
        const read = async () => structuredClone(await bounded(() => snapshot(context), signal));
        const first = await read();
        check();
        const result = project(first, indexSnapshot(first, context.namespace, check), initial.actor, args, check);
        const last = await read(); indexSnapshot(last, context.namespace, check);
        // Any graph/policy change invalidates pending output. Never retry a
        // denied query against broader authority or return partial stale data.
        if (last.revision !== first.revision || JSON.stringify(last) !== JSON.stringify(first)) fail();
        // Refresh identity after acquiring the second snapshot. The coordinated
        // release authority below validates changes during this awaited call.
        const fresh = await actorFor(request, signal);
        if (JSON.stringify(fresh.actor) !== JSON.stringify(initial.actor)) fail();
        check();
        // Mandatory coordinated final admission: this server authority must
        // validate current graph/policy revision AND current actor/grant under
        // its release barrier. Two independent asynchronous reads are not one
        // authority decision. No permissive fallback is provided.
        if (await bounded(() => authorizeRelease({ request, actor: structuredClone(initial.actor),
          namespace: context.namespace, revision: first.revision, signal }), signal) !== true) fail();
        check();
        return { content: [{ type: 'text', text: JSON.stringify(result) }] };
      } catch { fail(); }
    },
  });
}
