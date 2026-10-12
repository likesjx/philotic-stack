import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createPersonalMcp } from './gateway.mjs';
const resource = 'https://mcp.example.test/personal/mcp', authority = 'https://identity.example.test';
const policies = ['dot', 'claude'].map(clientId => ({ clientId, subjects: ['synthetic-operator'], vault: 'percival_connection_test', scopes: ['memory:recall'] }));
const config = { resource, enabledTools: ['muninn_recall'], muninnVault: 'percival_connection_test',
  allowedSubjects: new Set(['synthetic-operator']), allowedClients: new Set(['dot', 'claude']), clientPolicies: policies, clock: () => 1100000 };
test('remote policy cannot inherit private vault, LifeGraph or missing client policy', async () => {
  for (const change of [{ muninnVault: 'default' }, { enabledTools: ['muninn_recall', 'life.recall'] }, { clientPolicies: policies.slice(0, 1) }])
    await assert.rejects(createPersonalMcp({ ...config, ...change }), /synthetic per-client/);
});
test('both clients recall synthetic packets; revocation and privacy failures fail closed', async t => {
  const revoked = new Set(), calls = []; let mutation = {}, fail = false, revokeDuringCall = false;
  const server = await createPersonalMcp({ ...config,
    issuer: { issuer: authority, ready: async () => {}, inspect: async token => ({ active: !revoked.has(token), iss: authority,
      aud: resource, sub: 'synthetic-operator', client_id: token, iat: 1000, exp: 1900, scope: 'memory:recall', ...mutation }) },
    upstream: { list: async name => ({ name }), call: async (name, args) => {
      calls.push(args); if (revokeDuringCall) revoked.add('dot');
      return fail ? { content: [{ type: 'image', data: 'private' }] } : { content: [{ type: 'text', text: 'synthetic packet' }], _meta: { private: 'discard' } };
    } } });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => server.close(resolve)));
  const rpc = (client, args = { context: ['synthetic'] }) => fetch(`http://127.0.0.1:${server.address().port}/personal/mcp`, {
    method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + client },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'muninn_recall', arguments: args } }) });
  for (const client of ['dot', 'claude']) {
    const response = await rpc(client); assert.equal(response.status, 200); assert.doesNotMatch(await response.text(), /discard/);
  }
  assert.ok(calls.every(a => a.vault === 'percival_connection_test' && a.read_only === true));
  for (const change of [{ client_id: 'operations' }, { sub: 'other' }, { scope: 'memory:recall life:recall' }, { aud: [resource, 'https://ops.example.test'] }]) {
    mutation = change; assert.equal((await rpc('dot')).status, 401);
  }
  mutation = {}; assert.equal((await rpc('claude', { context: ['synthetic'], vault: 'private' })).status, 403);
  fail = true; const failure = await rpc('dot'); assert.equal(failure.status, 503); assert.doesNotMatch(await failure.text(), /private/); fail = false;
  revokeDuringCall = true; const withheld = await rpc('dot'); assert.equal(withheld.status, 401); assert.doesNotMatch(await withheld.text(), /synthetic packet/);
  revokeDuringCall = false; assert.equal((await rpc('dot')).status, 401); assert.equal((await rpc('claude')).status, 200);
});
