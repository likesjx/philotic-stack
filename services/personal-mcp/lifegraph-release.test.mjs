import { test } from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { createHash } from 'node:crypto';
import { createLifeGraphReleaseCoordinator } from './lifegraph-release.mjs';
const actor = { clientId: 'dot', subject: 'synthetic', grantVersion: '1', roles: ['reader'] };
const turn = () => new Promise(resolve => setImmediate(resolve));
const response = () => {
  const r = new EventEmitter(); r.destroyed = false; r.writableEnded = false;
  r.destroy = () => { r.destroyed = true; r.emit('close'); };
  r.finish = () => { r.writableEnded = true; r.emit('finish'); }; return r;
};
function fixture(options = {}) {
  let current = structuredClone(actor), revision = '1', released = 0, pinned = 0;
  const coordinator = createLifeGraphReleaseCoordinator({
    issuerAuthority: { resolveCurrent: async () => current },
    policyAuthority: { pin: async () => { pinned++; return { release: async () => { released++; } }; } },
    graphAuthority: { readSnapshot: async () => revision, currentRevision: async () => revision }, ...options });
  const request = {}, res = response(); coordinator.bindResponse(request, res);
  const admission = { request, actor, namespace: 'synthetic_dot', revision: '1', responseDigest: 'a'.repeat(64) };
  return { coordinator, request, res, admission, change: () => { current = null; revision = '2'; },
    counts: () => ({ pinned, released }), check: ({ actor: a, graphRevision }) => JSON.stringify(a) === JSON.stringify(actor) && graphRevision === '1' };
}
test('grant and graph changes wait through response handoff and revoke new requests', async () => {
  const f = fixture(); assert.equal(await f.coordinator.admit(f.admission, f.check), true);
  let changed = false;
  const change = f.coordinator.participateIssuerChange({ clientId: 'dot' }, async () => { f.change(); changed = true; });
  await turn(); assert.equal(changed, false); assert.equal(f.counts().released, 0);
  f.res.finish(); await change; assert.equal(changed, true); assert.equal(f.counts().released, 1);
  const other = {}; f.coordinator.bindResponse(other, response());
  assert.equal(await f.coordinator.admit({ ...f.admission, request: other }, f.check), false);
});
test('missing response binding, cross-request replay and repeated permit use deny', async () => {
  const f = fixture(); assert.equal(await f.coordinator.admit({ ...f.admission, request: {} }, f.check), false);
  assert.equal(await f.coordinator.admit(f.admission, f.check), true);
  assert.equal(await f.coordinator.admit(f.admission, f.check), false);
  f.res.finish(); await turn(); assert.deepEqual(f.counts(), { pinned: 1, released: 1 });
});
test('graph change before admission invalidates pending output', async () => {
  const f = fixture(); await f.coordinator.participateGraphChange({}, async () => f.change());
  assert.equal(await f.coordinator.admit(f.admission, f.check), false); assert.equal(f.counts().released, 0);
});
test('policy rejection and disconnect release the actual reservation exactly once', async () => {
  const f = fixture(); assert.equal(await f.coordinator.admit(f.admission, () => false), false);
  assert.deepEqual(f.counts(), { pinned: 1, released: 1 });
  const g = fixture(); assert.equal(await g.coordinator.admit(g.admission, g.check), true);
  g.res.destroy(); g.res.emit('finish'); await turn(); assert.deepEqual(g.counts(), { pinned: 1, released: 1 });
});
test('deadline closes an admitted response, bounded waiters deny, and owner close drains', async () => {
  const f = fixture({ maxLeaseMs: 100, maxPending: 1 });
  assert.equal(await f.coordinator.admit(f.admission, f.check), true);
  const pending = f.coordinator.readSnapshot({}).catch(() => {});
  await assert.rejects(f.coordinator.readSnapshot({}), /unavailable/);
  // Keep the test event loop alive independently of the unref'd owner timer.
  await new Promise(resolve => setTimeout(resolve, 120));
  await pending; assert.equal(f.res.destroyed, true); assert.equal(f.counts().released, 1);
  const g = fixture(); assert.equal(await g.coordinator.admit(g.admission, g.check), true);
  g.coordinator.close(); await turn(); assert.equal(g.res.destroyed, true);
  await assert.rejects(g.coordinator.readSnapshot({}), /unavailable/);
});
test('reservation-release failure permanently closes the coordinator before granting queued work', async () => {
  const f = fixture({ policyAuthority: { pin: async () => ({ release: async () => { throw new Error('release failed'); } }) } });
  assert.equal(await f.coordinator.admit(f.admission, f.check), true);
  const pending = f.coordinator.readSnapshot({}); const rejection = assert.rejects(pending, /unavailable/);
  f.res.finish(); await rejection;
  await assert.rejects(f.coordinator.readSnapshot({}), /unavailable/);
});
test('handoff verifies the exact admitted payload and rejects changed text or another request', async () => {
  const f = fixture(), result = { content: [{ type: 'text', text: '{"packets":[]}' }] };
  f.admission.responseDigest = createHash('sha256').update(result.content[0].text).digest('hex');
  assert.equal(await f.coordinator.admit(f.admission, f.check), true);
  assert.equal(f.coordinator.verifyResponse(f.request, result), true);
  assert.equal(f.coordinator.verifyResponse({}, result), false);
  assert.equal(f.coordinator.verifyResponse(f.request, { content: [{ type: 'text', text: 'changed' }] }), false);
  f.res.finish(); await turn(); assert.equal(f.coordinator.verifyResponse(f.request, result), false);
});
test('daemon fence is acquired before issuer checks and retained through policy release and HTTP handoff',async()=>{
  const order=[];let valid=true;
  const f=fixture({
    issuerAuthority:{resolveCurrent:async()=>{order.push('actor');return actor;}},
    policyAuthority:{pin:async()=>{order.push('policy');return{release:async()=>order.push('policy-release')};}},
    graphAuthority:{readSnapshot:async()=>{},currentRevision:async()=>{throw Error('unfenced read');},
      pinRelease:async()=>{order.push('daemon-pin');return{validate:()=>valid,currentRevision:async()=>{order.push('pinned-revision');return'1';},release:async()=>order.push('daemon-release')};}}
  });
  assert.equal(await f.coordinator.admit(f.admission,f.check),true);
  assert.deepEqual(order,['daemon-pin','actor','policy','pinned-revision']);
  valid=false;assert.equal(f.coordinator.verifyResponse(f.request,{content:[{type:'text',text:'synthetic'}]}),false);
  f.res.finish();await turn();assert.deepEqual(order.slice(-2),['policy-release','daemon-release']);
});
