import { test } from 'node:test';
import assert from 'node:assert/strict';
import { validateWholeVaultAccess, persistentClients } from './whole-vault.mjs';
import { start } from './server.mjs';
const access = { version: 1, subject: 'fixture-operator', vault: 'approved_fixture', includeFutureRecords: true };
const config = () => ({ resource: 'https://mcp.example.test/personal/mcp', muninnVault: access.vault,
  allowedSubjects: new Set([access.subject]), allowedClients: new Set(persistentClients), tools: ['muninn_recall'],
  clientPolicies: persistentClients.map(clientId => ({ clientId, subjects: [access.subject], vault: access.vault, scopes: ['memory:recall'] })) });
test('explicit same-vault mapping is copied and cannot silently exclude future records', () => {
  assert.deepEqual(validateWholeVaultAccess(access, config()), access);
  for (const change of [{ vault: null }, { subject: null }, { includeFutureRecords: null }, { includeFutureRecords: false }, { extra: true }])
    assert.throws(() => validateWholeVaultAccess({ ...access, ...change }, config()));
});
test('whole-vault policy rejects ambiguous subjects clients vaults scopes and resources', () => {
  for (const mutate of [c => c.allowedSubjects.add('other'), c => c.allowedClients.add('other'),
    c => c.tools.push('life.recall'), c => c.clientPolicies[0].vault = 'other',
    c => c.clientPolicies[0].subjects.push('other'), c => c.clientPolicies[0].scopes.push('memory:write'),
    c => c.clientPolicies[0].clientId = c.clientPolicies[1].clientId, c => c.resource += '?extra=1']) {
    const c = config(); mutate(c); assert.throws(() => validateWholeVaultAccess(access, c));
  }
});
test('server rejects mixed LifeGraph and whole-vault composition before credentials or IO', async () => {
  await assert.rejects(start({ port: 8913, enabledTools: ['life.recall'], allowedSubjects: ['fixture-operator'],
    allowedClients: persistentClients, lifeGraph: { enabled: true }, wholeVaultAccess: access }), /separate LifeGraph/);
});
