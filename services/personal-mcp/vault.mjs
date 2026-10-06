import { createConnection } from 'node:net';

export function vaultCredential({ socketPath, secretRef, connect = createConnection }) {
  if (typeof socketPath !== 'string' || !socketPath.startsWith('/') || !socketPath.endsWith('.sock') || socketPath.includes('\0')) throw new Error('Explicit local hotel socket required');
  if (typeof secretRef !== 'string' || secretRef.length > 4096 || !/^secret:\/\/hotel\/default\/[A-Za-z0-9_/-]+$/.test(secretRef)) throw new Error('Invalid vault reference');
  return () => new Promise((resolve, reject) => {
    let socket, pending = Buffer.alloc(0), registered = false, finished = false, frames = 0;
    const finish = (error, value) => {
      if (finished) return;
      finished = true; clearTimeout(timer); socket?.destroy();
      error ? reject(new Error('Hotel vault credential unavailable')) : resolve(value);
    };
    const timer = setTimeout(() => finish(true), 5000);
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
