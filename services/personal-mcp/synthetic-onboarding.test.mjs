import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  CLAUDE_HOSTED_CALLBACK,
  CLAUDE_HOSTED_CLIENT_ID,
  RESERVED_SYNTHETIC_VAULT,
  SyntheticReservationStore,
  prepareSyntheticOnboarding,
  runSyntheticDryRun,
  withSyntheticMuninnAdmin,
} from './synthetic-onboarding.mjs';

test('dry-run prepares hosted Claude recall onboarding without releasing credentials', async t => {
  const admin = await withSyntheticMuninnAdmin(t);
  const result = await runSyntheticDryRun({ adminBase: admin.base });
  assert.equal(result.status, 'prepared');
  assert.equal(result.gatewayStatus, 200);
  assert.equal(result.client.clientId, CLAUDE_HOSTED_CLIENT_ID);
  assert.equal(result.client.callback, CLAUDE_HOSTED_CALLBACK);
  assert.equal(result.vault, RESERVED_SYNTHETIC_VAULT);
  assert.equal(result.redacted.token, '[redacted]');
  assert.doesNotMatch(JSON.stringify(result), /mk_[A-Za-z0-9_-]+/);
  const request = [...admin.minted.values()][0].input;
  assert.deepEqual({ vault: request.vault, mode: request.mode, expires: request.expires }, {
    vault: RESERVED_SYNTHETIC_VAULT,
    mode: 'observe',
    expires: '1h',
  });
});

test('storage failure revokes only the newly minted synthetic key', async t => {
  const admin = await withSyntheticMuninnAdmin(t);
  await assert.rejects(prepareSyntheticOnboarding({
    operator: { subject: 'synthetic-operator' },
    adminBase: admin.base,
    storeFailure: true,
  }), /synthetic_storage_failure/);
  assert.deepEqual(admin.revoked, ['synthetic-key-1']);
});

test('uncertain mint fails closed and stores no synthetic credential', async t => {
  const admin = await withSyntheticMuninnAdmin(t, { uncertain: true });
  const store = new SyntheticReservationStore();
  await assert.rejects(prepareSyntheticOnboarding({
    operator: { subject: 'synthetic-operator' },
    adminBase: admin.base,
    reservationStore: store,
  }), /synthetic_mint_uncertain/);
  assert.equal(store.secrets.size, 0);
  assert.equal(admin.revoked.length, 0);
});

test('reserved synthetic vault refuses unknown provenance and requires existing operator', async t => {
  const admin = await withSyntheticMuninnAdmin(t);
  const store = new SyntheticReservationStore();
  const receipt = store.reserveVault({
    vault: RESERVED_SYNTHETIC_VAULT,
    operator: { subject: 'synthetic-operator' },
    source: { previous: true },
  });
  assert.equal(receipt.vault, RESERVED_SYNTHETIC_VAULT);
  assert.throws(() => store.reserveVault({
    vault: RESERVED_SYNTHETIC_VAULT,
    operator: { subject: 'synthetic-operator' },
    source: {},
  }), /vault_already_reserved/);
  const colliding = new SyntheticReservationStore();
  colliding.vaults.set(RESERVED_SYNTHETIC_VAULT, { provenance: { purpose: 'unknown' } });
  await assert.rejects(prepareSyntheticOnboarding({
    operator: { subject: 'synthetic-operator' },
    adminBase: admin.base,
    reservationStore: colliding,
  }), /vault_provenance_collision/);
  await assert.rejects(prepareSyntheticOnboarding({
    operator: null,
    adminBase: admin.base,
  }), /authenticated_operator_required/);
});

test('exact broker ACL fields are required for stored Muninn observe credential', () => {
  const store = new SyntheticReservationStore();
  store.reserveVault({ operator: { subject: 'synthetic-operator' }, source: {} });
  for (const bad of [
    { secretKind: 'other' },
    { scope: 'private' },
    { role: 'admin' },
    { guest: 'other' },
  ]) {
    assert.throws(() => store.storeCredential({
      secretKind: 'percival-muninn-observe',
      scope: RESERVED_SYNTHETIC_VAULT,
      role: 'percival-personal-recall',
      guest: 'percival-personal-gateway',
      token: 'mk_synthetic',
      ...bad,
    }), /exact_acl_required/);
  }
});
