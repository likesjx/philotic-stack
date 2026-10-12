import { createServer } from 'node:http';
import { createHash } from 'node:crypto';

export const SCOPES = Object.freeze({ muninn_recall: 'memory:recall', 'life.recall': 'life:recall' });
export function selectedTools(value = ['muninn_recall']) {
  if (!Array.isArray(value) || !value.length || value.length > Object.keys(SCOPES).length ||
      new Set(value).size !== value.length || value.some(tool => !Object.hasOwn(SCOPES, tool))) {
    throw new Error('Explicit nonempty unique supported tools required');
  }
  return [...value];
}
const versions = ['2025-11-25', '2025-06-18', '2025-03-26'];
class Rejected extends Error {
  constructor(status, code) { super(code); this.status = status; this.code = code; }
}
const deny = (status, code) => { throw new Rejected(status, code); };
// One request signal follows authentication, body read, credential lookup,
// upstream fetch/body, and final authorization. Never restart the total budget.
function abortable(work, signal) {
  if (!signal) return Promise.resolve().then(work);
  if (signal.aborted) return Promise.reject(signal.reason);
  return new Promise((resolve, reject) => {
    const stop = () => reject(signal.reason);
    signal.addEventListener('abort', stop, { once: true });
    Promise.resolve().then(() => { signal.throwIfAborted(); return work(); })
      .then(resolve, reject).finally(() => signal.removeEventListener('abort', stop));
  });
}
function phaseSignal(signal, timeout) {
  return signal ? AbortSignal.any([signal, AbortSignal.timeout(timeout)]) : AbortSignal.timeout(timeout);
}
function httpsUrl(value) {
  const url = new URL(value);
  if (url.protocol !== 'https:' || url.username || url.password || url.hash || url.search) throw new Error('HTTPS URL required');
  return url;
}
async function boundedJson(response, max = 262144) {
  if (!response.ok) deny(503, 'upstream_unavailable');
  let size = 0; const chunks = [];
  for await (const chunk of response.body) {
    size += chunk.length;
    if (size > max) deny(503, 'upstream_response_too_large');
    chunks.push(chunk);
  }
  try { return JSON.parse(Buffer.concat(chunks).toString()); }
  catch { deny(503, 'invalid_upstream_response'); }
}

// The existing identity service is an application login service, not a general
// OAuth issuer. An established issuer owns consent, code/PKCE, refresh and
// revocation. No plugin access token is ever forwarded to a Philotic upstream.
export function issuerAdapter({ issuer, discovery, introspection, clientId, clientSecret, credential, fetchImpl = fetch }) {
  httpsUrl(issuer);
  const authority = issuer; // OAuth issuer identifiers are exact, including trailing slash.
  for (const endpoint of [discovery, introspection]) {
    if (httpsUrl(endpoint).origin !== new URL(authority).origin) throw new Error('Issuer endpoints must share the configured origin');
  }
  if (!clientId || /:/.test(clientId) || (typeof credential !== 'function' && !clientSecret) || (credential && clientSecret)) throw new Error('Confidential introspection client required');
  const request = async (url, init, signal) => {
    const bounded = phaseSignal(signal, 5000);
    return abortable(async () => boundedJson(await fetchImpl(url, {
      ...init, redirect: 'error', signal: bounded,
    })), bounded);
  };
  return {
    issuer: authority,
    async ready({ allowPreregistered = false } = {}) {
      const metadata = await request(discovery, { headers: { Accept: 'application/json' } });
      if (metadata.issuer !== authority || !metadata.code_challenge_methods_supported?.includes('S256') ||
          !metadata.response_types_supported?.includes('code') ||
          !(metadata.grant_types_supported ?? ['authorization_code', 'implicit']).includes('authorization_code') ||
          !(allowPreregistered || metadata.registration_endpoint || metadata.client_id_metadata_document_supported === true)) {
        throw new Error('Issuer lacks MCP authorization-code/PKCE/registration capabilities');
      }
      for (const name of ['authorization_endpoint', 'token_endpoint', 'registration_endpoint']) {
        if (metadata[name]) httpsUrl(metadata[name]);
        else if (name !== 'registration_endpoint') throw new Error('Issuer metadata endpoint missing');
      }
      if (metadata.introspection_endpoint !== introspection) throw new Error('Introspection endpoint differs from pinned configuration');
      return metadata;
    },
    async inspect(token, { signal } = {}) {
      const secret = credential ? await abortable(() => credential({ signal }), signal) : clientSecret;
      if (typeof secret !== 'string' || secret.length < 32 || secret.length > 4096 || /[\x00-\x20\x7f]/.test(secret)) deny(503, 'credential_unavailable');
      return request(introspection, {
        method: 'POST', headers: {
          Authorization: `Basic ${Buffer.from(`${encodeURIComponent(clientId)}:${encodeURIComponent(secret)}`).toString('base64')}`,
          'Content-Type': 'application/x-www-form-urlencoded', Accept: 'application/json',
        }, body: new URLSearchParams({ token, token_type_hint: 'access_token' }),
      }, signal);
    },
  };
}

