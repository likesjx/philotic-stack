import { test } from 'node:test';
import assert from 'node:assert/strict';
import { start } from './server.mjs';
import { createDiscoveryBootstrap } from './gateway.mjs';

test('discovery-only serves public metadata without credentials, issuer calls or private RPC', async t => {
  const server = createDiscoveryBootstrap({ resource: 'https://mcp.example.test/personal/mcp', issuer: 'https://identity.example.test' });
  server.listen(0, '127.0.0.1'); await new Promise(resolve => server.once('listening', resolve));
  t.after(() => new Promise(resolve => server.close(resolve)));
  const base = `http://127.0.0.1:${server.address().port}`;
  const metadata = await (await fetch(base + '/.well-known/oauth-protected-resource/personal/mcp')).json();
  assert.deepEqual(metadata.scopes_supported, ['memory:recall']);
  assert.deepEqual(metadata.authorization_servers, ['https://identity.example.test']);
  for (const method of ['initialize', 'tools/list', 'tools/call']) {
    const response = await fetch(base + '/personal/mcp', { method: 'POST', headers: { Authorization: 'Bearer synthetic', 'Content-Type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method }) });
    assert.equal(response.status, 401);
    assert.deepEqual(await response.json(), { error: 'setup_incomplete' });
  }
  for (const path of ['/.well-known/oauth-protected-resource', '/oauth/token', '/oauth/authorize', '/.well-known/oauth-protected-resource/personal/mcp?unexpected=1']) {
    assert.equal((await fetch(base + path)).status, 404);
  }
});
test('composition rejects unknown mode before secrets or upstream access', async () => {
  const env = new Proxy({}, { get() { throw new Error('Secret access forbidden'); } });
  await assert.rejects(start({ port: 8913, mode: 'typo' }, env), /Unknown mode/);
});
