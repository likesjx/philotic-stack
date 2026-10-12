import { createServer } from 'node:http';
import { randomBytes } from 'node:crypto';
import { createPersonalMcp } from './gateway.mjs';

export const RESERVED_SYNTHETIC_VAULT = 'percival_connection_test';
export const CLAUDE_HOSTED_CLIENT_ID = 'claude_hosted';
export const CLAUDE_HOSTED_CALLBACK = 'https://claude.ai/api/mcp/auth_callback';
export const CLAUDE_HOSTED_SCOPE = 'memory:recall';

export class SyntheticReservationStore {
  constructor() {
    this.vaults = new Map();
    this.secrets = new Map();
  }

  reserveVault({ vault = RESERVED_SYNTHETIC_VAULT, operator, source }) {
    if (!operator || typeof operator.subject !== 'string' || !operator.subject) throw new Error('authenticated_operator_required');
    const existing = this.vaults.get(vault);
    if (existing && existing.provenance?.purpose !== 'hosted-claude-muninn-synthetic-acceptance') {
      throw new Error('vault_provenance_collision');
    }
    if (existing) throw new Error('vault_already_reserved');
    const receipt = {
      vault,
      provenance: {
        purpose: 'hosted-claude-muninn-synthetic-acceptance',
        operatorSubject: operator.subject,
        source,
        createdAt: new Date().toISOString(),
      },
    };
    this.vaults.set(vault, receipt);
    return receipt;
  }

  storeCredential(record, { fail = false } = {}) {
    if (fail) throw new Error('synthetic_storage_failure');
    if (record.secretKind !== 'percival-muninn-observe' ||
        record.scope !== RESERVED_SYNTHETIC_VAULT ||
        record.role !== 'percival-personal-recall' ||
        record.guest !== 'percival-personal-gateway') {
      throw new Error('exact_acl_required');
    }
    const secretRef = `secret://hotel/default/${record.secretKind}/${randomBytes(8).toString('hex')}`;
    this.secrets.set(secretRef, { ...record, secretRef });
    return { secretRef };
  }
}

