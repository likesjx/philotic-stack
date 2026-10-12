import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import {
  INSTALLED_VAULT_SCOPE,
  MissingExactAclStoreTransport,
  loadVerifiedProvenanceReceipt,
  prepareRealOperatorMintCheckpoint,
} from './operator-native-key.mjs';

function receipt(dir, value = {}) {
  const path = join(dir, 'receipt.json');
  writeFileSync(path, JSON.stringify({
    vault: INSTALLED_VAULT_SCOPE,
    provenance: {
      purpose: 'hosted-claude-muninn-synthetic-acceptance',
      verified: true,
      operatorSubject: 'synthetic-operator',
    },
    ...value,
  }));
  return path;
}

test('real operator checkpoint refuses before mint when exact-ACL encrypted store transport is unavailable', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'personal-mcp-'));
  try {
    await assert.rejects(prepareRealOperatorMintCheckpoint({
      provenanceReceipt: receipt(dir),
      operatorSessionFd: '999',
      adminBase: 'https://www.jaredlikes.com',
      hotelSocket: '/run/philotic/vps-jane.sock',
    }), MissingExactAclStoreTransport);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('verified installed-scope provenance receipt is mandatory', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'personal-mcp-'));
  try {
    await assert.rejects(loadVerifiedProvenanceReceipt(receipt(dir, {
      vault: 'percival_synthetic_recall_acceptance',
    })), /verified_installed_scope_provenance_receipt_required/);
    await assert.rejects(loadVerifiedProvenanceReceipt(receipt(dir, {
      provenance: { purpose: 'hosted-claude-muninn-synthetic-acceptance', verified: false, operatorSubject: 'synthetic-operator' },
    })), /verified_installed_scope_provenance_receipt_required/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
