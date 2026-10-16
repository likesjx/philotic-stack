import test from 'node:test';
import assert from 'node:assert/strict';
import { createApprovedLifeGraphGateway } from './lifegraph-production.mjs';
import { createLifeGraphMemgraphReader, lifeGraphContentDigest } from './lifegraph-memgraph.mjs';
import { createLifeGraphAdapter } from './lifegraph.mjs';
import { createPersonalMcp } from './gateway.mjs';
const resource = 'https://mcp.example/life/mcp';
test('production composition refuses missing canonical identity before opening storage', async () => {
  for (const config of [undefined, { profile: 'synthetic-read-only-v1' }, { profile: 'approved-read-only-v1', resource, storage: {} }])
    await assert.rejects(createApprovedLifeGraphGateway({ config, issuer: {} }), /canonical LifeGraph identity/);
});
test('approved catalog uses exact content bindings and cannot select a graph-wide namespace', async () => {
  const row = { id: 'goal:fixture', labels: ['Goal'], summary: 'synthetic approved context', sources: [] };
  const catalog = { namespace: 'approved_dot', nodes: [{ id: row.id, kind: 'Goal', label: 'Goal', sources: [], contentDigest: lifeGraphContentDigest(row) }], edges: [] };
  let calls = 0;
  const transport = { readTransaction: async (_context, work) => work({ query: async (_query, args) => { calls++; assert.deepEqual(args.ids, [row.id]); return [row]; } }) };
  assert.throws(() => createLifeGraphMemgraphReader({ transport, catalogs: [catalog] }));
  const reader = createLifeGraphMemgraphReader({ profile: 'approved-read-only-v1', transport, catalogs: [catalog] });
  assert.equal((await reader.readSnapshot({ namespace: 'approved_dot' })).nodes[0].id, row.id);
  await assert.rejects(reader.readSnapshot({ namespace: 'approved_claude' })); assert.equal(calls, 1);
  row.summary = 'changed'; await assert.rejects(reader.readSnapshot({ namespace: 'approved_dot' }));
});
test('approved projection keeps private and inherited source policy denial', async () => {
  const policy = { owner: 'agent', creator: 'agent', creator_read_grant: true, read_roles: [], private: false, external_operations: ['inference'], sources: [] };
  const nodes = [
    { id: 'visible', namespace: 'approved_dot', kind: 'Goal', summary: 'fixture visible', policy },
    { id: 'private', namespace: 'approved_dot', kind: 'Person', summary: 'fixture hidden', policy: { ...policy, private: true } },
    { id: 'derived', namespace: 'approved_dot', kind: 'Idea', summary: 'fixture derived', policy: { ...policy, sources: ['private'] } },
  ];
  const adapter = createLifeGraphAdapter({ profile: 'approved-read-only-v1', resource,
    clientPolicies: [{ clientId: 'dot', subjects: ['operator'], namespace: 'approved_dot', scopes: ['life:recall'] }],
    authenticate: async () => ({ active: true, audience: resource, clientId: 'dot', subject: 'operator', scope: 'life:recall', agentId: 'agent', roles: [], grantVersion: '0:1' }),
    snapshot: async () => ({ namespace: 'approved_dot', revision: '1', nodes, edges: [] }), authorizeRelease: async () => true });
  const result = JSON.parse((await adapter.recall({}, { query_text: 'fixture' })).content[0].text);
  assert.deepEqual(result.packets.map(p => p.id), ['visible']);
});
test('production gateway requires final identity and response lifetime authorities', async () => {
  await assert.rejects(createPersonalMcp({ resource, issuer: {}, allowedSubjects: new Set(['operator']), allowedClients: new Set(['dot']), enabledTools: ['life.recall'],
    lifeGraph: { enabled: true, profile: 'approved-read-only-v1', identityAuthority: { resolve() {} }, storageAuthority: { snapshot() {}, authorizeRelease() {} }, clientPolicies: [] } }), /response lifetime/);
});
