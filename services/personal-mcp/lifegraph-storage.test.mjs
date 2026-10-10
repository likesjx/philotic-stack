import { test } from 'node:test';
import assert from 'node:assert/strict';
import { DatabaseSync } from 'node:sqlite';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createLifeGraphStorageAuthority } from './lifegraph-storage.mjs';
import { createLifeGraphAdapter } from './lifegraph.mjs';
export const policies = ['dot', 'claude'].map(clientId => ({ clientId, subjects: ['synthetic-operator'], namespace: `synthetic_${clientId}`, scopes: ['life:recall'] }));
export const identity = clientId => ({ active: true, audience: 'https://mcp.example.test/life/mcp', clientId, subject: 'synthetic-operator',
  scope: 'life:recall', agentId: `agent-${clientId}`, roles: ['reader'], grantVersion: '1' });
export const policy = change => ({ owner: 'operator', creator: 'agent-dot', creator_read_grant: true, read_roles: ['reader'],
  private: false, external_operations: ['inference'], sources: [], ...change });
export async function storageFixture(t, options = {}) {
  const dir = await mkdtemp(join(tmpdir(), 'synthetic-lifegraph-'));
  const path = join(dir, 'policy.sqlite'); const writer = new DatabaseSync(path);
  writer.exec('CREATE TABLE privacy_revision(singleton INTEGER PRIMARY KEY CHECK(singleton=1), revision INTEGER NOT NULL); INSERT INTO privacy_revision VALUES(1,1); CREATE TABLE privacy_policy(resource TEXT PRIMARY KEY, policy_json TEXT NOT NULL)');
  const put = (id, p) => writer.prepare('INSERT OR REPLACE INTO privacy_policy VALUES (?, ?)').run(id, JSON.stringify(p));
  for (const client of ['dot', 'claude']) put(`${client}-root`, policy());
  let graphRevision = '1'; const calls = [], revoked = new Set();
  const reader = { readSnapshot: async c => { calls.push(c); return { namespace: c.namespace, revision: graphRevision,
    nodes: [{ id: `${c.clientId}-root`, namespace: c.namespace, kind: 'Goal', summary: `synthetic ${c.clientId} goal` }], edges: [] }; } };
  const barrier = { admit: async (a, check) => !revoked.has(a.actor.clientId) && check({ graphRevision, actor: identity(a.actor.clientId) }) };
  const storage = createLifeGraphStorageAuthority({ policyDatabase: path, graphReader: options.graphReader ?? reader, releaseBarrier: options.releaseBarrier ?? barrier });
  t.after(() => { storage.close(); writer.close(); return rm(dir, { recursive: true, force: true }); });
  return { storage, writer, path, put, calls, revoked, setGraphRevision: v => { graphRevision = v; } };
}
test('read-only canonical policy store refuses absent, wrong-schema or missing release authorities', async t => {
  const f = await storageFixture(t);
  assert.throws(() => createLifeGraphStorageAuthority({ policyDatabase: f.path, graphReader: {} }), /storage unavailable/);
  assert.throws(() => createLifeGraphStorageAuthority({ policyDatabase: join(tmpdir(), `synthetic-missing-${Date.now()}.sqlite`), graphReader: { readSnapshot() {} }, releaseBarrier: { admit() {} } }), /storage unavailable/);
  const wrong = join(tmpdir(), `synthetic-wrong-${Date.now()}.sqlite`), db = new DatabaseSync(wrong); db.close();
  t.after(() => rm(wrong, { force: true }));
  assert.throws(() => createLifeGraphStorageAuthority({ policyDatabase: wrong, graphReader: { readSnapshot() {} }, releaseBarrier: { admit() {} } }), /storage unavailable/);
});
test('canonical ACL and complete foreign source ancestry replace graph claims without writes', async t => {
  const f = await storageFixture(t); f.put('dot-root', policy({ sources: ['origin'] })); f.put('origin', policy({ private: true }));
  const before = f.writer.prepare('SELECT * FROM privacy_policy ORDER BY resource').all();
  const context = { namespace: 'synthetic_dot', clientId: 'dot', subject: 'synthetic-operator' };
  const snapshot = await f.storage.snapshot(context);
  assert.equal(snapshot.sourcePolicies[0].id, 'origin'); assert.equal(snapshot.sourcePolicies[0].policy.private, true);
  const adapter = createLifeGraphAdapter({ profile: 'synthetic-read-only-v1', resource: identity('dot').audience, clientPolicies: policies,
    authenticate: async c => identity(c), snapshot: c => f.storage.snapshot(c), authorizeRelease: a => f.storage.authorizeRelease(a) });
  const result = JSON.parse((await adapter.recall('dot', { query_text: 'synthetic' })).content[0].text);
  assert.deepEqual(result, { packets: [], edges: [] });
  assert.deepEqual(f.writer.prepare('SELECT * FROM privacy_policy ORDER BY resource').all(), before);
});
test('unlabelled, stale, forged graph policy and namespace mismatches fail closed', async t => {
  const f = await storageFixture(t); const context = { namespace: 'synthetic_dot', clientId: 'dot', subject: 'synthetic-operator' };
  f.writer.prepare('DELETE FROM privacy_policy WHERE resource=?').run('dot-root');
  await assert.rejects(f.storage.snapshot(context), /storage unavailable/);
  for (const change of [{ namespace: 'private' }, { policy: policy({ private: false }) }]) {
    const g = await storageFixture(t, { graphReader: { readSnapshot: async () => ({ namespace: context.namespace, revision: '1',
      nodes: [{ id: 'dot-root', namespace: context.namespace, kind: 'Goal', summary: 'synthetic', ...change }], edges: [] }) } });
    await assert.rejects(g.storage.snapshot(context), /storage unavailable/);
  }
});
test('actual separate-connection policy revision changes and identity revocation deny release', async t => {
  const f = await storageFixture(t); const context = { namespace: 'synthetic_dot', clientId: 'dot', subject: 'synthetic-operator' };
  const snapshot = await f.storage.snapshot(context), admission = { ...context, actor: identity('dot'), revision: snapshot.revision };
  assert.equal(await f.storage.authorizeRelease(admission), true);
  const other = new DatabaseSync(f.path); other.exec('UPDATE privacy_revision SET revision=2 WHERE singleton=1'); other.close();
  assert.equal(await f.storage.authorizeRelease(admission), false);
  const latest = await f.storage.snapshot(context); f.revoked.add('dot');
  assert.equal(await f.storage.authorizeRelease({ ...admission, revision: latest.revision }), false);
});
test('a barrier returning true without invoking current canonical validation cannot admit', async t => {
  const f = await storageFixture(t, { releaseBarrier: { admit: async () => true } });
  assert.equal(await f.storage.authorizeRelease({ actor: identity('dot'), revision: 'fake' }), false);
});
