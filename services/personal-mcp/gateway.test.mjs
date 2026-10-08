import { test } from 'node:test';
import { request as httpRequest } from 'node:http';
import assert from 'node:assert/strict';
import { createPersonalMcp, issuerAdapter, frontdoorAdapter } from './gateway.mjs';

const resource = 'https://mcp.example.test/personal/mcp';
const authority = 'https://identity.example.test';
const claims = () => ({ active: true, iss: authority, aud: resource, sub: 'synthetic-operator', client_id: 'synthetic-client',
  iat: 1000, exp: 1900, scope: 'memory:recall life:recall' });
async function fixture(t, mutate = value => value, options = {}) {
  const calls = []; let active = true;
  const server = await createPersonalMcp({ resource, enabledTools: options.enabledTools ?? ['muninn_recall', 'life.recall'], clock: () => 1100000, muninnVault: 'default', requestDeadlineMs: options.requestDeadlineMs ?? 45000,
    allowedSubjects: new Set(['synthetic-operator']), allowedClients: new Set(['synthetic-client']),
    issuer: { issuer: authority, ready: async () => {}, inspect: async (token, requestOptions) => {
      if (options.inspect) return options.inspect(token, requestOptions);
      if (token !== 'synthetic-access-token') return { active: false };
      return mutate({ ...claims(), active });
    } }, upstream: {
      list: async name => { options.list?.(name); return ({ name, description: 'Synthetic context', inputSchema: { type: 'object' } }); },
      call: async (name, args, requestOptions) => { calls.push({ name, args }); return options.call ? options.call(name, args, requestOptions) : { content: [{ type: 'text', text: 'synthetic packet' }] }; },
    } });
  server.listen(0, '127.0.0.1'); await new Promise(resolve => server.once('listening', resolve));
  t.after(() => new Promise(resolve => server.close(resolve)));
  const base = `http://127.0.0.1:${server.address().port}`;
  const rpc = (method, params = {}, headers = {}) => fetch(base + '/personal/mcp', { method: 'POST',
    headers: { 'Content-Type': 'application/json', Authorization: 'Bearer synthetic-access-token', ...headers },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }) });
  return { base, rpc, calls, revoke: () => { active = false; } };
}
test('discovery and unauthenticated challenge bind to the exact resource', async t => {
  const f = await fixture(t);
  const metadata = await fetch(f.base + '/.well-known/oauth-protected-resource/personal/mcp');
  assert.equal((await metadata.json()).resource, resource);
  const response = await f.rpc('initialize', {}, { Authorization: '' });
  assert.equal(response.status, 401);
  assert.match(response.headers.get('www-authenticate'), /oauth-protected-resource\/personal\/mcp/);
});
test('initialization, recall tools, scope metadata and synthetic routing', async t => {
  const f = await fixture(t);
  assert.equal((await (await f.rpc('initialize', { protocolVersion: '2025-11-25' })).json()).result.protocolVersion, '2025-11-25');
  const tools = (await (await f.rpc('tools/list')).json()).result.tools;
  assert.deepEqual(tools.map(tool => tool.name), ['muninn_recall', 'life.recall']);
  assert.equal(tools[0].annotations.readOnlyHint, true);
  assert.deepEqual(tools[0].securitySchemes[0].scopes, ['memory:recall']);
  assert.equal((await f.rpc('tools/call', { name: 'muninn_recall', arguments: { context: ['synthetic'] } })).status, 200);
  assert.equal(f.calls[0].args.read_only, true);
  assert.equal(f.calls[0].args.vault, 'default');
  assert.equal((await f.rpc('tools/call', { name: 'life.recall', arguments: { query_text: 'synthetic' } })).status, 200);
});
test('client cannot escape the operator-selected Muninn vault', async t => {
  const f = await fixture(t);
  assert.equal((await f.rpc('tools/call', { name: 'muninn_recall', arguments: { vault: 'other-vault' } })).status, 403);
  assert.equal(f.calls.length, 0);
});
test('scope projection and execution are enforced independently', async t => {
  const f = await fixture(t, token => ({ ...token, scope: 'memory:recall' }));
  assert.deepEqual((await (await f.rpc('tools/list')).json()).result.tools.map(tool => tool.name), ['muninn_recall']);
  assert.equal((await f.rpc('tools/call', { name: 'life.recall' })).status, 403);
  assert.equal(f.calls.length, 0);
});
test('write/admin tools cannot be dispatched even with broad issuer scopes', async t => {
  const f = await fixture(t, token => ({ ...token, scope: '* memory:recall life:recall life:write' }));
  for (const name of ['muninn_remember', 'muninn_decide', 'life.observe', 'life.commit', 'session_start', 'graph_decide']) {
    const result = await (await f.rpc('tools/call', { name })).json();
    assert.equal(result.error.code, -32602);
  }
  assert.equal(f.calls.length, 0);
});
for (const [name, changes] of Object.entries({
  inactive: { active: false }, issuer: { iss: 'https://wrong.test' }, audience: { aud: authority },
  subject: { sub: 'someone-else' }, client: { client_id: 'unapproved' }, expired: { exp: 1100 },
  future: { iat: 1101 }, lifetime: { exp: 1901 }, missingAudience: { aud: undefined }, missingExpiry: { exp: undefined },
})) test(`reject ${name} before dispatch`, async t => {
  const f = await fixture(t, token => ({ ...token, ...changes }));
  assert.equal((await f.rpc('tools/call', { name: 'life.recall' })).status, 401);
  assert.equal(f.calls.length, 0);
});
test('revocation affects next call without a cached authorization', async t => {
  const f = await fixture(t);
  assert.equal((await f.rpc('tools/list')).status, 200);
  f.revoke(); assert.equal((await f.rpc('tools/list')).status, 401);
});
test('authority lost during recall prevents releasing personal content', async t => {
  let checks = 0;
  const f = await fixture(t, token => ({ ...token, active: ++checks === 1 }));
  const response = await f.rpc('tools/call', { name: 'life.recall', arguments: { query_text: 'synthetic' } });
  assert.equal(response.status, 401);
  assert.equal(f.calls.length, 1);
  assert.doesNotMatch(await response.text(), /synthetic packet/);
});
test('issuer outage fails closed and does not reveal provider diagnostics', async t => {
  const f = await fixture(t, () => { throw new Error('synthetic-secret-do-not-disclose'); });
  const response = await f.rpc('tools/list');
  assert.equal(response.status, 503);
  assert.doesNotMatch(await response.text(), /synthetic-secret/);
  assert.equal(f.calls.length, 0);
});
test('oversized body and non-JSON content cannot reach a tool', async t => {
  const f = await fixture(t);
  assert.equal((await f.rpc('tools/call', { name: 'life.recall', arguments: { query_text: 'x'.repeat(33000) } })).status, 413);
  assert.equal((await f.rpc('tools/list', {}, { 'Content-Type': 'text/plain' })).status, 415);
  assert.equal(f.calls.length, 0);
});
test('cross-origin, protocol version, array batch and unknown methods denied', async t => {
  const f = await fixture(t);
  assert.equal((await f.rpc('tools/list', {}, { Origin: 'https://attacker.test' })).status, 403);
  assert.equal((await f.rpc('tools/list', {}, { 'MCP-Protocol-Version': 'unknown' })).status, 400);
  assert.equal((await (await f.rpc('resources/read')).json()).error.code, -32601);
  assert.equal((await fetch(f.base + '/personal/mcp', { method: 'POST', headers: { Authorization: 'Bearer synthetic-access-token',
    'Content-Type': 'application/json' }, body: '[]' })).status, 400);
});
const providerConfig = { issuer: authority, discovery: authority + '/.well-known/oauth-authorization-server',
  introspection: authority + '/introspect', clientId: 'fixture', clientSecret: 'synthetic-introspection-secret-000000000000000'  };
