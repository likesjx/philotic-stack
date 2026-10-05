// Synthetic protocol contract, NOT an authorization-server implementation.
// Never import or deploy this fixture. It models the issuer obligations so the
// resource adapter can be exercised end-to-end without real accounts or data.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomBytes, createHash } from 'node:crypto';
import { createPersonalMcp, issuerAdapter, frontdoorAdapter } from './gateway.mjs';

const issuer = 'https://issuer.example.test';
const resource = 'https://mcp.example.test/personal/mcp';
const callback = 'https://client.example.test/callback';
const random = () => randomBytes(32).toString('base64url');
const challenge = value => createHash('sha256').update(value).digest('base64url');
async function listen(t, server) {
  server.listen(0, '127.0.0.1'); await new Promise(resolve => server.once('listening', resolve));
  t.after(() => new Promise(resolve => server.close(resolve)));
  return `http://127.0.0.1:${server.address().port}`;
}
async function fixture(t) {
  const clients = new Map(), codes = new Map(), tokens = new Map(), refreshes = new Map();
  const basic = 'Basic ' + Buffer.from('fixture-introspection:synthetic-secret').toString('base64');
  const metadata = { issuer, authorization_endpoint: issuer + '/authorize', token_endpoint: issuer + '/token',
    introspection_endpoint: issuer + '/introspect', registration_endpoint: issuer + '/register',
    revocation_endpoint: issuer + '/revoke', code_challenge_methods_supported: ['S256'],
    response_types_supported: ['code'], grant_types_supported: ['authorization_code', 'refresh_token'] };
  const base = await listen(t, createServer(async (req, res) => {
    const url = new URL(req.url, issuer);
    const json = (status, data) => { res.writeHead(status, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(data)); };
    let body = '';
    for await (const chunk of req) body += chunk;
    const p = new URLSearchParams(body);
    if (url.pathname === '/.well-known/oauth-authorization-server') return json(200, metadata);
    if (url.pathname === '/register') {
      const input = JSON.parse(body);
      if (input.token_endpoint_auth_method !== 'none' || JSON.stringify(input.redirect_uris) !== JSON.stringify([callback])) {
        return json(400, { error: 'invalid_client_metadata' });
      }
      clients.set('synthetic-client', { redirect: callback });
      return json(201, { client_id: 'synthetic-client', redirect_uris: [callback], token_endpoint_auth_method: 'none' });
    }
    if (url.pathname === '/authorize') {
      const q = url.searchParams;
      if (!clients.has(q.get('client_id')) || q.get('redirect_uri') !== callback || q.get('resource') !== resource ||
          q.get('response_type') !== 'code' || q.get('code_challenge_method') !== 'S256' ||
          !/^[A-Za-z0-9_-]{43}$/.test(q.get('code_challenge')) || !q.get('state') ||
          q.get('scope')?.split(' ').some(scope => !['memory:recall', 'life:recall', 'offline_access'].includes(scope))) {
        return json(400, { error: 'invalid_request' });
      }
      const target = new URL(callback); target.searchParams.set('state', q.get('state'));
      if (q.get('fixture_consent') !== 'allow') target.searchParams.set('error', 'access_denied');
      else {
        const code = random(); codes.set(code, { client: q.get('client_id'), resource, redirect: callback,
          challenge: q.get('code_challenge'), scope: q.get('scope'), expires: Date.now() + 60000 });
        target.searchParams.set('code', code);
      }
      res.writeHead(302, { Location: target.href }); return res.end();
    }
    if (url.pathname === '/token') {
      let grant;
      if (p.get('grant_type') === 'authorization_code') {
        grant = codes.get(p.get('code'));
        if (!grant || grant.expires <= Date.now() || grant.client !== p.get('client_id') || grant.redirect !== p.get('redirect_uri') ||
            grant.resource !== p.get('resource') || grant.challenge !== challenge(p.get('code_verifier') || '')) return json(400, { error: 'invalid_grant' });
        codes.delete(p.get('code'));
        grant.family = random();
      } else if (p.get('grant_type') === 'refresh_token') {
        grant = refreshes.get(p.get('refresh_token'));
        if (!grant || grant.client !== p.get('client_id') || grant.resource !== p.get('resource')) return json(400, { error: 'invalid_grant' });
        if (grant.used) {
          for (const token of tokens.values()) if (token.family === grant.family) token.active = false;
          return json(400, { error: 'invalid_grant' });
        }
        grant.used = true;
      } else return json(400, { error: 'unsupported_grant_type' });
      const now = Math.floor(Date.now() / 1000), access = random(), refresh = random();
      tokens.set(access, { active: true, iss: issuer, aud: resource, sub: 'synthetic-operator', client_id: grant.client,
        iat: now, exp: now + 900, scope: grant.scope, family: grant.family });
      refreshes.set(refresh, { ...grant, used: false });
      return json(200, { access_token: access, token_type: 'Bearer', expires_in: 900, scope: grant.scope, refresh_token: refresh });
    }
    if (url.pathname === '/introspect') {
      if (req.headers.authorization !== basic) return json(401, { error: 'invalid_client' });
      return json(200, tokens.get(p.get('token')) || { active: false });
    }
    if (url.pathname === '/revoke') {
      const token = tokens.get(p.get('token'));
      if (token?.client_id === p.get('client_id')) token.active = false;
      return json(200, {});
    }
    json(404, {});
  }));
  const calls = [];
  const backendBase = await listen(t, createServer(async (req, res) => {
    let body = ''; for await (const chunk of req) body += chunk;
    const request = JSON.parse(body);
    if (req.headers.authorization !== 'Bearer synthetic-backend-grant') { res.writeHead(401); return res.end('{}'); }
    const result = request.method === 'tools/list' ? { tools: ['muninn_recall', 'life.recall', 'life.commit'].map(name => ({
      name, description: 'Synthetic', inputSchema: { type: 'object' } })) } : { content: [{ type: 'text', text: 'synthetic-only result' }] };
    if (request.method === 'tools/call') calls.push(request.params);
    res.setHeader('Content-Type', 'application/json'); res.end(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }));
  }));
  // Only reserved fixture origins are remapped. Production adapters still
  // enforce HTTPS URL policy; no TLS/real provider claim is made by this test.
  const fixtureFetch = (url, options) => {
    const target = new URL(url);
    assert.ok([issuer, 'https://backend.example.test'].includes(target.origin));
    return fetch((target.origin === issuer ? base : backendBase) + target.pathname, options);
  };
  const provider = issuerAdapter({ issuer, discovery: issuer + '/.well-known/oauth-authorization-server',
    introspection: issuer + '/introspect', clientId: 'fixture-introspection', clientSecret: 'synthetic-secret', fetchImpl: fixtureFetch });
  const backend = frontdoorAdapter({ fetchImpl: fixtureFetch, endpoints: Object.fromEntries(['muninn_recall', 'life.recall'].map(name => [name,
    { url: 'https://backend.example.test/mcp', credential: async () => 'synthetic-backend-grant' }])) });
  const gateway = await createPersonalMcp({ resource, issuer: provider, upstream: backend, muninnVault: 'synthetic-vault',
    allowedSubjects: new Set(['synthetic-operator']), allowedClients: new Set(['synthetic-client']) });
  const gatewayBase = await listen(t, gateway);
  const register = () => fetch(base + '/register', { method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ redirect_uris: [callback], token_endpoint_auth_method: 'none' }) });
  await register();
  const verifier = random();
  const authParams = { client_id: 'synthetic-client', redirect_uri: callback, resource, response_type: 'code',
    state: 'synthetic-state', code_challenge: challenge(verifier), code_challenge_method: 'S256',
    scope: 'memory:recall life:recall', fixture_consent: 'allow' };
  const authorize = (changes = {}) => fetch(base + '/authorize?' + new URLSearchParams({ ...authParams, ...changes }), { redirect: 'manual' });
  const exchange = (code, changes = {}) => fetch(base + '/token', { method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' }, body: new URLSearchParams({
      grant_type: 'authorization_code', client_id: 'synthetic-client', redirect_uri: callback, resource, code_verifier: verifier, code, ...changes }) });
  const mint = async () => {
    const response = await authorize(); assert.equal(response.status, 302);
    const url = new URL(response.headers.get('location')); assert.equal(url.searchParams.get('state'), authParams.state);
    const code = url.searchParams.get('code'); const tokens = await (await exchange(code)).json(); return { code, ...tokens };
  };
  const recall = token => fetch(gatewayBase + '/personal/mcp', { method: 'POST', headers: {
    Authorization: 'Bearer ' + token, 'Content-Type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'life.recall', arguments: { query_text: 'synthetic' } } }) });
  return { base, gatewayBase, mint, authorize, exchange, recall, calls };
}
test('synthetic issuer discovery → DCR → consent → PKCE exchange → existing MCP routing → revocation', async t => {
  const f = await fixture(t);
  const metadata = await (await fetch(f.gatewayBase + '/.well-known/oauth-protected-resource/personal/mcp')).json();
  assert.deepEqual(metadata.authorization_servers, [issuer]);
  const minted = await f.mint();
  assert.equal((await f.recall(minted.access_token)).status, 200);
  assert.equal(f.calls.length, 1);
  await fetch(f.base + '/revoke', { method: 'POST', body: new URLSearchParams({ token: minted.access_token, client_id: 'synthetic-client' }) });
  assert.equal((await f.recall(minted.access_token)).status, 401);
  assert.equal(f.calls.length, 1);
});
test('synthetic issuer denies unregistered callback, wrong resource, plain PKCE and write scope', async t => {
  const f = await fixture(t);
  for (const change of [{ redirect_uri: 'https://attacker.example.test/callback' }, { resource: 'https://other.example.test/mcp' },
    { code_challenge_method: 'plain' }, { scope: 'life:write' }]) assert.equal((await f.authorize(change)).status, 400);
});
test('synthetic exchange denies verifier/client/callback/resource mismatch and code replay', async t => {
  const f = await fixture(t);
  const code = new URL((await f.authorize()).headers.get('location')).searchParams.get('code');
  for (const change of [{ code_verifier: random() }, { client_id: 'unregistered' }, { redirect_uri: 'https://other.example.test/callback' },
    { resource: 'https://other.example.test/mcp' }]) assert.equal((await f.exchange(code, change)).status, 400);
  assert.equal((await f.exchange(code)).status, 200);
  assert.equal((await f.exchange(code)).status, 400);
});
test('synthetic consent denial returns state and never issues a code', async t => {
  const f = await fixture(t);
  const url = new URL((await f.authorize({ fixture_consent: 'deny' })).headers.get('location'));
  assert.equal(url.searchParams.get('error'), 'access_denied');
  assert.equal(url.searchParams.get('state'), 'synthetic-state'); assert.equal(url.searchParams.has('code'), false);
});
test('synthetic refresh rotation and replay invalidate the token family', async t => {
  const f = await fixture(t), minted = await f.mint();
  const refresh = () => fetch(f.base + '/token', { method: 'POST', body: new URLSearchParams({ grant_type: 'refresh_token',
    client_id: 'synthetic-client', resource, refresh_token: minted.refresh_token }) });
  const fresh = await (await refresh()).json();
  assert.equal((await f.recall(fresh.access_token)).status, 200);
  assert.equal((await refresh()).status, 400);
  assert.equal((await f.recall(fresh.access_token)).status, 401);
});
