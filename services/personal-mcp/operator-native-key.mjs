import { readFile } from 'node:fs/promises';
import { createReadStream } from 'node:fs';
import process, { argv } from 'node:process';

export const INSTALLED_VAULT_SCOPE = 'percival_connection_test';
export const REQUIRED_SECRET_KIND = 'percival-muninn-observe';
export const REQUIRED_ROLE = 'percival-personal-recall';
export const REQUIRED_GUEST = 'percival-personal-gateway';

export class MissingExactAclStoreTransport extends Error {
  constructor() {
    super(
      'missing_exact_acl_store_transport: existing AddVaultEntry IPC stores scope="hotel" and allowed_guests=[]; broker exact ACL requires scope="percival_connection_test", allowed_roles=["percival-personal-recall"], allowed_guests=["percival-personal-gateway"]'
    );
  }
}

function usage() {
  return [
    'Usage:',
    '  node services/personal-mcp/operator-native-key.mjs \\',
    '    --provenance-receipt ./receipt.json \\',
    '    --operator-session-fd 3 \\',
    '    --admin-base https://www.jaredlikes.com \\',
    '    --hotel-socket /run/philotic/vps-jane.sock',
    '',
    'This command refuses before minting until the hotel exposes an exact-ACL store_secret transport.',
  ].join('\n');
}

function parseArgs(args) {
  const out = {};
  for (let i = 0; i < args.length; i += 2) {
    const key = args[i];
    const value = args[i + 1];
    if (!key?.startsWith('--') || value === undefined) throw new Error(usage());
    out[key.slice(2)] = value;
  }
  return out;
}

async function readFdText(fd) {
  if (fd === undefined) throw new Error('operator_session_fd_required');
  const n = Number(fd);
  if (!Number.isInteger(n) || n < 3) throw new Error('operator_session_fd_required');
  const stream = createReadStream(null, { fd: n, autoClose: false });
  const chunks = [];
  for await (const chunk of stream) chunks.push(chunk);
  return Buffer.concat(chunks).toString('utf8').trim();
}

export async function loadVerifiedProvenanceReceipt(path) {
  const receipt = JSON.parse(await readFile(path, 'utf8'));
  if (receipt?.vault !== INSTALLED_VAULT_SCOPE ||
      receipt?.provenance?.purpose !== 'hosted-claude-muninn-synthetic-acceptance' ||
      receipt?.provenance?.verified !== true ||
      typeof receipt?.provenance?.operatorSubject !== 'string' ||
      !receipt.provenance.operatorSubject) {
    throw new Error('verified_installed_scope_provenance_receipt_required');
  }
  return receipt;
}

export function assertExactAclStoreTransportAvailable({ addVaultEntryShape = true } = {}) {
  if (addVaultEntryShape) throw new MissingExactAclStoreTransport();
}

export async function prepareRealOperatorMintCheckpoint(options) {
  const receipt = await loadVerifiedProvenanceReceipt(options.provenanceReceipt);
  if (!/^https:\/\/[^/?#]+/.test(options.adminBase || '')) throw new Error('https_admin_base_required');
  if (typeof options.hotelSocket !== 'string' || !options.hotelSocket.startsWith('/')) throw new Error('hotel_socket_required');

  // Important: do not call /api/admin/keys before this passes. A minted key
  // without exact encrypted storage would have no safe acceptance path.
  assertExactAclStoreTransportAvailable();
  const operatorSession = await readFdText(options.operatorSessionFd);
  if (!operatorSession) throw new Error('operator_session_required');
  return { receipt };
}

if (import.meta.url === `file://${argv[1]}`) {
  try {
    const args = parseArgs(argv.slice(2));
    await prepareRealOperatorMintCheckpoint({
      provenanceReceipt: args['provenance-receipt'],
      operatorSessionFd: args['operator-session-fd'],
      adminBase: args['admin-base'],
      hotelSocket: args['hotel-socket'],
    });
  } catch (error) {
    console.error(error instanceof MissingExactAclStoreTransport ? error.message : 'operator native key checkpoint failed');
    process.exitCode = 1;
  }
}