const providerMetadata = { issuer: authority, authorization_endpoint: authority + '/authorize', token_endpoint: authority + '/token',
  introspection_endpoint: authority + '/introspect', registration_endpoint: authority + '/register',
  code_challenge_methods_supported: ['S256'], response_types_supported: ['code'], grant_types_supported: ['authorization_code'] };
test('issuer adapter validates PKCE discovery and uses confidential introspection', async () => {
  const requests = [];
  const adapter = issuerAdapter({ ...providerConfig, fetchImpl: async (url, options) => {
    requests.push({ url, options }); return Response.json(url.endsWith('/introspect') ? claims() : providerMetadata);
  } });
  await adapter.ready(); await adapter.inspect('synthetic-access-token');
  assert.equal(requests[1].options.redirect, 'error');
  assert.match(requests[1].options.headers.Authorization, /^Basic /);
  assert.equal(requests[1].options.body.get('token'), 'synthetic-access-token');
});
for (const [name, changes] of Object.entries({ noPkce: { code_challenge_methods_supported: ['plain'] },
  wrongIssuer: { issuer: resource }, noRegistration: { registration_endpoint: undefined },
  wrongIntrospection: { introspection_endpoint: authority + '/other' }, insecureToken: { token_endpoint: 'http://identity.example.test/token' },
})) test(`issuer startup rejects ${name}`, async () => {
  const adapter = issuerAdapter({ ...providerConfig, fetchImpl: async () => Response.json({ ...providerMetadata, ...changes }) });
  await assert.rejects(adapter.ready());
});
test('frontdoor selects per-tool credentials, checks response IDs and never forwards OAuth tokens', async () => {
  const requests = [];
  const endpoints = Object.fromEntries(Object.keys({ muninn_recall: 1, 'life.recall': 1 }).map(name => [name,
    { url: 'http://127.0.0.1:9999/mcp', credential: async () => 'synthetic-scoped-backend' }]));
  const adapter = frontdoorAdapter({ endpoints, enabledTools: ['muninn_recall', 'life.recall'], fetchImpl: async (url, options) => {
    requests.push(options); const request = JSON.parse(options.body);
    return Response.json({ jsonrpc: '2.0', id: request.id, result: { content: [] } });
  } });
  await adapter.call('life.recall', { query_text: 'synthetic' });
  assert.equal(requests[0].headers.Authorization, 'Bearer synthetic-scoped-backend');
  assert.equal(JSON.parse(requests[0].body).params.name, 'life.recall');
  const broken = frontdoorAdapter({ endpoints, enabledTools: ['muninn_recall', 'life.recall'], fetchImpl: async () => Response.json({ jsonrpc: '2.0', id: 'wrong', result: {} }) });
  await assert.rejects(broken.call('life.recall', {}));
});
test('bad configuration cannot create a gateway', async () => {
  assert.throws(() => issuerAdapter({ ...providerConfig, introspection: 'https://other.test/introspect' }));
  await assert.rejects(createPersonalMcp({ resource, allowedSubjects: new Set(), allowedClients: new Set(['client']) }));
});
test('pre-registered clients need neither DCR nor CIMD metadata', async () => {
  const adapter = issuerAdapter({ ...providerConfig, fetchImpl: async () => Response.json({ ...providerMetadata, registration_endpoint: undefined }) });
  await adapter.ready({ allowPreregistered: true });
});
test('exact issuer trailing slash is preserved in discovery and token authority', async () => {
  const exactIssuer = authority + '/';
  const adapter = issuerAdapter({ ...providerConfig, issuer: exactIssuer,
    fetchImpl: async () => Response.json({ ...providerMetadata, issuer: exactIssuer }) });
  assert.equal(adapter.issuer, exactIssuer); await adapter.ready();
});
test('RFC8414 omitted grant types defaults to authorization_code support', async () => {
  const adapter = issuerAdapter({ ...providerConfig, fetchImpl: async () => Response.json({ ...providerMetadata, grant_types_supported: undefined }) });
  await adapter.ready();
});
test('initialized notification rejects unsupported protocol version', async t => {
  const f = await fixture(t);
  const response = await fetch(f.base + '/personal/mcp', { method: 'POST', headers: { Authorization: 'Bearer synthetic-access-token',
    'Content-Type': 'application/json', 'MCP-Protocol-Version': 'unsupported' },
    body: JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' }) });
  assert.equal(response.status, 400);
});
test('bounded curated recall arguments deny unknown fields and expensive queries', async t => {
  const f = await fixture(t);
  for (const arguments_ of [{ context: [] }, { context: ['x'], limit: 100000 }, { context: ['x'], limit: -1 },
    { context: ['x'.repeat(2049)] }, { context: ['x'], threshold: 2 }, { context: ['x'], read_only: false },
    { context: Array(17).fill('x') }, { context: ['x'], filters: { bypass: true } }]) {
    assert.equal((await f.rpc('tools/call', { name: 'muninn_recall', arguments: arguments_ })).status, 400);
  }
  for (const arguments_ of [{ query_text: '' }, { query_text: 'x'.repeat(4097) }, { query_text: 'x', max_context_packets: 100000 },
    { query_text: 'x', operator_intent: 'arbitrary' }, { query_text: 'x', extra: true }, { query_text: 'x', query_id: 'x'.repeat(129) }]) {
    assert.equal((await f.rpc('tools/call', { name: 'life.recall', arguments: arguments_ })).status, 400);
  }
  assert.equal(f.calls.length, 0);
});
test('production factory requires an explicitly selected vault', async () => {
  await assert.rejects(createPersonalMcp({ resource, issuer: {}, upstream: {},
    allowedSubjects: new Set(['operator']), allowedClients: new Set(['client']) }), /vault/);
});
test('backend tool errors suppress raw diagnostics and structured error data', async t => {
  const f = await fixture(t, value => value, { call: async () => ({ isError: true,
    content: [{ type: 'text', text: 'synthetic secret internal socket /private/data' }], structuredContent: { secret: 'sensitive diagnostic' } }) });
  const response = await f.rpc('tools/call', { name: 'life.recall', arguments: { query_text: 'synthetic' } });
  assert.equal(response.status, 200);
  const message = await response.json(); assert.equal(message.result.isError, true);
  assert.equal(message.result.content[0].text, 'Recall unavailable');
  assert.doesNotMatch(JSON.stringify(message), /secret|socket|private|sensitive/);
});
test('malformed backend error/result shapes are not released', async t => {
  const f = await fixture(t, value => value, { call: async () => ({ isError: 'true',
    content: [{ type: 'text', text: 'synthetic-private-diagnostic' }] }) });
  const response = await f.rpc('tools/call', { name: 'life.recall', arguments: { query_text: 'synthetic' } });
  assert.equal(response.status, 503);
  assert.doesNotMatch(await response.text(), /synthetic-private/);
});
test('discovery and recently verified callers retain admission during unknown-token pressure', async t => {
  const release = [];
  let entered = 0, allEntered;
  const barrier = new Promise(resolve => { allEntered = resolve; });
  const f = await fixture(t, value => value, { inspect: async token => {
    if (token === 'synthetic-access-token') return claims();
    entered++; if (entered === 8) allEntered();
    return new Promise(resolve => { release.push(() => resolve({ active: false })); });
  } });
  assert.equal((await f.rpc('tools/list')).status, 200); // establishes scheduling hint, not cached authority
  const pending = Array.from({ length: 8 }, (_, i) => f.rpc('tools/list', {}, { Authorization: 'Bearer unknown-' + i }));
  await barrier;
  try {
    assert.equal((await fetch(f.base + '/.well-known/oauth-protected-resource')).status, 200);
    assert.equal((await f.rpc('tools/list', {}, { Authorization: 'Bearer excess-unknown' })).status, 429);
    assert.equal((await f.rpc('tools/list')).status, 200);
    assert.equal((await f.rpc('tools/list', {}, { Authorization: '' })).status, 401);
  } finally { for (const resolve of release) resolve(); await Promise.all(pending); }
});

test('successful recall drops top-level and text-item diagnostic extras', async t => {
  const f = await fixture(t, value => value, { call: async () => ({ isError: false,
    content: [{ type: 'text', text: 'approved recall', debug: 'private-diagnostic',
      _meta: { secret: 'private-diagnostic' }, annotations: { debug: 'private-diagnostic' } }],
    debug: 'private-diagnostic', structuredContent: { secret: 'private-diagnostic' },
    _meta: { secret: 'private-diagnostic' } }) });
  const response = await f.rpc('tools/call', { name: 'life.recall', arguments: { query_text: 'synthetic' } });
  assert.equal(response.status, 200);
  const message = await response.json();
  assert.deepEqual(message.result, { isError: false, content: [{ type: 'text', text: 'approved recall' }] });
  assert.doesNotMatch(JSON.stringify(message), /private-diagnostic|debug|secret|structuredContent|_meta|annotations/);
});

test('Muninn-only startup, metadata and broad-token calls never touch LifeGraph', async t => {
  const listed = [];
  const f = await fixture(t, value => ({ ...value, scope: '* memory:recall life:recall' }), { enabledTools: ['muninn_recall'], list: name => listed.push(name) });
  assert.deepEqual(listed, ['muninn_recall']);
  const metadata = await (await fetch(f.base + '/.well-known/oauth-protected-resource/personal/mcp')).json();
  assert.deepEqual(metadata.scopes_supported, ['memory:recall']);
  const denied = await f.rpc('tools/list', {}, { Authorization: '' });
  assert.doesNotMatch(denied.headers.get('www-authenticate'), /life:recall/);
  const tools = (await (await f.rpc('tools/list')).json()).result.tools;
  assert.deepEqual(tools.map(tool => tool.name), ['muninn_recall']);
  assert.equal((await (await f.rpc('tools/call', { name: 'life.recall', arguments: { query_text: 'excluded' } })).json()).error.code, -32602);
  assert.equal(f.calls.length, 0);
  await f.rpc('tools/call', { name: 'muninn_recall', arguments: { context: ['synthetic'] } });
  assert.deepEqual(f.calls[0], { name: 'muninn_recall', args: { context: ['synthetic'], vault: 'default', read_only: true, limit: 10 } });
});
test('default backend requires only Muninn and refuses disabled tools before credential access', async () => {
  let credentialReads = 0, fetches = 0;
  const backend = frontdoorAdapter({ endpoints: { muninn_recall: { url: 'http://127.0.0.1:9999/mcp', credential: async () => { credentialReads++; return 'synthetic'; } } },
    fetchImpl: async (_url, init) => { fetches++; const rpc = JSON.parse(init.body); return Response.json({ jsonrpc: '2.0', id: rpc.id, result: { tools: [{ name: 'muninn_recall', inputSchema: {} }] } }); } });
  await assert.rejects(backend.call('life.recall', {}));
  assert.equal(credentialReads, 0); assert.equal(fetches, 0);
  await backend.list('muninn_recall');
  assert.equal(credentialReads, 1); assert.equal(fetches, 1);
});
for (const enabledTools of [[], ['unknown'], ['muninn_recall', 'muninn_recall'], 'muninn_recall']) {
  test(`invalid enabled tool selection rejected: ${JSON.stringify(enabledTools)}`, async () => {
    assert.throws(() => frontdoorAdapter({ endpoints: {}, enabledTools }));
    await assert.rejects(createPersonalMcp({ resource, enabledTools }));
  });
}

test('explicit Muninn unary JSON profile rejects SSE and other backend profiles', async () => {
  const endpoints = { muninn_recall: { url: 'http://127.0.0.1:8750/mcp', backendProtocol: 'muninn-unary-json', credential: async () => 'mk_synthetic' } };
  const adapter = frontdoorAdapter({ endpoints, fetchImpl: async () => new Response('data: private diagnostic', { headers: { 'content-type': 'text/event-stream' } }) });
  await assert.rejects(adapter.list('muninn_recall'), /upstream_unavailable/);
  assert.throws(() => frontdoorAdapter({ endpoints: { muninn_recall: { ...endpoints.muninn_recall, backendProtocol: 'general-streamable-mcp' } } }));
  assert.throws(() => frontdoorAdapter({ endpoints: { muninn_recall: { ...endpoints.muninn_recall, url: 'http://127.0.0.1:8750/other' } } }));
  const calls = [];
  const unary = frontdoorAdapter({ endpoints, fetchImpl: async (_url, init) => {
    const request = JSON.parse(init.body); calls.push(request.method);
    assert.equal(init.headers.Accept, 'application/json');
    return Response.json({ jsonrpc: '2.0', id: request.id, result: { tools: [{ name: 'muninn_recall', inputSchema: {} }] } });
  } });
  await unary.list('muninn_recall'); assert.deepEqual(calls, ['tools/list']);
});

const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
test('one deadline bounds stalled initial authorization and releases request admission', async t => {
  let signal, stall = true;
  const f = await fixture(t, value => value, { requestDeadlineMs: 60,
    inspect: async (_token, options) => { signal = options.signal; return stall ? new Promise(() => {}) : claims(); } });
  const response = await f.rpc('tools/list');
  assert.equal(response.status, 504); assert.equal((await response.json()).error, 'request_timeout');
  assert.equal(signal.aborted, true); assert.equal(f.calls.length, 0);
  stall = false; assert.equal((await f.rpc('tools/list')).status, 200);
});
test('overall deadline is not reset after initial authorization or before final authorization', async t => {
  let inspections = 0, callSignal;
  const f = await fixture(t, value => value, { requestDeadlineMs: 90,
    inspect: async () => { inspections++; await delay(35); return claims(); },
    call: async (_name, _args, { signal }) => { callSignal = signal; await delay(35); return { content: [{ type: 'text', text: 'must not escape' }] }; } });
  const response = await f.rpc('tools/call', { name: 'muninn_recall', arguments: { context: ['synthetic'] } });
  assert.equal(response.status, 504); assert.doesNotMatch(await response.text(), /must not escape/);
  assert.equal(inspections, 2); assert.equal(callSignal.aborted, true);
});
test('deadline aborts stalled upstream and late completion cannot release recalled content', async t => {
  let finish, signal;
  const f = await fixture(t, value => value, { requestDeadlineMs: 60,
    call: async (_name, _args, options) => { signal = options.signal; return new Promise(resolve => { finish = resolve; }); } });
  const response = await f.rpc('tools/call', { name: 'muninn_recall', arguments: { context: ['synthetic'] } });
  assert.equal(response.status, 504); assert.equal(signal.aborted, true);
  finish({ content: [{ type: 'text', text: 'synthetic late result' }] });
  assert.doesNotMatch(await response.text(), /late result/);
});
test('total deadline bounds slowly supplied request body', async t => {
  const f = await fixture(t, value => value, { requestDeadlineMs: 60 });
  const response = await new Promise((resolve, reject) => {
    const req = httpRequest(f.base + '/personal/mcp', { method: 'POST', headers: {
      'Content-Type': 'application/json', Authorization: 'Bearer synthetic-access-token', 'Content-Length': '100' } }, res => {
      let body = ''; res.on('data', chunk => { body += chunk; }); res.on('end', () => resolve({ status: res.statusCode, body }));
    });
    req.on('error', reject); req.write('{');
  });
  assert.equal(response.status, 504); assert.equal(JSON.parse(response.body).error, 'request_timeout');
  assert.equal(f.calls.length, 0);
});
test('request signal stops upstream credential retrieval before fetch', async () => {
  const controller = new AbortController(); let fetches = 0;
  const upstream = frontdoorAdapter({ enabledTools: ['muninn_recall'], endpoints: {
    muninn_recall: { url: 'http://127.0.0.1:8750/mcp', backendProtocol: 'muninn-unary-json',
      credential: async ({ signal }) => { assert.equal(signal, controller.signal); return new Promise(() => {}); } } },
    fetchImpl: async () => { fetches++; throw new Error('unexpected fetch'); } });
  const pending = upstream.call('muninn_recall', {}, { signal: controller.signal });
  controller.abort(new Error('synthetic cancellation'));
  await assert.rejects(pending, /synthetic cancellation/); assert.equal(fetches, 0);
});

test('disconnect after complete request body cancels in-flight backend work', async t => {
  let started, stopped;
  const entered = new Promise(resolve => { started = resolve; });
  const aborted = new Promise(resolve => { stopped = resolve; });
  const f = await fixture(t, value => value, { call: async (_name, _args, { signal }) => {
    signal.addEventListener('abort', stopped, { once: true }); started(); return new Promise(() => {});
  } });
  const body = JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/call',
    params: { name: 'muninn_recall', arguments: { context: ['synthetic'] } } });
  const req = httpRequest(f.base + '/personal/mcp', { method: 'POST', headers: {
    Authorization: 'Bearer synthetic-access-token', 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(body) } });
  req.on('error', () => {}); req.end(body); await entered; req.destroy();
  await Promise.race([aborted, delay(1000).then(() => { throw new Error('backend was not cancelled'); })]);
});

test('issuer fetches introspection vault credential afresh and fails closed before HTTP', async () => {
  let lookups = 0, calls = 0;
  const credential = async () => { lookups++; if (lookups === 3) throw new Error('private vault diagnostic'); return 'i'.repeat(43); };
  const adapter = issuerAdapter({ ...providerConfig, clientSecret: undefined, credential, fetchImpl: async (_url, init) => {
    calls++; assert.equal(init.headers.Authorization, 'Basic ' + Buffer.from('fixture:' + 'i'.repeat(43)).toString('base64'));
    return Response.json({ active: false });
  } });
  await adapter.inspect('synthetic'); await adapter.inspect('synthetic');
  await assert.rejects(adapter.inspect('synthetic'));
  assert.equal(lookups, 3); assert.equal(calls, 2);
});