function recallSchema(tool, vault) {
  return { type: 'object', additionalProperties: false, required: [tool === 'muninn_recall' ? 'context' : 'query_text'],
    properties: tool === 'muninn_recall' ? {
      context: { type: 'array', minItems: 1, maxItems: 16, items: { type: 'string', minLength: 1, maxLength: 2048 } },
      limit: { type: 'integer', minimum: 1, maximum: 20, default: 10 }, threshold: { type: 'number', minimum: 0, maximum: 1 },
      vault: { type: 'string', const: vault },
    } : {
      query_text: { type: 'string', minLength: 1, maxLength: 4096 }, query_id: { type: 'string', minLength: 1, maxLength: 128 },
      max_context_packets: { type: 'integer', minimum: 1, maximum: 12, default: 6 },
      operator_intent: { type: 'string', enum: ['open_loops_by_context', 'goals_and_next_actions', 'commitments_approaching', 're_entry_context'] },
    } };
}
function recallArguments(tool, args, vault) {
  if (!args || typeof args !== 'object' || Array.isArray(args)) deny(400, 'invalid_arguments');
  if (tool === 'muninn_recall' && args.vault !== undefined && args.vault !== vault) deny(403, 'vault_not_granted');
  const schema = recallSchema(tool, vault);
  if (Object.keys(args).some(key => !Object.hasOwn(schema.properties, key))) deny(400, 'invalid_arguments');
  const text = (value, max) => typeof value === 'string' && value.trim().length > 0 && value.length <= max;
  const boundedInt = (value, max) => value === undefined || (Number.isInteger(value) && value >= 1 && value <= max);
  if (tool === 'muninn_recall') {
    if (!Array.isArray(args.context) || args.context.length < 1 || args.context.length > 16 ||
        args.context.some(value => !text(value, 2048)) || !boundedInt(args.limit, 20) ||
        (args.threshold !== undefined && !(typeof args.threshold === 'number' && args.threshold >= 0 && args.threshold <= 1))) deny(400, 'invalid_arguments');
    return { ...args, vault, read_only: true, limit: args.limit ?? 10 };
  }
  if (!text(args.query_text, 4096) || (args.query_id !== undefined && !text(args.query_id, 128)) ||
      !boundedInt(args.max_context_packets, 12) ||
      (args.operator_intent !== undefined && !schema.properties.operator_intent.enum.includes(args.operator_intent))) deny(400, 'invalid_arguments');
  return { ...args, max_context_packets: args.max_context_packets ?? 6 };
}

export function frontdoorAdapter({ endpoints, enabledTools, fetchImpl = fetch }) {
  const tools = selectedTools(enabledTools);
  for (const tool of tools) {
    const config = endpoints[tool];
    if (!config || typeof config.credential !== 'function') throw new Error(`Missing scoped upstream for ${tool}`);
    const url = new URL(config.url);
    if (config.backendProtocol !== undefined && (config.backendProtocol !== 'muninn-unary-json' || tool !== 'muninn_recall' || url.pathname !== '/mcp')) throw new Error('Unsupported backend profile');
    if (url.username || url.password || url.hash || url.search ||
        !(url.protocol === 'https:' || (url.protocol === 'http:' && ['127.0.0.1', '[::1]'].includes(url.hostname)))) {
      throw new Error('Upstream must be HTTPS or literal loopback');
    }
  }
  let sequence = 0;
  async function rpc(tool, method, params, { signal } = {}) {
    if (!tools.includes(tool)) deny(403, 'tool_not_enabled');
    const { url, credential, backendProtocol } = endpoints[tool];
    const secret = await abortable(() => credential({ signal }), signal);
    signal?.throwIfAborted();
    if (typeof secret !== 'string' || !secret || /[\r\n]/.test(secret)) deny(503, 'upstream_unavailable');
    const id = ++sequence;
    const response = await fetchImpl(url, { method: 'POST', redirect: 'error', signal: phaseSignal(signal, 35000),
      headers: { Authorization: `Bearer ${secret}`, 'Content-Type': 'application/json', Accept: 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id, method, params }) });
    // This explicit profile supports Muninn unary JSON POST only, not general
    // session/SSE MCP. Reject an unexpected streaming/content transport.
    if (backendProtocol === 'muninn-unary-json' && !/^application\/json(?:;|$)/i.test(response.headers.get('content-type') || '')) deny(503, 'upstream_unavailable');
    const message = await abortable(() => boundedJson(response), signal);
    if (message.jsonrpc !== '2.0' || message.id !== id || message.error || !message.result) deny(503, 'upstream_unavailable');
    return message.result;
  }
  return {
    async list(tool) {
      const result = await rpc(tool, 'tools/list', {});
      const descriptor = result.tools?.find(item => item.name === tool);
      if (!descriptor?.inputSchema) deny(503, 'upstream_unavailable');
      return descriptor;
    },
    call(tool, args, options) { return rpc(tool, 'tools/call', { name: tool, arguments: args }, options); },
  };
}

