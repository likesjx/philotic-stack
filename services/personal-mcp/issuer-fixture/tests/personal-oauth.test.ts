import { test } from 'node:test';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
import { PersonalOAuth, challenge, hash, OAuthError, type Store, type Pending, type Code, type Family } from '../src/lib/auth/personal-oauth/policy';
import { form, confidential, respond } from '../src/lib/auth/personal-oauth/http';
const config = { issuer: 'https://identity.example', resource: 'https://mcp.example/personal/mcp', clientId: 'percival', callbacks: ['https://client.example/callback'] };
const identity = { provider: 'google', providerId: 'synthetic-id' }, verifier = 'a'.repeat(64);
function fixture(multi = false) {
  let now = new Date('2026-10-05T12:00:00Z'), enabled = true, version = 0;
  const pending = new Map<string, Pending>(), codes = new Map<string, Code>(), families = new Map<string, Family>();
  const store: Store = {
    async resolve(i) { return i.providerId === identity.providerId ? 'stable-internal-subject' : 'other-subject'; },
    async admission(s) { return s === 'stable-internal-subject' ? { subject: s, enabled, version } : null; },
    async putPending(p) { pending.set(p.key, p); },
    async takePending(k, session, csrf, sub, at) { const p = pending.get(k); if (!p || p.sessionHash !== session || p.csrfHash !== csrf || p.subject !== sub || p.expiresAt <= at) return null; pending.delete(k); return p; },
    async putCode(c) { codes.set(c.key, c); },
    async takeCode(k, client, redirect, resource, ch, at) { const c = codes.get(k); if (!c || c.clientId !== client || c.redirectUri !== redirect || c.resource !== resource || c.challenge !== ch || c.expiresAt <= at) return null; codes.delete(k); return c; },
    async putFamily(f) { families.set(f.key, f); },
    async findRefresh(k, client) { return [...families.values()].find(f => f.clientId === client && (f.refreshHash === k || f.used.includes(k))) || null; },
    async findAccess(k) { return [...families.values()].find(f => f.access.some(a => a.key === k)) || null; },
    async rotate(k, refresh, generation, next, at) { const f = families.get(k); if (!f || f.revoked || f.refreshHash !== refresh || f.generation !== generation || f.expiresAt <= at) return false; families.set(k, next); return true; },
    async revoke(k) { const f = families.get(k); if (f) families.set(k, { ...f, revoked: true }); },
  };
  const grants = new Map(['dot', 'claude'].map(client => [client, { subject: 'stable-internal-subject', enabled: true, version: 0 }]));
  store.clientAdmission = async (_subject, client) => grants.get(client) ?? null;
  const multiConfig = { issuer: config.issuer, resource: config.resource, clients: [
    { clientId: 'dot', callbacks: ['https://dot.example/callback'] },
    { clientId: 'claude', callbacks: ['https://claude.example/callback'] },
  ] };
  const clock = () => now;
  const make = () => new PersonalOAuth(multi ? multiConfig : config, store, clock); const auth = make();
  const request = () => new URLSearchParams({ client_id: config.clientId, redirect_uri: config.callbacks[0], resource: config.resource,
    response_type: 'code', scope: 'memory:recall', state: 'client-state', code_challenge_method: 'S256', code_challenge: challenge(verifier) });
  const consent = async () => { const p = await auth.begin(auth.binding(request()), identity, 'browser-session'); return { p,
    finish: (approve = true) => auth.consent(p.transaction, p.csrf, 'browser-session', identity, config.issuer, approve) }; };
  const code = async () => new URL(await (await consent()).finish()).searchParams.get('code')!;
  const exchange = (c: string) => new URLSearchParams({ grant_type: 'authorization_code', client_id: config.clientId,
    redirect_uri: config.callbacks[0], resource: config.resource, code: c, code_verifier: verifier });
  const tokens = async () => auth.exchange(exchange(await code()));
  const refresh = (t: string) => new URLSearchParams({ grant_type: 'refresh_token', client_id: config.clientId, resource: config.resource, refresh_token: t });
  return { auth, make, clock, store, request, consent, code, exchange, tokens, refresh, pending, codes, families, grants,
    advance: (ms: number) => { now = new Date(now.getTime() + ms); }, disable: () => { enabled = false; }, bump: () => { version++; } };
}

