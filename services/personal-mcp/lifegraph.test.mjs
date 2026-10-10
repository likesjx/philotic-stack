import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createLifeGraphAdapter } from './lifegraph.mjs';
const resource = 'https://mcp.example.test/life/mcp';
const clientPolicies = ['dot', 'claude'].map(clientId => ({ clientId, subjects: ['synthetic-operator'],
  namespace: `synthetic_${clientId}`, scopes: ['life:recall'] }));
const actor = clientId => ({ active: true, audience: resource, clientId, subject: 'synthetic-operator',
  scope: 'life:recall', agentId: `agent-${clientId}`, roles: ['reader'], grantVersion: '1' });
const policy = change => ({ owner: 'operator', creator: 'agent-dot', creator_read_grant: true,
  read_roles: ['reader'], private: false, external_operations: ['inference'], sources: [], ...change });
const node = (id, change = {}) => ({ id, namespace: 'synthetic_dot', kind: 'Goal', summary: 'synthetic project', policy: policy(), ...change });
function fixture(options = {}) {
  const calls = [], graph = { namespace: 'synthetic_dot', revision: '1', nodes: [node('root')], edges: [] };
  const adapter = createLifeGraphAdapter({ profile: 'synthetic-read-only-v1', resource, clientPolicies,
    authenticate: async request => actor(request), snapshot: async context => { calls.push(context); return graph; }, ...options });
  return { adapter, graph, calls };
}
const recall = async (adapter, client = 'dot', args = { query_text: 'synthetic' }) => JSON.parse((await adapter.recall(client, args)).content[0].text);

