import { test } from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { vaultCredential } from './vault.mjs';
const secretRef = 'secret://hotel/default/percival-muninn-observe/synthetic';
function fixture(mode = 'success') {
  const calls = []; let destroyed = false;
  class Socket extends EventEmitter {
    write(raw) {
      const request = JSON.parse(raw.subarray(4)); calls.push(request);
      const response = request.operation === 'register' ? (mode === 'registration' ? { ok: false, message: 'private diagnostic' } : { ok: true })
        : mode === 'denied' ? { ok: false, code: 'SECRET_ERROR', message: 'private diagnostic' }
        : { secret_ref: mode === 'wrong-ref' ? secretRef + '-wrong' : secretRef,
            value_json: mode === 'malformed' ? 'private diagnostic' : JSON.stringify(mode === 'unsafe' ? 'unsafe\nvalue' : 'synthetic-vault-key') };
      const body = Buffer.from(JSON.stringify(response)); const header = Buffer.alloc(4); header.writeUInt32BE(body.length);
      const frame = Buffer.concat([header, body]);
      queueMicrotask(() => { this.emit('data', frame.subarray(0, 3)); this.emit('data', frame.subarray(3)); });
    }
    destroy() { destroyed = true; }
  }
  const connect = () => { const socket = new Socket(); queueMicrotask(() => mode === 'socket' ? socket.emit('error', new Error('private diagnostic')) : socket.emit('connect')); return socket; };
  return { connect, calls, destroyed: () => destroyed };
}
test('vault credential resolves only the pinned reference using dedicated role and no cache', async () => {
  const f = fixture(); const read = vaultCredential({ socketPath: '/run/philotic/synthetic.sock', secretRef, connect: f.connect });
  assert.equal(await read(), 'synthetic-vault-key');
  assert.equal(await read(), 'synthetic-vault-key');
  assert.deepEqual(f.calls.map(call => call.operation), ['register', 'get_secret', 'register', 'get_secret']);
  assert.equal(f.calls[0].payload.role, 'percival-personal-recall');
  assert.deepEqual(f.calls[1].payload, { secret_ref: secretRef });
  assert.equal(f.destroyed(), true);
});
for (const mode of ['registration', 'denied', 'wrong-ref', 'malformed', 'unsafe', 'socket']) {
  test(`vault ${mode} fails closed with sanitized error`, async () => {
    const f = fixture(mode);
    await assert.rejects(vaultCredential({ socketPath: '/run/philotic/synthetic.sock', secretRef, connect: f.connect })(), error => {
      assert.equal(error.message, 'Hotel vault credential unavailable'); return true;
    });
    assert.equal(f.destroyed(), true);
    if (['registration', 'socket'].includes(mode)) assert.equal(f.calls.some(call => call.operation === 'get_secret'), false);
  });
}
test('invalid local socket/reference rejected before connect', () => {
  const connect = () => { throw new Error('Must not connect'); };
  for (const value of ['http://remote.test', 'secret://other/default/key', 'secret://hotel/default/key\nsecret']) {
    assert.throws(() => vaultCredential({ socketPath: '/run/hotel.sock', secretRef: value, connect }));
  }
  assert.throws(() => vaultCredential({ socketPath: 'relative.sock', secretRef, connect }));
});
