// Pure preparation/runtime policy primitives. No deployment, storage or credential IO.
// This does not replace issuer introspection or the backend's privacy authority.
const CLIENTS = ['dot', 'claude'];
const integer = x => Number.isSafeInteger(x) && x >= 0;
function require(value, reason) { if (!value) throw new Error(reason); }
function frozenJson(value) {
  const copy = structuredClone(value);
  const seen = new Set();
  function freeze(v) {
    if (v === null || typeof v === 'string' || typeof v === 'boolean') return v;
    if (typeof v === 'number') { require(Number.isFinite(v), 'invalid JSON number'); return v; }
    require(typeof v === 'object' && (Array.isArray(v) || Object.getPrototypeOf(v) === Object.prototype), 'JSON value required');
    require(!seen.has(v), 'cyclic JSON'); seen.add(v);
    for (const x of Object.values(v)) freeze(x);
    seen.delete(v); return Object.freeze(v);
  }
  return freeze(copy);
}
function httpsCallback(value) {
  try {
    const u = new URL(value);
    return u.protocol === 'https:' && !u.username && !u.password && !u.hash &&
      !value.includes('*') && !u.hostname.endsWith('.example') && u.hostname !== 'example.com';
  } catch { return false; }
}

export function preparePersistentRecall(input) {
  const m = structuredClone(input);
  require(m && m.version === 1 && m.mode === 'persistent', 'persistent manifest required');
  require(typeof m.subject === 'string' && m.subject.length > 0, 'subject required');
  require(typeof m.resource === 'string' && httpsCallback(m.resource), 'HTTPS resource required');
  require(Array.isArray(m.clients) && m.clients.length === 2 &&
    new Set(m.clients.map(c => c?.kind)).size === 2 &&
    m.clients.every(c => CLIENTS.includes(c?.kind)), 'two distinct client kinds required');
  const blockers = [];
  const ids = new Set(), callbacks = new Set();
  for (const client of m.clients) {
    if (typeof client.clientId !== 'string' || !client.clientId.trim()) blockers.push(`${client.kind}:client-id`);
    else { require(!ids.has(client.clientId), 'duplicate client ID'); ids.add(client.clientId); }
    if (!Array.isArray(client.callbacks) || !client.callbacks.length) blockers.push(`${client.kind}:callbacks`);
    else for (const callback of client.callbacks) {
      require(httpsCallback(callback), 'exact HTTPS callback required');
      require(!callbacks.has(callback), 'callbacks cannot be shared'); callbacks.add(callback);
    }
    if (client.publicS256Verified !== true) blockers.push(`${client.kind}:public-S256`);
    if (client.resourceIndicatorVerified !== true) blockers.push(`${client.kind}:resource-indicator`);
    if (client.runtimeToolExposureVerified !== true) blockers.push(`${client.kind}:runtime-tool-exposure`);
  }
  if (!Array.isArray(m.vaults) || !m.vaults.length) blockers.push('approved-real-memory-scope');
  else require(new Set(m.vaults).size === m.vaults.length &&
    m.vaults.every(v => typeof v === 'string' && /^[A-Za-z0-9_-]{1,128}$/.test(v)), 'invalid vault scope');
  // Evidence identifiers are references for review, not permission to activate.
  for (const field of ['privacyAuthorityReceipt', 'readonlyReceipt', 'credentialDeliveryReceipt',
    'rotationReceipt', 'rollbackReceipt', 'serviceLifecycleReceipt']) {
    if (typeof m[field] !== 'string' || !/^[a-f0-9]{64}$/.test(m[field])) blockers.push(field);
  }
  return { version: 1, mode: 'persistent', activationAllowed: false,
    preparationComplete: blockers.length === 0, blockers, manifest: m,
    requiredAction: 'separate reviewed integration and exact production approval' };
}

// In-memory reference state machine for a future durable owner. Not a DB adapter.
// Each transition returns a new state; callers must persist via compare-and-swap.
export function revokeClient(state, clientId) {
  const next = structuredClone(state);
  const c = next.clients?.[clientId];
  require(c && integer(c.version) && c.version < Number.MAX_SAFE_INTEGER, 'unknown or exhausted client');
  c.enabled = false; c.version++;
  next.families = (next.families || []).map(f => f.clientId === clientId ? { ...f, revoked: true } : f);
  // Global subject state and the other client's version/families are untouched.
  return next;
}

export function beginRotation(state, replacement, now) {
  require(integer(now) && state.phase === 'active' && state.activeCredential, 'rotation state');
  require(replacement && typeof replacement.reference === 'string' && replacement.reference.startsWith('secret://') &&
    replacement.reference !== state.activeCredential.reference && integer(replacement.expiresAt) && replacement.expiresAt > now,
    'replacement reference required');
  return { ...structuredClone(state), phase: 'rotation-pending', replacement: structuredClone(replacement) };
}
export function finishRotation(state, evidence, now) {
  require(integer(now) && state.phase === 'rotation-pending' && state.replacement.expiresAt > now, 'rotation state');
  require(evidence?.reference === state.replacement.reference && evidence.adopted === true &&
    evidence.positiveRecall === true && evidence.oldCredentialDenied === true, 'rotation evidence incomplete');
  const next = structuredClone(state);
  next.activeCredential = next.replacement; delete next.replacement; next.phase = 'active';
  return next;
}

export function canRecall(state, request, now) {
  const c = state.clients?.[request.clientId];
  return integer(now) && state.phase === 'active' && state.subjectEnabled === true &&
    request.subject === state.subject && request.resource === state.resource &&
    request.scope === 'memory:recall' && request.tool === 'muninn_recall' && request.readOnly === true &&
    Array.isArray(state.vaults) && state.vaults.includes(request.vault) &&
    c?.enabled === true && integer(c.version) && request.clientVersion === c.version &&
    integer(state.activeCredential?.expiresAt) && state.activeCredential.expiresAt > now &&
    integer(state.privacyLeaseExpiresAt) && state.privacyLeaseExpiresAt > now;
}

export function withdraw(state) {
  const next = structuredClone(state); next.phase = 'withdrawn';
  return next;
}

// Re-evaluate after upstream completion so revocation/expiry during recall fails shut.
// The authority owns record filtering and binds its decision to the exact payload.
export async function guardedRecall({ state, request, now, fetchRecall, privacyAuthority }) {
  const boundRequest = frozenJson(request);
  require(canRecall(await state(), boundRequest, now()), 'recall denied');
  require(typeof privacyAuthority?.filterAndBind === 'function' && typeof privacyAuthority?.verify === 'function', 'privacy authority absent');
  const raw = await fetchRecall();
  const bounded = frozenJson(await privacyAuthority.filterAndBind(raw, boundRequest));
  require(await privacyAuthority.verify(bounded, boundRequest), 'privacy decision invalid');
  require(canRecall(await state(), boundRequest, now()), 'recall revoked');
  return bounded;
}
