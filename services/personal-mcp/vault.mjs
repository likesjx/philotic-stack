import { createConnection } from 'node:net';

export function vaultCredential({ socketPath, secretRef, connect = createConnection }) {
  if (typeof socketPath !== 'string' || !socketPath.startsWith('/') || !socketPath.endsWith('.sock') || socketPath.includes('\0')) throw new Error('Explicit local hotel socket required');
  if (typeof secretRef !== 'string' || secretRef.length > 4096 || !/^secret:\/\/hotel\/default\/[A-Za-z0-9_/-]+$/.test(secretRef)) throw new Error('Invalid vault reference');
  return ({ signal } = {}) => new Promise((resolve, reject) => {
    let socket, pending = Buffer.alloc(0), registered = false, finished = false, frames = 0;
    const finish = (error, value) => {
      if (finished) return;
      finished = true; clearTimeout(timer); signal?.removeEventListener('abort', abort); socket?.destroy();
      error ? reject(new Error('Hotel vault credential unavailable')) : resolve(value);
    };
    const abort = () => finish(true);
    const timer = setTimeout(abort, 5000);
    if (signal?.aborted) return finish(true);
    signal?.addEventListener('abort', abort, { once: true });
    const send = (operation, payload) => {
      const body = Buffer.from(JSON.stringify({ operation, payload }));
      const header = Buffer.alloc(4); header.writeUInt32BE(body.length);
      socket.write(Buffer.concat([header, body]));
    };
    try {
      socket = connect({ path: socketPath });
      socket.on('connect', () => send('register', { guest_id: 'percival-personal-recall', role: 'percival-personal-recall', supported_tools: [] }));
      socket.on('error', () => finish(true));
      socket.on('close', () => { if (!finished) finish(true); });
      socket.on('data', chunk => {
        if (finished) return;
        pending = Buffer.concat([pending, chunk]);
        if (pending.length > 262148) return finish(true);
        while (pending.length >= 4 && !finished) {
          const size = pending.readUInt32BE();
          if (size > 262144 || size === 0 || ++frames > 64) return finish(true);
          if (pending.length < size + 4) { frames--; break; }
          let frame;
          try { frame = JSON.parse(pending.subarray(4, size + 4)); } catch { return finish(true); }
          pending = pending.subarray(size + 4);
          if (!frame || Array.isArray(frame) || typeof frame !== 'object') return finish(true);
          if (!registered) {
            if (!Object.hasOwn(frame, 'ok')) continue; // unsolicited hotel frames
            if (frame.ok !== true) return finish(true);
            registered = true; send('get_secret', { secret_ref: secretRef });
          } else if (Object.hasOwn(frame, 'secret_ref')) {
            if (frame.secret_ref !== secretRef || typeof frame.value_json !== 'string') return finish(true);
            let value;
            try { value = JSON.parse(frame.value_json); } catch { return finish(true); }
            if (typeof value !== 'string' || !value || value.length > 4096 || /[\x00-\x20\x7f]/.test(value)) return finish(true);
            finish(false, value);
          } else if (Object.hasOwn(frame, 'ok') || Object.hasOwn(frame, 'code')) return finish(true);
        }
      });
    } catch { finish(true); }
  });
}

// Broker owns the fixed reference/role; caller sends neither.
export function brokerCredential({ socketPath, kind = 'muninn', connect = createConnection }) {
  if (!['muninn', 'introspection'].includes(kind)) throw new Error('Unknown fixed credential kind');
  if (typeof socketPath !== 'string' || !socketPath.startsWith('/') || !socketPath.endsWith('.sock') || socketPath.includes('\0')) throw new Error('Explicit local broker socket required');
  return ({ signal } = {}) => new Promise((resolve, reject) => {
    let socket, bytes = Buffer.alloc(0), done = false;
    const finish = value => {
      if (done) return;
      done = true; clearTimeout(timer); signal?.removeEventListener('abort', abort); socket?.destroy();
      value === undefined ? reject(new Error('Scoped vault broker unavailable')) : resolve(value);
    };
    const abort = () => finish();
    const timer = setTimeout(abort, 5000);
    if (signal?.aborted) return finish();
    signal?.addEventListener('abort', abort, { once: true });
    try {
      socket = connect({ path: socketPath });
      socket.on('connect', () => {
        const body = Buffer.from(JSON.stringify({ operation: kind === 'muninn' ? 'get_percival_credential' : 'get_percival_introspection_credential' }));
        const header = Buffer.alloc(4); header.writeUInt32BE(body.length); socket.write(Buffer.concat([header, body]));
      });
      socket.on('error', () => finish());
      socket.on('close', () => { if (!done) finish(); });
      socket.on('data', chunk => {
        if (done) return;
        bytes = Buffer.concat([bytes, chunk]);
        if (bytes.length > 8196) return finish();
        if (bytes.length < 4) return;
        const length = bytes.readUInt32BE();
        if (length < 1 || length > 8192) return finish();
        if (bytes.length < length + 4) return;
        if (bytes.length !== length + 4) return finish();
        let reply;
        try { reply = JSON.parse(bytes.subarray(4)); } catch { return finish(); }
        if (kind === 'introspection' && typeof reply?.credential === 'string' && reply.credential.startsWith('mk_')) return finish();
        if (!reply || Array.isArray(reply) || Object.keys(reply).length !== 1 || typeof reply.credential !== 'string' || reply.credential.length > 4096 || !(kind === 'muninn' ? /^mk_[A-Za-z0-9_-]+$/ : /^[A-Za-z0-9_-]{43,128}$/).test(reply.credential)) return finish();
        finish(reply.credential);
      });
    } catch { finish(); }
  });
}