test('explicit synthetic LifeGraph admission never expands memory-only policies', () => {
  for (const change of [ { profile: 'production' }, { clientPolicies: clientPolicies.map(p => ({ ...p, scopes: ['memory:recall'] })) },
    { clientPolicies: clientPolicies.map(p => ({ ...p, scopes: ['memory:recall', 'life:recall'] })) },
    { clientPolicies: clientPolicies.map(p => ({ ...p, namespace: 'default' })) },
    { clientPolicies: clientPolicies.map(p => ({ ...p, namespace: 'synthetic_shared' })) } ])
    assert.throws(() => fixture(change), /^Error: LifeGraph unavailable$/);
});
test('both clients receive only their own synthetic namespace and fixed projection', async () => {
  const adapter = fixture({ snapshot: async c => ({ namespace: c.namespace, revision: '1',
    nodes: [node(c.clientId, { namespace: c.namespace, kind: 'Idea' })], edges: [] }) }).adapter;
  for (const client of ['dot', 'claude']) assert.equal((await recall(adapter, client)).packets[0].id, client);
  assert.deepEqual(Object.keys(adapter.descriptor().inputSchema.properties), ['query_text', 'max_context_packets']);
});
test('creator privilege, role grants, private default deny and external inference are independent', async () => {
  for (const [change, count] of [[{}, 1], [{ read_roles: [] }, 1], [{ read_roles: [], creator_read_grant: false }, 0],
    [{ creator: 'other', read_roles: ['reader'] }, 1], [{ private: true, owner: 'agent-dot' }, 0],
    [{ external_operations: [] }, 0], [{ external_operations: ['embedding'] }, 0]]) {
    const { adapter, graph } = fixture(); graph.nodes[0].policy = policy(change);
    assert.equal((await recall(adapter)).packets.length, count);
  }
});
test('missing, malformed, cyclic and over-depth provenance fails closed', async () => {
  const { adapter, graph } = fixture();
  graph.nodes[0].policy.sources = ['missing']; assert.deepEqual(await recall(adapter), { packets: [], edges: [] });
  graph.nodes.push(node('source', { summary: 'unmatched', policy: policy({ private: true }) }));
  graph.nodes[0].policy.sources = ['source']; assert.equal((await recall(adapter)).packets.length, 0);
  graph.nodes[1].policy = policy({ sources: ['root'] }); assert.equal((await recall(adapter)).packets.length, 0);
  graph.nodes[1].policy = undefined; await assert.rejects(recall(adapter), /LifeGraph unavailable/);
  graph.nodes = Array.from({ length: 130 }, (_, i) => node(`n${i}`, { policy: policy({ sources: i < 129 ? [`n${i + 1}`] : [] }) }));
  assert.equal((await recall(adapter, 'dot', { query_text: 'synthetic', max_context_packets: 12 })).packets.some(n => n.id === 'n0'), false);
});
test('canonical roots collapse variants without mutating records or claiming semantic equivalence', async () => {
  const { adapter, graph } = fixture(); graph.nodes.push(node('variant', { canonicalId: 'root', summary: 'variant query' }));
  const before = structuredClone(graph);
  assert.deepEqual((await recall(adapter, 'dot', { query_text: 'variant' })).packets, [{ id: 'root', kind: 'Goal', summary: 'synthetic project' }]);
  assert.deepEqual(graph, before);
  graph.nodes[0].policy.private = true; assert.deepEqual(await recall(adapter, 'dot', { query_text: 'variant' }), { packets: [], edges: [] });
  graph.nodes.push(node('derivative', { summary: 'variant query', policy: policy({ sources: ['variant'] }) }));
  assert.equal((await recall(adapter, 'dot', { query_text: 'variant' })).packets.length, 0);
});
test('hidden nodes and hidden edge policies cannot affect graph traversal, counts, rank or output', async () => {
  const { adapter, graph } = fixture(); const baseline = await recall(adapter);
  graph.nodes.push(node('secret', { policy: policy({ private: true }) }));
  graph.edges.push({ id: 'secret-edge', namespace: graph.namespace, from: 'root', to: 'secret', relation: 'REVEALS', policy: policy() });
  assert.deepEqual(await recall(adapter), baseline);
  graph.nodes.push(node('visible'));
  graph.edges.push({ id: 'hidden-relation', namespace: graph.namespace, from: 'root', to: 'visible', relation: 'SECRET', policy: policy({ private: true }) });
  assert.deepEqual((await recall(adapter)).edges, []);
  graph.edges[1].policy.private = false;
  assert.deepEqual((await recall(adapter)).edges, [{ from: 'root', to: 'visible', relation: 'SECRET' }]);
});
test('request identity hints, raw queries and writes never reach the graph authority', async () => {
  const { adapter, calls } = fixture();
  for (const change of [{ agentId: 'operator' }, { roles: ['admin'] }, { namespace: 'default' }, { cypher: 'MATCH (n) RETURN n' },
    { read_only: false }, { max_context_packets: 13 }, { query_text: '' }])
    await assert.rejects(recall(adapter, 'dot', { query_text: 'synthetic', ...change }), /LifeGraph unavailable/);
  assert.equal(calls.length, 0);
});
test('wrong audience, scopes, subject, unknown client and namespace mismatch deny', async () => {
  for (const change of [{ audience: [resource] }, { scope: 'memory:recall life:recall' }, { subject: 'other' },
    { active: false }, { clientId: 'operations' }, { agentId: '' }, { roles: undefined }, { grantVersion: undefined }]) {
    const { adapter, calls } = fixture({ authenticate: async () => ({ ...actor('dot'), ...change }) });
    await assert.rejects(recall(adapter), /LifeGraph unavailable/); assert.equal(calls.length, 0);
  }
  await assert.rejects(recall(fixture().adapter, 'claude'), /LifeGraph unavailable/);
});
test('revocation, role loss, grant changes and policy revisions during recall withhold all output', async () => {
  for (const change of [{ active: false }, { roles: [] }, { grantVersion: '2' }]) {
    let count = 0; const { adapter } = fixture({ authenticate: async () => ({ ...actor('dot'), ...(count++ ? change : {}) }) });
    await assert.rejects(recall(adapter), /LifeGraph unavailable/);
  }
  let count = 0; const { adapter } = fixture({ snapshot: async () => ({ namespace: 'synthetic_dot',
    revision: String(count++), nodes: [node('root')], edges: [] }) });
  await assert.rejects(recall(adapter), /LifeGraph unavailable/);
});
test('revoking dot leaves Claude independently authorized', async () => {
  const revoked = new Set(['dot']);
  const { adapter } = fixture({ authenticate: async c => ({ ...actor(c), active: !revoked.has(c) }),
    snapshot: async c => ({ namespace: c.namespace, revision: '1', nodes: [node('root', { namespace: c.namespace })], edges: [] }) });
  await assert.rejects(recall(adapter), /LifeGraph unavailable/); assert.equal((await recall(adapter, 'claude')).packets.length, 1);
});
test('revocation during final snapshot and unversioned policy changes are withheld', async () => {
  let reads = 0, revoked = false;
  const { adapter } = fixture({ authenticate: async () => ({ ...actor('dot'), active: !revoked }),
    snapshot: async () => { if (++reads === 2) revoked = true; return { namespace: 'synthetic_dot', revision: '1', nodes: [node('root')], edges: [] }; } });
  await assert.rejects(recall(adapter), /LifeGraph unavailable/);
  reads = 0;
  const changed = fixture({ snapshot: async () => ({ namespace: 'synthetic_dot', revision: '1',
    nodes: [node('root', { policy: policy({ private: ++reads === 2 }) })], edges: [] }) }).adapter;
  await assert.rejects(recall(changed), /LifeGraph unavailable/);
});
test('backend diagnostics, missing policies, duplicate ids and cancellation are sanitized', async () => {
  await assert.rejects(recall(fixture({ snapshot: async () => { throw new Error('private credentials'); } }).adapter), /^Error: LifeGraph unavailable$/);
  const { adapter, graph } = fixture(); graph.nodes.push(node('root')); await assert.rejects(recall(adapter), /LifeGraph unavailable/);
  const stalled = fixture({ authenticate: async () => new Promise(() => {}), requestDeadlineMs: 10 }).adapter;
  // Timeout signals are unref'ed; keep this test alive while the authority stalls.
  const keepAlive = setTimeout(() => {}, 1000);
  try { await assert.rejects(recall(stalled), /^Error: LifeGraph unavailable$/); } finally { clearTimeout(keepAlive); }
  const controller = new AbortController(); controller.abort();
  await assert.rejects(fixture().adapter.recall('dot', { query_text: 'synthetic' }, { signal: controller.signal }), /LifeGraph unavailable/);
});
