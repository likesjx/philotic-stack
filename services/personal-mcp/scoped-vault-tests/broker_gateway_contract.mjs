// Numeric fixture UID1234 -> actual encrypted hotel broker -> gateway ->
// authenticated unary JSON fixture. No real issuer, Muninn or memory access.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { createPersonalMcp, frontdoorAdapter } from '../gateway.mjs';
import { brokerCredential } from '../vault.mjs';
const resource = 'https://mcp.example/personal/mcp', authority = 'https://identity.example';
const socketPath = '/run/percival-personal-vault-broker.sock';
const introspection = brokerCredential({ socketPath, kind: 'introspection' });
const credential = brokerCredential({ socketPath });
const revoked = new Set(); let calls = 0;
const backend = createServer(async (request, response) => {
  assert.equal(request.url, '/mcp'); assert.equal(request.method, 'POST');
  assert.equal(request.headers.authorization, 'Bearer mk_synthetic');
  const chunks = []; for await (const chunk of request) chunks.push(chunk);
  const rpc = JSON.parse(Buffer.concat(chunks));
  if (rpc.method === 'tools/list') {
    assert.deepEqual(rpc.params, {});
    response.writeHead(200, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify({ jsonrpc: '2.0', id: rpc.id,
      result: { tools: [{ name: 'muninn_recall', description: 'Synthetic recall',
        inputSchema: { type: 'object' } }] } }));
    return;
  }
  assert.equal(rpc.method, 'tools/call'); assert.equal(rpc.params.name, 'muninn_recall');
  assert.equal(rpc.params.arguments.vault, 'percival_connection_test');
  assert.equal(rpc.params.arguments.read_only, true); calls++;
  response.writeHead(200, { 'Content-Type': 'application/json' });
  response.end(JSON.stringify({ jsonrpc: '2.0', id: rpc.id,
    result: { content: [{ type: 'text', text: 'public synthetic packet' }] } }));
});
await new Promise(resolve => backend.listen(0, '127.0.0.1', resolve));
let gateway;
try {
  const upstream = frontdoorAdapter({ enabledTools: ['muninn_recall'], endpoints: {
    muninn_recall: { url: `http://127.0.0.1:${backend.address().port}/mcp`, backendProtocol: 'muninn-unary-json', credential } } });
  gateway = await createPersonalMcp({ resource, clock: () => 1001 * 1000,
    enabledTools: ['muninn_recall'], muninnVault: 'percival_connection_test',
    allowedSubjects: new Set(['synthetic-operator']), allowedClients: new Set(['dot', 'claude']),
    clientPolicies: ['dot', 'claude'].map(clientId => ({ clientId, subjects: ['synthetic-operator'], scopes: ['memory:recall'], vault: 'percival_connection_test' })),
    issuer: { issuer: authority, ready: async () => {}, inspect: async (token, options) => {
      assert.equal(await introspection(options), 'i'.repeat(43));
      return { active: ['dot', 'claude'].includes(token) && !revoked.has(token), iss: authority, aud: resource,
        sub: 'synthetic-operator', client_id: token, iat: 1000, exp: 1900, scope: 'memory:recall' };
    } }, upstream });
  await new Promise(resolve => gateway.listen(0, '127.0.0.1', resolve));
  const rpc = (client, args = { context: ['public synthetic connection'] }, tool = 'muninn_recall') => fetch(`http://127.0.0.1:${gateway.address().port}/personal/mcp`, {
    method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + client },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: tool, arguments: args } }) });
  for (const client of ['dot', 'claude']) {
    const response = await rpc(client); assert.equal(response.status, 200);
    assert.match(await response.text(), /public synthetic packet/);
  }
  assert.equal((await rpc('dot', { context: ['synthetic'], vault: 'user_likesjx' })).status, 403);
  const disabled = await rpc('dot', { context: ['synthetic'] }, 'life.recall');
  assert.equal(disabled.status, 200);
  const disabledBody = await disabled.json();
  assert.equal(disabledBody.error.code, -32602);
  assert.equal(disabledBody.result, undefined);
  assert.equal(calls, 2);
  revoked.add('dot'); assert.equal((await rpc('dot')).status, 401);
  assert.equal((await rpc('claude')).status, 200); assert.equal(calls, 3);
  console.log('PASS actual hotel broker to two-client gateway to authenticated unary JSON fixture; private vault, LifeGraph and revoked client denied');
} finally {
  if (gateway) await new Promise(resolve => gateway.close(resolve));
  await new Promise(resolve => backend.close(resolve));
}
