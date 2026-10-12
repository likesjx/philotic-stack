import { test } from 'node:test';
import assert from 'node:assert/strict';
import { DatabaseSync } from 'node:sqlite';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createPersonalMcp } from './gateway.mjs';
import { createLifeGraphGateway } from './lifegraph-gateway.mjs';
import { createLifeGraphStorageAuthority } from './lifegraph-storage.mjs';
const resource = 'https://mcp.example.test/life/mcp', issuerUrl = 'https://identity.example.test';
const policies = ['dot', 'claude'].map(clientId => ({ clientId, subjects: ['synthetic-operator'], namespace: `synthetic_${clientId}`, scopes: ['life:recall'] }));
const actor = clientId => ({ active: true, audience: resource, clientId, subject: 'synthetic-operator', scope: 'life:recall', agentId: `agent-${clientId}`, roles: ['reader'], grantVersion: '1' });
const policy = () => ({ owner: 'operator', creator: 'agent-dot', creator_read_grant: true, read_roles: ['reader'], private: false, external_operations: ['inference'], sources: [] });
async function fixture(t) {
  const dir = await mkdtemp(join(tmpdir(), 'synthetic-lifegateway-')), db = new DatabaseSync(join(dir, 'acl.sqlite'));
  db.exec('CREATE TABLE privacy_revision(singleton INTEGER PRIMARY KEY, revision INTEGER); INSERT INTO privacy_revision VALUES(1,1); CREATE TABLE privacy_policy(resource TEXT PRIMARY KEY, policy_json TEXT)');
  for (const c of ['dot', 'claude']) db.prepare('INSERT INTO privacy_policy VALUES(?,?)').run(c, JSON.stringify(policy()));
  const revoked = new Set(), graphCalls = []; let claimsChange = {}, beforeRelease = () => {}, graphRevision = '1';
  const issuer = { issuer: issuerUrl, ready: async () => {}, inspect: async c => ({ active: !revoked.has(c), iss: issuerUrl,
    aud: resource, sub: 'synthetic-operator', client_id: c, scope: 'life:recall', grant_version: '1',
    iat: Math.floor(Date.now() / 1000) - 1, exp: Math.floor(Date.now() / 1000) + 600, ...claimsChange }) };
  const identityAuthority = { resolve: async () => ({ agentId: 'agent-dot', roles: ['reader'] }) };
  identityAuthority.resolve = async c => ({ agentId: `agent-${c.clientId}`, roles: ['reader'] });
  const storage = createLifeGraphStorageAuthority({ policyDatabase: join(dir, 'acl.sqlite'),
    graphReader: { readSnapshot: async c => { graphCalls.push(c); return { namespace: c.namespace, revision: graphRevision,
      nodes: [{ id: c.clientId, namespace: c.namespace, kind: 'Goal', summary: `synthetic ${c.clientId}` }], edges: [] }; } },
    releaseBarrier: { admit: async (a, check) => { beforeRelease(); return !revoked.has(a.actor.clientId) && check({ graphRevision, actor: actor(a.actor.clientId) }); } } });
  const config = { resource, profile: 'synthetic-read-only-v1', allowedClients: ['dot', 'claude'], allowedSubjects: ['synthetic-operator'], clientPolicies: policies };
  const server = await createLifeGraphGateway({ enabled: true, config, issuer, identityAuthority, storageAuthorityFactory: async () => storage });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(async () => { await new Promise(resolve => server.close(resolve)); db.close(); await rm(dir, { recursive: true, force: true }); });
  const base = `http://127.0.0.1:${server.address().port}`;
  const rpc = (client, method, params) => fetch(base + '/life/mcp', { method: 'POST', headers: { 'content-type': 'application/json', Authorization: 'Bearer ' + client }, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }) });
  return { base, rpc, revoked, graphCalls, db, setClaims: value => { claimsChange = value; }, beforeRelease: f => { beforeRelease = f; } };
}
test('disabled composition initializes no issuer or storage and advertises nothing', async () => {
  let opened = false;
  assert.equal(await createLifeGraphGateway({ storageAuthorityFactory: async () => { opened = true; throw new Error('opened'); } }), null);
  assert.equal(opened, false);
});
test('actual HTTP discovery, both client recalls and no Muninn/write dispatch', async t => {
  const f = await fixture(t), discovery = await (await fetch(f.base + '/.well-known/oauth-protected-resource/life/mcp')).json();
  assert.deepEqual(discovery.scopes_supported, ['life:recall']);
  for (const c of ['dot', 'claude']) {
    const tools = (await (await f.rpc(c, 'tools/list')).json()).result.tools;
    assert.deepEqual(tools.map(t => t.name), ['life.recall']); assert.deepEqual(tools[0].securitySchemes[0].scopes, ['life:recall']);
    const result = (await (await f.rpc(c, 'tools/call', { name: 'life.recall', arguments: { query_text: 'synthetic' } })).json()).result;
    assert.equal(JSON.parse(result.content[0].text).packets[0].id, c);
    for (const name of ['muninn_recall', 'life.observe', 'life.commit', 'graph_query']) assert.equal((await (await f.rpc(c, 'tools/call', { name })).json()).error.code, -32602);
  }
  assert.ok(f.graphCalls.every(c => c.namespace === `synthetic_${c.clientId}` && !('request' in c)));
});
test('memory-only or combined tokens, wrong audience and identity hints cannot reach graph', async t => {
  const f = await fixture(t);
  for (const change of [{ scope: 'memory:recall' }, { scope: 'memory:recall life:recall' }, { aud: [resource] }, { grant_version: undefined }]) {
    f.setClaims(change); assert.equal((await f.rpc('dot', 'tools/call', { name: 'life.recall', arguments: { query_text: 'synthetic' } })).status, 401);
  }
  f.setClaims({}); assert.equal((await f.rpc('dot', 'tools/call', { name: 'life.recall', arguments: { query_text: 'synthetic', roles: ['admin'] } })).status, 503);
  assert.equal(f.graphCalls.length, 0);
});
test('policy or grant revocation inside final admission withholds bytes and isolates clients', async t => {
  const f = await fixture(t); f.beforeRelease(() => f.revoked.add('dot'));
  const denied = await f.rpc('dot', 'tools/call', { name: 'life.recall', arguments: { query_text: 'synthetic' } });
  assert.equal(denied.status, 503); assert.doesNotMatch(await denied.text(), /synthetic dot/);
  assert.equal((await f.rpc('claude', 'tools/call', { name: 'life.recall', arguments: { query_text: 'synthetic' } })).status, 200);
  f.beforeRelease(() => f.db.exec('UPDATE privacy_revision SET revision=revision+1'));
  assert.equal((await f.rpc('claude', 'tools/call', { name: 'life.recall', arguments: { query_text: 'synthetic' } })).status, 503);
});
test('separate LifeGraph profile rejects inherited memory policy configuration', async () => {
  await assert.rejects(createPersonalMcp({ resource, issuer: {}, allowedSubjects: new Set(['synthetic-operator']), allowedClients: new Set(['dot']),
    enabledTools: ['life.recall', 'muninn_recall'], clientPolicies: [{ clientId: 'dot', scopes: ['memory:recall'] }],
    lifeGraph: { enabled: true } }), /separate LifeGraph authority/);
});