function json(res, status, value) {
  res.writeHead(status, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' });
  res.end(JSON.stringify(value));
}

// Local mock harness only. This is intentionally not a trusted provisioner and
// must not be used as evidence that real keys, grants, vaults, or admissions
// were created.
export async function withSyntheticMuninnAdmin(t, { uncertain = false } = {}) {
  const minted = new Map();
  const revoked = [];
  const server = createServer(async (req, res) => {
    const url = new URL(req.url, 'http://127.0.0.1');
    if (req.method === 'POST' && url.pathname === '/api/admin/keys') {
      let body = '';
      for await (const chunk of req) body += chunk;
      const input = JSON.parse(body || '{}');
      if (input.vault !== RESERVED_SYNTHETIC_VAULT || input.mode !== 'observe' || input.expires !== '1h' ||
          typeof input.label !== 'string' || !input.label.startsWith('synthetic-hosted-claude-')) {
        return json(res, 400, { error: 'invalid_synthetic_key_request' });
      }
      const keyId = `synthetic-key-${minted.size + 1}`;
      const token = `mk_${randomBytes(24).toString('base64url')}`;
      minted.set(keyId, { token, input });
      if (uncertain) return req.destroy();
      return json(res, 201, { key_id: keyId, token, mode: 'observe', vault: input.vault, expires_in: 3600 });
    }
    if (req.method === 'POST' && url.pathname.startsWith('/api/admin/keys/') && url.pathname.endsWith('/revoke')) {
      const keyId = url.pathname.split('/')[4];
      if (minted.has(keyId)) revoked.push(keyId);
      return json(res, 200, { revoked: true });
    }
    if (req.method === 'POST' && url.pathname === '/mcp') {
      let body = '';
      for await (const chunk of req) body += chunk;
      const rpc = JSON.parse(body || '{}');
      if (rpc.method === 'tools/list') {
        return json(res, 200, { jsonrpc: '2.0', id: rpc.id, result: { tools: [{ name: 'muninn_recall', inputSchema: { type: 'object' } }] } });
      }
      if (rpc.method === 'tools/call' && rpc.params?.name === 'muninn_recall') {
        return json(res, 200, { jsonrpc: '2.0', id: rpc.id, result: { content: [{ type: 'text', text: 'synthetic recall packet' }] } });
      }
      return json(res, 200, { jsonrpc: '2.0', id: rpc.id, error: { code: -32601, message: 'unavailable' } });
    }
    json(res, 404, { error: 'not_found' });
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t?.after?.(() => new Promise(resolve => server.close(resolve)));
  return { base: `http://127.0.0.1:${server.address().port}`, minted, revoked, close: () => new Promise(resolve => server.close(resolve)) };
}

async function mintObserveKey(adminBase, fetchImpl) {
  const base = new URL(adminBase);
  if (base.protocol !== 'http:' || !['127.0.0.1', 'localhost', '[::1]'].includes(base.hostname) || base.username || base.password) {
    throw new Error('local_mock_admin_required');
  }
  const signal = AbortSignal.timeout(1000);
  const response = await fetchImpl(`${base.origin}/api/admin/keys`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    signal,
    body: JSON.stringify({
      vault: RESERVED_SYNTHETIC_VAULT,
      label: `synthetic-hosted-claude-${Date.now()}`,
      mode: 'observe',
      expires: '1h',
    }),
  });
  if (!response.ok) throw new Error('synthetic_mint_failed');
  const body = await response.json();
  if (typeof body.key_id !== 'string' || typeof body.token !== 'string' || body.mode !== 'observe' || body.vault !== RESERVED_SYNTHETIC_VAULT) {
    throw new Error('synthetic_mint_uncertain');
  }
  return body;
}

async function revokeKey(adminBase, keyId, fetchImpl) {
  await fetchImpl(`${adminBase}/api/admin/keys/${encodeURIComponent(keyId)}/revoke`, { method: 'POST' }).catch(() => {});
}

export async function prepareSyntheticOnboarding({ operator, adminBase, source = {}, reservationStore = new SyntheticReservationStore(), fetchImpl = fetch, storeFailure = false }) {
  const receipt = reservationStore.reserveVault({ operator, source });
  let minted;
  try {
    minted = await mintObserveKey(adminBase, fetchImpl);
  } catch {
    throw new Error('synthetic_mint_uncertain');
  }
  try {
    const stored = reservationStore.storeCredential({
      secretKind: 'percival-muninn-observe',
      scope: RESERVED_SYNTHETIC_VAULT,
      role: 'percival-personal-recall',
      guest: 'percival-personal-gateway',
      token: minted.token,
    }, { fail: storeFailure });
    return {
      status: 'prepared',
      client: { clientId: CLAUDE_HOSTED_CLIENT_ID, callback: CLAUDE_HOSTED_CALLBACK, scope: CLAUDE_HOSTED_SCOPE },
      vault: receipt.vault,
      provenance: receipt.provenance,
      secretRef: stored.secretRef,
      redacted: { mintedKeyId: minted.key_id, token: '[redacted]' },
    };
  } catch (error) {
    await revokeKey(adminBase, minted.key_id, fetchImpl);
    throw error;
  }
}

export async function runSyntheticDryRun({ adminBase, token = 'synthetic-access-token', subject = 'synthetic-operator' }) {
  const reservationStore = new SyntheticReservationStore();
  const prepared = await prepareSyntheticOnboarding({
    operator: { subject },
    adminBase,
    source: { issuerCommit: 'synthetic-local', gatewayCommit: 'synthetic-local' },
    reservationStore,
  });
  const server = await createPersonalMcp({
    resource: 'https://mcp.example.test/personal/mcp',
    enabledTools: ['muninn_recall'],
    muninnVault: RESERVED_SYNTHETIC_VAULT,
    allowedSubjects: new Set([subject]),
    allowedClients: new Set([CLAUDE_HOSTED_CLIENT_ID]),
    clientPolicies: [{ clientId: CLAUDE_HOSTED_CLIENT_ID, subjects: [subject], scopes: [CLAUDE_HOSTED_SCOPE], vault: RESERVED_SYNTHETIC_VAULT }],
    clock: () => 1100000,
    issuer: { issuer: 'https://www.jaredlikes.com', ready: async () => {}, inspect: async presented => ({
      active: presented === token,
      iss: 'https://www.jaredlikes.com',
      aud: 'https://mcp.example.test/personal/mcp',
      sub: subject,
      client_id: CLAUDE_HOSTED_CLIENT_ID,
      iat: 1000,
      exp: 1900,
      scope: CLAUDE_HOSTED_SCOPE,
    }) },
    upstream: {
      list: async name => ({ name, inputSchema: { type: 'object' } }),
      call: async (_name, args) => {
        if (args.vault !== RESERVED_SYNTHETIC_VAULT || args.read_only !== true) throw new Error('unexpected synthetic args');
        return { content: [{ type: 'text', text: 'synthetic recall packet' }] };
      },
    },
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  try {
    const response = await fetch(`http://127.0.0.1:${server.address().port}/personal/mcp`, {
      method: 'POST',
      headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'muninn_recall', arguments: { context: ['synthetic'] } } }),
    });
    if (response.status !== 200) throw new Error('synthetic_gateway_failed');
    return { mode: 'local_mock_dry_run', ...prepared, gatewayStatus: response.status };
  } finally {
    await new Promise(resolve => server.close(resolve));
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  try {
    const local = await withSyntheticMuninnAdmin();
    try {
      const result = await runSyntheticDryRun({ adminBase: local.base });
      console.log(JSON.stringify(result, null, 2));
    } finally {
      await local.close();
    }
  } catch {
    console.error('Synthetic hosted Claude onboarding dry-run failed');
    process.exitCode = 1;
  }
}
