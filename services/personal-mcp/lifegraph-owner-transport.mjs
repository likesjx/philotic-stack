// Bounded client for a supervised, same-UID owner on a private Unix socket.
// No credential, role, address, raw Cypher or write operation crosses this API.
// The owner must separately authenticate the registered gateway's kernel peer.
import { createConnection } from 'node:net';
import { lstatSync } from 'node:fs';
import { dirname, isAbsolute } from 'node:path';
import { randomUUID } from 'node:crypto';
import { performance } from 'node:perf_hooks';
import { LIFEGRAPH_NODE_QUERY, LIFEGRAPH_EDGE_QUERY, LIFEGRAPH_ROOT_QUERY, LIFEGRAPH_ALIAS_QUERY } from './lifegraph-memgraph.mjs';
const fail = () => { throw new Error('LifeGraph owner unavailable'); };
const operations = new Map([[LIFEGRAPH_NODE_QUERY,'nodes'],[LIFEGRAPH_EDGE_QUERY,'edge'],
  [LIFEGRAPH_ROOT_QUERY,'verified_root'],[LIFEGRAPH_ALIAS_QUERY,'approved_alias']]);
const text = v => typeof v === 'string' && v.trim() && v.length <= 128;
function validateParameters(op, value) {
  const keys = op === 'nodes' ? ['ids'] : op === 'edge' ? ['from','to','relation'] : ['key'];
  if (!value || Array.isArray(value) || Object.keys(value).length !== keys.length ||
      Object.keys(value).some(k => !keys.includes(k))) fail();
  if (op === 'nodes' ? !Array.isArray(value.ids) || value.ids.length > 2048 || !value.ids.every(text) ||
      new Set(value.ids).size !== value.ids.length : !keys.every(k => text(value[k]))) fail();
}
export function createLifeGraphOwnerTransport({ socketPath, ownerIdentity, requestDeadlineMs = 5000 }) {
  if (typeof socketPath !== 'string' || !isAbsolute(socketPath) || !socketPath.endsWith('.sock') ||
      socketPath.includes('\0') || !text(ownerIdentity) || !Number.isInteger(requestDeadlineMs) ||
      requestDeadlineMs < 1 || requestDeadlineMs > 5000) fail();
  const checkSocket = () => {
    const parent = lstatSync(dirname(socketPath)), socket = lstatSync(socketPath);
    if (!parent.isDirectory() || parent.uid !== process.getuid() || (parent.mode & 0o077) !== 0 ||
        !socket.isSocket() || socket.uid !== process.getuid() || (socket.mode & 0o077) !== 0) fail();
    return `${socket.dev}:${socket.ino}`;
  };
  return Object.freeze({ async readTransaction({ signal } = {}, work) {
    signal?.throwIfAborted();
    if (typeof work !== 'function') fail();
    const expectedSocket = checkSocket(), session = randomUUID(), deadline = performance.now() + requestDeadlineMs;
    const socket = createConnection({ path: socketPath });
    let pending, sequence = 0, closed = false, header = Buffer.alloc(4), headerBytes = 0, body, bodyBytes = 0;
    function stop() {
      if (closed) return; closed = true; socket.destroy();
      const next = pending; pending = null; next?.reject(new Error('LifeGraph owner unavailable'));
    }
    const timer = setTimeout(stop, requestDeadlineMs); timer.unref?.();
    signal?.addEventListener('abort', stop, { once: true });
    const check = () => { signal?.throwIfAborted(); if (closed || performance.now() >= deadline || checkSocket() !== expectedSocket) fail(); };
    socket.on('error', stop); socket.on('close', stop);
    socket.on('data', chunk => {
      if (closed) return;
      let offset = 0;
      try {
        while (offset < chunk.length) {
          if (!pending) fail();
          if (!body) {
            const count = Math.min(4-headerBytes,chunk.length-offset);
            chunk.copy(header,headerBytes,offset,offset+count); headerBytes += count; offset += count;
            if (headerBytes < 4) continue;
            const length = header.readUInt32BE(); if (length < 1 || length > 1048576) fail();
            body = Buffer.alloc(length); bodyBytes = 0;
          }
          const count = Math.min(body.length-bodyBytes,chunk.length-offset);
          chunk.copy(body,bodyBytes,offset,offset+count); bodyBytes += count; offset += count;
          if (bodyBytes === body.length) {
            const reply = JSON.parse(body.toString('utf8'));
            check();
            if (!reply || Array.isArray(reply) || Object.keys(reply).some(k => !['owner','session','sequence','ok','result'].includes(k)) ||
                reply.owner !== ownerIdentity || reply.session !== session || reply.sequence !== pending.sequence || reply.ok !== true) fail();
            // Extra/coalesced unsolicited replies are not part of this protocol.
            if (offset !== chunk.length) fail();
            const next = pending; pending = null; body = undefined; bodyBytes = 0; headerBytes = 0;
            next.resolve(reply.result);
          }
        }
      } catch { stop(); }
    });
    function send(operation, parameters) {
      check(); if (pending || sequence >= 6148) fail();
      return new Promise((resolve,reject) => {
        const id = ++sequence; pending = { resolve,reject,sequence:id };
        const payload = Buffer.from(JSON.stringify({ version:1,owner:ownerIdentity,session,sequence:id,operation,parameters }));
        if (payload.length > 262144) { stop(); return; }
        const head = Buffer.alloc(4); head.writeUInt32BE(payload.length); socket.write(head); socket.write(payload);
      });
    }
    try {
      await new Promise((resolve,reject) => {
        pending = { reject,sequence:0 };
        socket.once('connect', () => { pending = null; try { check(); resolve(); } catch { stop(); reject(new Error('LifeGraph owner unavailable')); } });
      });
      if (await send('begin_snapshot', {}) !== true) fail();
      const result = await work({ async query(query, parameters) {
        const operation = operations.get(query); if (!operation) fail(); validateParameters(operation,parameters);
        const rows = await send(operation,parameters); if (!Array.isArray(rows) || rows.length > (operation === 'nodes' ? 2049 : 2)) fail();
        return rows;
      } });
      check(); if (await send('finish_snapshot', {}) !== true) fail(); check(); return result;
    } catch { fail(); }
    finally {
      // Connection close is rollback/cancel on the owner. That owner must keep
      // its reservation until transaction rollback and server quiescence ack.
      stop(); clearTimeout(timer); signal?.removeEventListener('abort', stop);
    }
  } });
}
