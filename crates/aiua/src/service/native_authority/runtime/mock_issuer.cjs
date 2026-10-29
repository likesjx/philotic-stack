// Synthetic qualification issuer only. Built-ins only; never a production issuer.
const fs = require('node:fs');
const https = require('node:https');
const net = require('node:net');
const { randomUUID } = require('node:crypto');
const c = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
let active = true;
const issued = Math.floor(Date.now()/1000);
const readOnly = 'r'.repeat(43), wrongAudience = 'u'.repeat(43), wrongSubject = 's'.repeat(43);
function frame(socket, value) {
  const bytes = Buffer.from(JSON.stringify(value));
  const header = Buffer.alloc(4); header.writeUInt32BE(bytes.length);
  socket.write(Buffer.concat([header, bytes]));
}
async function revoke() {
  const session = randomUUID();
  const socket = net.createConnection(process.env.PERSONAL_OAUTH_LIFEGRAPH_OWNER_SOCKET);
  let buffer = Buffer.alloc(0), sequence = 1;
  return new Promise((resolve, reject) => {
    socket.setTimeout(4000, () => socket.destroy(new Error('timeout')));
    socket.on('error', reject);
    const send = operation => frame(socket, { version: 1, owner: process.env.PERSONAL_OAUTH_LIFEGRAPH_OWNER_ID, session, sequence, operation, parameters: {} });
    socket.on('connect', () => send('begin_issuer_change'));
    socket.on('data', bytes => {
      buffer = Buffer.concat([buffer, bytes]);
      if (buffer.length < 4 || buffer.length < buffer.readUInt32BE(0) + 4) return;
      const size = buffer.readUInt32BE(0);
      const reply = JSON.parse(buffer.subarray(4, size + 4)); buffer = buffer.subarray(size + 4);
      if (!reply.ok || reply.sequence !== sequence || reply.session !== session) return socket.destroy(new Error('denied'));
      if (sequence === 1) { active = false; sequence = 2; send('end_issuer_change'); }
      else { socket.end(); resolve(); }
    });
  });
}
https.createServer({ key: fs.readFileSync(c.key), cert: fs.readFileSync(c.cert) }, async (req, res) => {
  res.setHeader('content-type', 'application/json');
  if (req.url === '/test/revoke' && req.headers.authorization === `Bearer ${c.token}`) {
    try { await revoke(); res.end('{"revoked":true}'); }
    catch { res.statusCode = 503; res.end('{}'); }
    return;
  }
  if (req.url !== '/oauth/introspect' || req.headers.authorization !== `Basic ${Buffer.from(`fixture-client:${c.secret}`).toString('base64')}`) {
    res.statusCode = 401; res.end('{}'); return;
  }
  let body = ''; for await (const bytes of req) { body += bytes; if (body.length > 8192) { res.statusCode = 400; res.end('{}'); return; } }
  const token = new URLSearchParams(body).get('token');
  if (!active || ![c.token, readOnly, wrongAudience, wrongSubject].includes(token)) { res.end('{"active":false}'); return; }
  res.end(JSON.stringify({ active: true, iss:c.issuer, aud:token===wrongAudience?'https://wrong.example/mcp':c.resource,
    sub:token===wrongSubject?'other':c.subject, client_id:'fixture-dot', grant_version:'0:7',
    scope: token===readOnly?'native-memory:recall':'native-memory:remember native-memory:recall', iat:issued,exp:issued+300 }));
}).listen(c.port, '127.0.0.1');
