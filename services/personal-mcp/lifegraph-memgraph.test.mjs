import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createLifeGraphMemgraphReader, lifeGraphContentDigest, LIFEGRAPH_NODE_QUERY } from './lifegraph-memgraph.mjs';
const row = { id: 'idea:synthetic', labels: ['GrowthHypothesis'], summary: 'synthetic idea', sources: ['source:synthetic'] };
function fixture(overrides = {}) {
  const catalog = { namespace: 'synthetic_dot', nodes: [{ id: row.id, kind: 'Idea', label: 'GrowthHypothesis',
    contentDigest: lifeGraphContentDigest(row), sources: [...row.sources] }], edges: [] };
  const calls = [], transport = { readTransaction: async (_c, work) => work({ query: async (q,p) => {
    calls.push({ q,p }); return [{ ...structuredClone(row), sources: JSON.stringify(row.sources), ...overrides }]; } }) };
  return { reader: createLifeGraphMemgraphReader({ transport, catalogs: [catalog] }), calls, catalog };
}
test('existing idea ontology projects a content-bound read and exact catalog query fence', async () => {
  const f = fixture(); f.catalog.nodes[0].id = 'forged';
  const result = await f.reader.readSnapshot({ namespace: 'synthetic_dot' });
  assert.equal(result.nodes[0].kind, 'Idea'); assert.equal(result.nodes[0].id, row.id);
  assert.deepEqual(result.nodes[0].sourcePolicyManifest, row.sources);
  assert.deepEqual(f.calls, [{ q: LIFEGRAPH_NODE_QUERY, p: { ids: [row.id] } }]);
  await assert.rejects(f.reader.readSnapshot({ namespace: 'synthetic_claude' }), /unavailable/);
});
test('changed content, provenance, label and malformed manifests deny', async () => {
  for (const value of [{ summary: 'changed' }, { sources: '[]' }, { labels: ['Idea'] }, { sources: '["duplicate","duplicate"]' }]) {
    await assert.rejects(fixture(value).reader.readSnapshot({ namespace: 'synthetic_dot' }), /unavailable/);
  }
});
test('unreviewed ontology and production namespace do not construct a reader', () => {
  const transport = { readTransaction: async () => {} };
  for (const value of [{ namespace: 'production', nodes: [], edges: [] }, { namespace: 'synthetic_dot',
    nodes: [{ id: 'idea:x', kind: 'Idea', label: 'Idea', sources: [], contentDigest: 'a'.repeat(64) }], edges: [] }]) {
    assert.throws(() => createLifeGraphMemgraphReader({ transport, catalogs: [value] }), /unavailable/);
  }
});