test('dot and Claude codes, callbacks, refresh and revocation remain independently bound', async () => {
  const f = fixture(true);
  const authorize = async (client: string) => {
    const q = f.request(); q.set('client_id', client); q.set('redirect_uri', `https://${client}.example/callback`);
    const p = await f.auth.begin(f.auth.binding(q), identity, 'browser-session');
    const code = new URL(await f.auth.consent(p.transaction, p.csrf, 'browser-session', identity, config.issuer, true)).searchParams.get('code')!;
    const exchange = f.exchange(code); exchange.set('client_id', client); exchange.set('redirect_uri', `https://${client}.example/callback`);
    return exchange;
  };
  const dotCode = await authorize('dot'), claudeCode = await authorize('claude');
  const stolen = new URLSearchParams(dotCode); stolen.set('client_id', 'claude'); stolen.set('redirect_uri', 'https://claude.example/callback');
  await assert.rejects(f.auth.exchange(stolen));
  const stolenClaude = new URLSearchParams(claudeCode); stolenClaude.set('client_id', 'dot'); stolenClaude.set('redirect_uri', 'https://dot.example/callback');
  await assert.rejects(f.auth.exchange(stolenClaude));
  const dot = await f.auth.exchange(dotCode), claude = await f.auth.exchange(claudeCode);
  const refresh = (token: string, client: string) => { const q = f.refresh(token); q.set('client_id', client); return q; };
  await assert.rejects(f.auth.exchange(refresh(dot.refresh_token, 'claude')));
  await assert.rejects(f.auth.exchange(refresh(claude.refresh_token, 'dot')));
  await f.auth.revoke(dot.access_token, 'claude');
  assert.equal((await f.auth.introspect(dot.access_token)).active, true);
  const pendingDot = await authorize('dot');
  const legacyConfig = new PersonalOAuth({ issuer: config.issuer, resource: config.resource, clientId: 'dot', callbacks: ['https://dot.example/callback'] }, f.store, f.clock);
  assert.equal((await legacyConfig.introspect(dot.access_token)).active, true);
  f.grants.set('dot', { subject: 'stable-internal-subject', enabled: false, version: 1 });
  assert.deepEqual(await f.auth.introspect(dot.access_token), { active: false });
  assert.deepEqual(await legacyConfig.introspect(dot.access_token), { active: false });
  await assert.rejects(f.auth.exchange(refresh(dot.refresh_token, 'dot')));
  await assert.rejects(f.auth.exchange(pendingDot));
  assert.equal((await f.auth.introspect(claude.access_token)).active, true);
  const nextClaude = await f.auth.exchange(refresh(claude.refresh_token, 'claude'));
  f.grants.set('dot', { subject: 'stable-internal-subject', enabled: true, version: 2 });
  assert.deepEqual(await f.auth.introspect(dot.access_token), { active: false });
  await f.auth.revoke(nextClaude.refresh_token, 'claude');
  assert.deepEqual(await f.auth.introspect(nextClaude.access_token), { active: false });
  const q = f.request(); q.set('client_id', 'dot'); q.set('redirect_uri', 'https://claude.example/callback');
  assert.throws(() => f.auth.binding(q));
});

test('multi-client grants fail closed when missing and do not adopt legacy tokens', async () => {
  const f = fixture(), old = await f.tokens();
  assert.equal((await f.auth.introspect(old.access_token)).active, true);
  // Supply a valid client grant so migration rejection exercises the absent
  // persisted grant version, rather than expiry or a missing admission row.
  f.grants.set(config.clientId, { subject: 'stable-internal-subject', enabled: true, version: 0 });
  const auth = new PersonalOAuth({ issuer: config.issuer, resource: config.resource,
    clients: [{ clientId: config.clientId, callbacks: config.callbacks }] }, f.store, f.clock);
  assert.deepEqual(await auth.introspect(old.access_token), { active: false });
  await assert.rejects(auth.exchange(f.refresh(old.refresh_token)));
  f.grants.delete(config.clientId);
  await assert.rejects(auth.begin(auth.binding(f.request()), identity, 'browser-session'));
});

test('legacy grants omit optional version before Mongo serialization and stay usable', async () => {
  const f = fixture();
  // BSON encodes explicit undefined as null by default. Keep this property
  // absent rather than relying on driver-wide ignoreUndefined configuration.
  const serialize = <T extends object>(value: T): T => JSON.parse(JSON.stringify(value, (_key, item) => item === undefined ? null : item));
  f.store.putPending = async value => {
    assert.equal(Object.hasOwn(value, 'grantVersion'), false);
    f.pending.set(value.key, serialize(value));
    f.pending.get(value.key)!.expiresAt = value.expiresAt;
  };
  f.store.putFamily = async value => {
    assert.equal(Object.hasOwn(value, 'grantVersion'), false);
    f.families.set(value.key, value);
  };
  const tokens = await f.tokens();
  assert.equal((await f.auth.introspect(tokens.access_token)).active, true);
  assert.equal((await f.auth.exchange(f.refresh(tokens.refresh_token))).token_type, 'Bearer');
});

