import test from 'node:test';
import assert from 'node:assert/strict';
import { createApprovedLifeGraphGateway } from './lifegraph-production.mjs';
import { createLifeGraphMemgraphReader, lifeGraphContentDigest } from './lifegraph-memgraph.mjs';
import { createLifeGraphAdapter } from './lifegraph.mjs';
import { createPersonalMcp, issuerAdapter } from './gateway.mjs';
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

test('issuer binding adapter pins origin and discovery, uses confidential POST and never sends client roles', async () => {
  const issuer = 'https://issuer.example', binding = issuer + '/oauth/lifegraph-binding', introspection = issuer + '/oauth/introspect';
  const calls = [], options = { issuer, discovery: issuer + '/.well-known/oauth-authorization-server', introspection, binding,
    clientId: 'gateway', clientSecret: 's'.repeat(43), fetchImpl: async (url, init) => {
      calls.push({ url, init });
      if (url.endsWith('oauth-authorization-server')) return Response.json({ issuer, code_challenge_methods_supported: ['S256'], response_types_supported: ['code'],
        grant_types_supported: ['authorization_code'], authorization_endpoint: issuer + '/oauth/authorize', token_endpoint: issuer + '/oauth/token',
        introspection_endpoint: introspection, lifegraph_binding_endpoint: binding });
      return Response.json({ subject: 'synthetic', clientId: 'dot', grantVersion: '0:1', agentId: 'synthetic-agent', roles: ['reader'] });
    } };
  assert.throws(() => issuerAdapter({ ...options, binding: 'https://other.example/binding' }));
  const adapter = issuerAdapter(options); await adapter.ready({ allowPreregistered: true });
  await adapter.resolveBinding({ subject: 'synthetic', clientId: 'dot', resource, grantVersion: '0:1', roles: ['admin'], agentId: 'forged' });
  const call = calls.at(-1); assert.equal(call.url, binding); assert.equal(call.init.method, 'POST'); assert.equal(call.init.redirect, 'error');
  assert.match(call.init.headers.Authorization, /^Basic /);
  assert.deepEqual(Object.fromEntries(call.init.body), { subject: 'synthetic', client_id: 'dot', resource, grant_version: '0:1' });
  await assert.rejects(issuerAdapter({ ...options, binding: issuer + '/other' }).ready({ allowPreregistered: true }), /differs/);
  assert.equal(issuerAdapter({ ...options, binding: undefined }).resolveBinding, undefined);
});
