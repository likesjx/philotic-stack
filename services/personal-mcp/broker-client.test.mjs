import { test } from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { brokerCredential } from './vault.mjs';
function fixture(reply) {
  const calls = [];
  class Socket extends EventEmitter {
    write(raw) {
      calls.push(JSON.parse(raw.subarray(4)));
      const body = Buffer.from(JSON.stringify(reply)); const header = Buffer.alloc(4); header.writeUInt32BE(body.length);
      const frame = Buffer.concat([header, body]);
      queueMicrotask(() => { this.emit('data', frame.subarray(0, 2)); this.emit('data', frame.subarray(2)); });
    }
    destroy() { this.destroyed = true; }
  }
  const connect = () => { const socket = new Socket(); queueMicrotask(() => socket.emit('connect')); return socket; };
  return { calls, connect };
}
test('broker request carries no secret reference, role or caller identity and does not cache', async () => {
  const f = fixture({ credential: 'mk_synthetic' });
  const read = brokerCredential({ socketPath: '/run/percival-broker.sock', connect: f.connect });
  assert.equal(await read(), 'mk_synthetic'); assert.equal(await read(), 'mk_synthetic');
  assert.deepEqual(f.calls, [{ operation: 'get_percival_credential' }, { operation: 'get_percival_credential' }]);
});
for (const reply of [{ error: 'private diagnostic' }, { credential: 'mk_bad\nvalue' }, { credential: 'mk_bad=value' }, { credential: 'mk_ok', private: 'diagnostic' }, [], null]) {
  test(`broker fails closed for malformed or denied reply ${JSON.stringify(reply)}`, async () => {
    const f = fixture(reply);
    await assert.rejects(brokerCredential({ socketPath: '/run/percival-broker.sock', connect: f.connect })(), /Scoped vault broker unavailable/);
  });
}

test('request cancellation closes scoped credential socket with sanitized failure', async () => {
  const socket = new EventEmitter(); socket.write = () => {}; socket.destroy = () => { socket.destroyed = true; };
  const controller = new AbortController();
  const read = brokerCredential({ socketPath: '/run/percival-broker.sock', connect: () => socket });
  const pending = read({ signal: controller.signal }); controller.abort();
  await assert.rejects(pending, /Scoped vault broker unavailable/); assert.equal(socket.destroyed, true);
});
test('already expired request never connects to credential endpoint', async () => {
  const controller = new AbortController(); controller.abort(); let connections = 0;
  const read = brokerCredential({ socketPath: '/run/percival-broker.sock', connect: () => { connections++; } });
  await assert.rejects(read({ signal: controller.signal }), /Scoped vault broker unavailable/);
  assert.equal(connections, 0);
});

test('introspection uses a separate fixed operation, shape and uncached fetch', async () => {
  const f = fixture({ credential: 'i'.repeat(43) });
  const read = brokerCredential({ socketPath: '/run/percival-broker.sock', kind: 'introspection', connect: f.connect });
  assert.equal(await read(), 'i'.repeat(43)); assert.equal(await read(), 'i'.repeat(43));
  assert.deepEqual(f.calls, [{ operation: 'get_percival_introspection_credential' }, { operation: 'get_percival_introspection_credential' }]);
  await assert.rejects(brokerCredential({ socketPath: '/run/percival-broker.sock', connect: f.connect })());
  await assert.rejects(brokerCredential({ socketPath: '/run/percival-broker.sock', kind: 'introspection', connect: fixture({ credential: 'mk_synthetic' }).connect })());
  assert.throws(() => brokerCredential({ socketPath: '/run/percival-broker.sock', kind: 'get_secret' }));
});

test('long Muninn key cannot be interpreted as introspection credential', async () => {
  const f = fixture({ credential: 'mk_' + 'a'.repeat(43) });
  await assert.rejects(brokerCredential({ socketPath: '/run/percival-broker.sock', kind: 'introspection', connect: f.connect })());
});