export async function createPersonalMcp({ resource, issuer, upstream, allowedSubjects, allowedClients, clientPolicies,
  clock = () => Date.now(), maxLifetimeSeconds = 900, muninnVault, enabledTools, requestDeadlineMs = 45000 }) {
  const tools = selectedTools(enabledTools);
  const canonical = httpsUrl(resource);
  if (!Number.isInteger(requestDeadlineMs) || requestDeadlineMs < 1 || requestDeadlineMs > 45000) throw new Error('Bounded request deadline required');
  if (!canonical.pathname.endsWith('/mcp')) throw new Error('Resource must identify the MCP path');
  if (!(allowedSubjects instanceof Set) || !allowedSubjects.size || !(allowedClients instanceof Set) || !allowedClients.size) {
    throw new Error('Explicit operator subject and OAuth client allowlists required');
  }
  if ([allowedSubjects, allowedClients].some(values => values.size > 16 || [...values].some(value =>
    typeof value !== 'string' || !value.trim() || value.length > 2048))) throw new Error('Bounded string allowlists required');
  if (!Number.isInteger(maxLifetimeSeconds) || maxLifetimeSeconds < 1 || maxLifetimeSeconds > 900) throw new Error('Bounded lifetime required');
  if (typeof muninnVault !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(muninnVault)) throw new Error('Explicit bounded Muninn vault required');
  // Transitional remote-client milestone: synthetic recall only. No LifeGraph,
  // private vault, operation authority or write projection may be inherited.
  if (allowedClients.size > 1 && clientPolicies === undefined) throw new Error('Explicit synthetic per-client recall policies required');
  if (clientPolicies !== undefined) {
    if (!Array.isArray(clientPolicies) || clientPolicies.length !== allowedClients.size ||
        new Set(clientPolicies.map(p => p?.clientId)).size !== clientPolicies.length ||
        muninnVault !== 'percival_connection_test' || tools.length !== 1 || tools[0] !== 'muninn_recall' ||
        clientPolicies.some(p => !p || !allowedClients.has(p.clientId) || !Array.isArray(p.subjects) ||
          !p.subjects.length || p.subjects.some(s => !allowedSubjects.has(s)) || p.vault !== muninnVault ||
          !Array.isArray(p.scopes) || p.scopes.length !== 1 || p.scopes[0] !== 'memory:recall'))
      throw new Error('Explicit synthetic per-client recall policies required');
    clientPolicies = structuredClone(clientPolicies);
  }
  await issuer.ready({ allowPreregistered: true }); // Explicit client allowlist supports pre-registration.
  const descriptors = new Map();
  for (const tool of tools) {
    const descriptor = await upstream.list(tool);
    descriptors.set(tool, { name: tool, description: descriptor.description, inputSchema: recallSchema(tool, muninnVault),
      annotations: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
      securitySchemes: [{ type: 'oauth2', scopes: [SCOPES[tool]] }],
      _meta: { securitySchemes: [{ type: 'oauth2', scopes: [SCOPES[tool]] }] } });
  }
  const metadataPath = '/.well-known/oauth-protected-resource' + canonical.pathname;
  const metadataUrl = canonical.origin + metadataPath;
  const challenge = `Bearer resource_metadata="${metadataUrl}", scope="${tools.map(tool => SCOPES[tool]).join(' ')}"`;
  const metadata = { resource, authorization_servers: [issuer.issuer], scopes_supported: tools.map(tool => SCOPES[tool]), bearer_methods_supported: ['header'] };
  function presentedToken(req) {
    if (req.rawHeaders.filter((header, index) => index % 2 === 0 && header.toLowerCase() === 'authorization').length !== 1) deny(401, 'invalid_token');
    const match = /^Bearer ([\x21-\x7e]{1,4096})$/.exec(req.headers.authorization || '');
    if (!match) deny(401, 'invalid_token');
    return match[1];
  }
  async function authorize(req, signal) {
    const token = await abortable(() => issuer.inspect(presentedToken(req), { signal }), signal); // No gateway authorization cache.
    const now = Math.floor(clock() / 1000);
    if (token.active !== true || token.iss !== issuer.issuer ||
        !(token.aud === resource || (Array.isArray(token.aud) && token.aud.includes(resource))) ||
        !Number.isInteger(token.exp) || !Number.isInteger(token.iat) || token.exp <= now || token.iat > now ||
        token.exp <= token.iat || token.exp - token.iat > maxLifetimeSeconds ||
        !allowedSubjects.has(token.sub) || !allowedClients.has(token.client_id) || typeof token.scope !== 'string') {
      deny(401, 'invalid_token');
    }
    if (clientPolicies !== undefined) {
      const policy = clientPolicies.find(p => p.clientId === token.client_id);
      if (!policy?.subjects.includes(token.sub) || token.aud !== resource || token.scope !== 'memory:recall') deny(401, 'invalid_token');
    }
    return new Set(token.scope.split(' ').filter(Boolean));
  }
  const reply = (res, status, value, extra = {}) => {
    if (res.destroyed || res.writableEnded) return;
    res.writeHead(status, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store',
      'X-Content-Type-Options': 'nosniff', ...extra });
    res.end(value === undefined ? undefined : JSON.stringify(value));
  };
  let knownInflight = 0, unknownInflight = 0;
  // Hashes are scheduling hints only. Both lanes still introspect every request;
  // no token is authorized based on its admission history.
  const recentAdmissions = new Map();
  const server = createServer(async (req, res) => {
    let admittedLane, timer;
    const controller = new AbortController();
    const signal = controller.signal;
    const disconnected = () => setImmediate(() => {
      // A parser/body-limit rejection can itself abort IncomingMessage; let its
      // precise error win before treating the event as a remote disconnect.
      if (!res.writableEnded) controller.abort(new Rejected(499, 'client_disconnected'));
    });
    req.once('aborted', disconnected);
    res.once('close', disconnected);
    try {
      const url = new URL(req.url, canonical.origin);
      if (req.method === 'GET' && [metadataPath, '/.well-known/oauth-protected-resource'].includes(url.pathname)) {
        return reply(res, 200, metadata);
      }
      if (url.pathname !== canonical.pathname || url.search) return reply(res, 404, { error: 'not_found' });
      timer = setTimeout(() => controller.abort(new Rejected(504, 'request_timeout')), requestDeadlineMs);
      if (req.headers.origin && req.headers.origin !== canonical.origin) deny(403, 'invalid_origin');
      const fingerprint = createHash('sha256').update(presentedToken(req)).digest('hex');
      for (const [key, expiry] of recentAdmissions) if (expiry <= clock()) recentAdmissions.delete(key);
      const known = recentAdmissions.has(fingerprint);
      if ((known && knownInflight >= 24) || (!known && unknownInflight >= 8)) deny(429, 'too_many_requests');
      admittedLane = known ? 'known' : 'unknown';
      if (known) knownInflight++; else unknownInflight++;
      const scopes = await authorize(req, signal);
      if (recentAdmissions.size >= 32 && !recentAdmissions.has(fingerprint)) recentAdmissions.delete(recentAdmissions.keys().next().value);
      recentAdmissions.set(fingerprint, clock() + 30000);
      if (req.method !== 'POST') return reply(res, 405, { error: 'method_not_allowed' }, { Allow: 'POST' });
      if (!(req.headers['content-type'] || '').match(/^application\/json(?:;|$)/)) deny(415, 'invalid_content_type');
      let size = 0; const chunks = [];
      await abortable(async () => {
        for await (const chunk of req) { signal.throwIfAborted(); size += chunk.length; if (size > 32768) deny(413, 'request_too_large'); chunks.push(chunk); }
      }, signal);
      let message;
      try { message = JSON.parse(Buffer.concat(chunks)); } catch { deny(400, 'invalid_request'); }
      if (!message || Array.isArray(message) || message.jsonrpc !== '2.0' || typeof message.method !== 'string' ||
          (message.id !== undefined && !(typeof message.id === 'string' || Number.isSafeInteger(message.id)))) deny(400, 'invalid_request');
      const protocol = req.headers['mcp-protocol-version'];
      if (protocol && !versions.includes(protocol)) deny(400, 'unsupported_protocol_version');
      if (message.id === undefined) {
        if (message.method !== 'notifications/initialized') deny(400, 'invalid_request');
        return reply(res, 202);
      }
      let result;
      switch (message.method) {
        case 'initialize': result = { protocolVersion: versions.includes(message.params?.protocolVersion) ? message.params.protocolVersion : versions[0],
          capabilities: { tools: { listChanged: false } }, serverInfo: { name: 'philotic-personal-recall', version: '0.1.0' } }; break;
        case 'ping': result = {}; break;
        case 'tools/list': result = { tools: [...descriptors].filter(([name]) => scopes.has(SCOPES[name])).map(([, descriptor]) => descriptor) }; break;
        case 'tools/call': {
          const name = message.params?.name;
          if (!descriptors.has(name)) return reply(res, 200, { jsonrpc: '2.0', id: message.id, error: { code: -32602, message: 'Tool unavailable' } });
          if (!scopes.has(SCOPES[name])) deny(403, 'insufficient_scope');
          const args = recallArguments(name, message.params.arguments ?? {}, muninnVault);
          result = await abortable(() => upstream.call(name, args, { signal }), signal);
          if (!result || typeof result !== 'object' || Array.isArray(result) ||
              (result.isError !== undefined && typeof result.isError !== 'boolean') || !Array.isArray(result.content) ||
              result.content.some(item => !item || item.type !== 'text' || typeof item.text !== 'string')) deny(503, 'upstream_unavailable');
          // This recall projection retains only text and the validated error flag.
          // structuredContent, _meta, annotations and unknown diagnostic fields
          // have no approved schema here and are intentionally discarded.
          result = result.isError === true
            ? { isError: true, content: [{ type: 'text', text: 'Recall unavailable' }] }
            : { content: result.content.map(item => ({ type: 'text', text: item.text })),
                ...(result.isError === false ? { isError: false } : {}) };
          const freshScopes = await authorize(req, signal);
          if (!freshScopes.has(SCOPES[name])) deny(403, 'insufficient_scope');
          break;
        }
        default: return reply(res, 200, { jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'Method unavailable' } });
      }
      reply(res, 200, { jsonrpc: '2.0', id: message.id, result });
    } catch (error) {
      const failure = error instanceof Rejected ? error : signal.aborted ? signal.reason : error;
      const status = failure instanceof Rejected ? failure.status : 503;
      const code = failure instanceof Rejected ? failure.code : 'service_unavailable';
      if (code === 'request_timeout') {
        res.setHeader('Connection', 'close');
        res.once('finish', () => req.destroy());
      }
      reply(res, status, { error: code }, [401, 403].includes(status) ? { 'WWW-Authenticate': challenge + `, error="${code}"` } :
        status === 429 ? { 'Retry-After': '1' } : {});
    } finally {
      clearTimeout(timer); req.removeListener('aborted', disconnected); res.removeListener('close', disconnected);
      if (admittedLane === 'known') knownInflight--;
      else if (admittedLane === 'unknown') unknownInflight--;
    }
  });
  server.requestTimeout = 15000;
  server.headersTimeout = 10000;
  server.maxHeadersCount = 64;
  return server;
}