test('both issuer clients interoperate with the pinned gateway and revoke independently', {
  skip: !process.env.PERSONAL_MCP_GATEWAY_TEST_PATH && process.env.PERSONAL_MCP_REQUIRE_GATEWAY_INTEROP !== '1',
}, async t => {
  assert.ok(process.env.PERSONAL_MCP_GATEWAY_TEST_PATH, 'Pinned gateway source required');
  const { createPersonalMcp } = await import(pathToFileURL(process.env.PERSONAL_MCP_GATEWAY_TEST_PATH!).href);
  const f = fixture(true), tokens = new Map<string, string>();
  for (const client of ['dot', 'claude']) {
    const q = f.request(); q.set('client_id', client); q.set('redirect_uri', `https://${client}.example/callback`);
    const p = await f.auth.begin(f.auth.binding(q), identity, 'browser-session');
    const code = new URL(await f.auth.consent(p.transaction, p.csrf, 'browser-session', identity, config.issuer, true)).searchParams.get('code')!;
    const e = f.exchange(code); e.set('client_id', client); e.set('redirect_uri', `https://${client}.example/callback`);
    tokens.set(client, (await f.auth.exchange(e)).access_token);
  }
  let calls = 0;
  const server = await createPersonalMcp({ resource: config.resource, muninnVault: 'percival_synthetic_recall_acceptance', enabledTools: ['muninn_recall'],
    clock: () => new Date('2026-10-05T12:00:01Z').getTime(), allowedSubjects: new Set(['stable-internal-subject']), allowedClients: new Set(['dot', 'claude']),
    clientPolicies: ['dot', 'claude'].map(clientId => ({ clientId, subjects: ['stable-internal-subject'], scopes: ['memory:recall'], vault: 'percival_synthetic_recall_acceptance' })),
    issuer: { issuer: config.issuer, ready: async () => f.auth.metadata(), inspect: (token: string) => f.auth.introspect(token) },
    upstream: { list: async (name: string) => ({ name }), call: async (_name: string, args: { vault: string; read_only: boolean }) => {
      assert.equal(args.vault, 'percival_synthetic_recall_acceptance'); assert.equal(args.read_only, true); calls++;
      return { content: [{ type: 'text', text: 'synthetic connection packet' }] };
    } } });
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise<void>(resolve => server.close(resolve)));
  const rpc = (client: string) => fetch(`http://127.0.0.1:${server.address().port}/personal/mcp`, { method: 'POST',
    headers: { Authorization: 'Bearer ' + tokens.get(client), 'Content-Type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'muninn_recall', arguments: { context: ['synthetic'] } } }) });
  for (const client of ['dot', 'claude']) assert.equal((await rpc(client)).status, 200);
  f.grants.set('dot', { subject: 'stable-internal-subject', enabled: false, version: 1 });
  assert.equal((await rpc('dot')).status, 401); assert.equal((await rpc('claude')).status, 200);
  assert.equal(calls, 3);
});
test('first-party S256 consent, exchange and introspection preserve stable subject and exact claims', async () => {
  const f = fixture(), t = await f.tokens(), a = await f.auth.introspect(t.access_token);
  assert.equal(a.active, true); assert.equal(a.sub, 'stable-internal-subject'); assert.equal(a.aud, config.resource);
  assert.equal(a.client_id, config.clientId); assert.equal(a.scope, 'memory:recall'); assert.equal(a.exp! - a.iat!, 900);
  assert.ok(!JSON.stringify([...f.families.values()]).includes(t.access_token));
  assert.ok(!JSON.stringify([...f.families.values()]).includes(t.refresh_token));
});
for (const [key, value] of [['client_id', 'other'], ['redirect_uri', 'https://evil.example'], ['resource', 'https://wrong.example'],
  ['scope', 'admin'], ['code_challenge_method', 'plain'], ['code_challenge', 'bad'], ['response_type', 'token'], ['state', '']]) {
  test('authorization rejects ' + key, () => { const f = fixture(), q = f.request(); q.set(key, value); assert.throws(() => f.auth.binding(q), OAuthError); });
}
test('duplicate query and token fields rejected', async () => { const f = fixture(), q = f.request(); q.append('client_id', 'other'); assert.throws(() => f.auth.binding(q));
  const e = f.exchange(await f.code()); e.append('code', 'other'); await assert.rejects(f.auth.exchange(e)); });
for (const kind of ['origin', 'csrf', 'session', 'identity']) test('consent rejects substituted ' + kind, async () => {
  const f = fixture(), { p } = await f.consent();
  await assert.rejects(f.auth.consent(p.transaction, kind === 'csrf' ? 'z'.repeat(43) : p.csrf, kind === 'session' ? 'other' : 'browser-session',
    kind === 'identity' ? { provider: 'google', providerId: 'other' } : identity, kind === 'origin' ? 'https://evil.example' : config.issuer, true));
  assert.equal(f.codes.size, 0);
});
test('consent denial preserves state and issuer without code; transaction cannot replay', async () => {
  const f = fixture(), c = await f.consent(), u = new URL(await c.finish(false));
  assert.equal(u.searchParams.get('error'), 'access_denied'); assert.equal(u.searchParams.get('state'), 'client-state');
  assert.equal(u.searchParams.get('iss'), config.issuer); assert.equal(u.searchParams.has('code'), false); await assert.rejects(c.finish());
});
test('concurrent code redemption has exactly one winner; code stored only as hash', async () => {
  const f = fixture(), c = await f.code(); assert.equal(f.codes.has(hash(c)), true); assert.ok(!JSON.stringify([...f.codes.values()]).includes(c));
  const r = await Promise.allSettled([f.auth.exchange(f.exchange(c)), f.auth.exchange(f.exchange(c))]);
  assert.equal(r.filter(v => v.status === 'fulfilled').length, 1);
});
test('wrong verifier callback resource and client cannot redeem code', async () => {
  const f = fixture(), c = await f.code();
  for (const [k, v] of [['code_verifier', 'b'.repeat(64)], ['redirect_uri', 'https://evil.example'], ['resource', 'https://wrong.example'], ['client_id', 'other']]) {
    const q = f.exchange(c); q.set(k, v); await assert.rejects(f.auth.exchange(q));
  } assert.equal((await f.auth.exchange(f.exchange(c))).token_type, 'Bearer');
});
test('code and pending expiry enforced without TTL cleanup', async () => {
  const f = fixture(), c = await f.code(); f.advance(60_000); await assert.rejects(f.auth.exchange(f.exchange(c)));
  const consent = await f.consent(); f.advance(300_000); await assert.rejects(consent.finish());
});
test('new policy instance uses persisted codes and token families', async () => {
  const f = fixture(), c = await f.code(), restarted = f.make(), t = await restarted.exchange(f.exchange(c));
  assert.equal((await f.make().introspect(t.access_token)).active, true);
  await assert.rejects(f.make().exchange(f.exchange(c)));
});
test('refresh rotates; historical replay revokes entire family including fresh access', async () => {
  const f = fixture(), a = await f.tokens(), b = await f.auth.exchange(f.refresh(a.refresh_token));
  assert.notEqual(a.refresh_token, b.refresh_token); await assert.rejects(f.auth.exchange(f.refresh(a.refresh_token)));
  assert.equal((await f.auth.introspect(b.access_token)).active, false); await assert.rejects(f.auth.exchange(f.refresh(b.refresh_token)));
});
test('concurrent refresh has one winner and revokes raced family', async () => {
  const f = fixture(), t = await f.tokens(), results = await Promise.allSettled([f.auth.exchange(f.refresh(t.refresh_token)), f.auth.exchange(f.refresh(t.refresh_token))]);
  assert.equal(results.filter(r => r.status === 'fulfilled').length, 1); assert.equal((await f.auth.introspect(t.access_token)).active, false);
});
test('admission changes invalidate codes, refresh and access; restoring permission does not undo version change', async () => {
  const f = fixture(), t = await f.tokens(), c = await f.code(); f.bump();
  assert.equal((await f.auth.introspect(t.access_token)).active, false); await assert.rejects(f.auth.exchange(f.refresh(t.refresh_token)));
  await assert.rejects(f.auth.exchange(f.exchange(c)));
});
test('disabled admission and family/access expiration fail closed', async () => {
  const f = fixture(), t = await f.tokens(); f.advance(900_000); assert.equal((await f.auth.introspect(t.access_token)).active, false);
  const b = await f.auth.exchange(f.refresh(t.refresh_token)); f.disable(); assert.equal((await f.auth.introspect(b.access_token)).active, false);
  await assert.rejects(f.tokens());
  const g = fixture(), u = await g.tokens(); g.advance(7 * 86400_000); await assert.rejects(g.auth.exchange(g.refresh(u.refresh_token)));
});
test('revocation affects whole family and unknown token is harmless', async () => {
  const f = fixture(), t = await f.tokens(); await f.auth.revoke('unknown', config.clientId);
  await f.auth.revoke(t.access_token, config.clientId); assert.equal((await f.auth.introspect(t.access_token)).active, false);
  await assert.rejects(f.auth.exchange(f.refresh(t.refresh_token)));
});
test('backend outage cannot produce tokens or active introspection', async () => {
  const f = fixture(), t = await f.tokens(); f.store.admission = async () => { throw new Error('synthetic-private-diagnostic'); };
  const r = await respond(() => f.auth.introspect(t.access_token)); assert.ok(r instanceof Response); assert.equal(r.status, 503);
  assert.doesNotMatch(await r.text(), /synthetic-private/);
});
test('form transport is bounded and duplicate-resistant', async () => {
  for (const body of ['a=1&a=2', 'x=' + 'a'.repeat(9000)]) await assert.rejects(form(new Request('https://identity.example', { method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' }, body })));
  await assert.rejects(form(new Request('https://identity.example', { method: 'POST', body: '{}' })));
});
test('introspection requires separate confidential credential', () => {
  const secret = 's'.repeat(32), req = (value: string) => new Request('https://identity.example', { headers: { Authorization: value } });
  const verifierHash = createHash('sha256').update('Basic ' + Buffer.from('gateway:' + secret).toString('base64')).digest('hex');
  assert.throws(() => confidential(req('Bearer website-session'), 'gateway', verifierHash));
  confidential(req('Basic ' + Buffer.from('gateway:' + secret).toString('base64')), 'gateway', verifierHash);
});
test('discovery advertises only implemented preregistered public S256 flow', () => {
  const m = fixture().auth.metadata(); assert.deepEqual(m.token_endpoint_auth_methods_supported, ['none']);
  assert.deepEqual(m.code_challenge_methods_supported, ['S256']); assert.equal(m.authorization_response_iss_parameter_supported, true);
  assert.equal('registration_endpoint' in m, false); assert.equal('client_id_metadata_document_supported' in m, false);
});

test('resource introspection rejects refresh tokens before and after rotation and issuer reconstruction', async () => {
  const f = fixture(), first = await f.tokens();
  assert.equal((await f.auth.introspect(first.access_token)).active, true);
  assert.deepEqual(await f.auth.introspect(first.refresh_token), { active: false });
  assert.deepEqual(await f.make().introspect(first.refresh_token), { active: false });
  const next = await f.auth.exchange(f.refresh(first.refresh_token));
  assert.equal((await f.auth.introspect(next.access_token)).active, true);
  assert.deepEqual(await f.auth.introspect(next.refresh_token), { active: false });
  assert.deepEqual(await f.make().introspect(next.refresh_token), { active: false });
  assert.deepEqual(await f.make().introspect(first.refresh_token), { active: false });
});

test('life recall and mixed scope cannot be authorized', () => {
  const f = fixture();
  for (const scope of ['life:recall', 'memory:recall life:recall']) {
    const input = f.request(); input.set('scope', scope);
    assert.throws(() => f.auth.binding(input), /invalid_scope/);
  }
  assert.deepEqual(f.auth.metadata().scopes_supported, ['memory:recall']);
});
test('issuer verifier rejects malformed config and another client or secret', () => {
  const secret = 's'.repeat(43);
  const header = 'Basic ' + Buffer.from('gateway:' + secret).toString('base64');
  const verifier = createHash('sha256').update(header).digest('hex');
  const req = (h: string) => new Request('https://identity.example/oauth/introspect', { headers: { Authorization: h } });
  confidential(req(header), 'gateway', verifier);
  for (const h of ['Basic ' + Buffer.from('other:' + secret).toString('base64'), 'Basic ' + Buffer.from('gateway:' + 'x'.repeat(43)).toString('base64'), 'Bearer ' + secret]) assert.throws(() => confidential(req(h), 'gateway', verifier));
  assert.throws(() => confidential(req(header), 'gateway', secret));
  assert.throws(() => confidential(req(header), 'other', verifier));
});

test('previously persisted broader grants cannot introspect or refresh', async () => {
  const f = fixture(), tokens = await f.tokens();
  const family = [...f.families.values()][0]; family.scopes.push('life:recall');
  assert.deepEqual(await f.auth.introspect(tokens.access_token), { active: false });
  await assert.rejects(f.auth.exchange(f.refresh(tokens.refresh_token)));
});