// Public resource discovery only: no issuer/credential initialization, RPC, or data.
// This does not make a plugin ready: the real issuer must also publish discovery.
export function createDiscoveryBootstrap({ resource, issuer, enabledTools }) {
  const canonical = httpsUrl(resource);
  httpsUrl(issuer);
  if (!canonical.pathname.endsWith('/mcp')) throw new Error('Resource must identify the MCP path');
  const scopes = selectedTools(enabledTools).map(tool => SCOPES[tool]);
  const path = '/.well-known/oauth-protected-resource' + canonical.pathname;
  const server = createServer((req, res) => {
    const url = new URL(req.url, canonical.origin);
    const discovery = req.method === 'GET' && !url.search && url.pathname === path;
    const mcp = url.pathname === canonical.pathname && !url.search;
    res.writeHead(discovery ? 200 : mcp ? 401 : 404, {
      'Content-Type': 'application/json', 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff',
      ...(mcp ? { 'WWW-Authenticate': `Bearer resource_metadata="${canonical.origin + path}", scope="${scopes.join(' ')}"` } : {}),
    });
    res.end(JSON.stringify(discovery ? { resource, authorization_servers: [issuer], scopes_supported: scopes, bearer_methods_supported: ['header'] }
      : { error: mcp ? 'setup_incomplete' : 'not_found' }));
  });
  server.requestTimeout = 15000; server.headersTimeout = 10000; server.maxHeadersCount = 64;
  return server;
}
