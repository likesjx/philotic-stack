// Canonical server-owned bindings shared by initial and fenced final admission.
// This adapter never upgrades a memory token or supplies roles from request data.
const fail = () => { throw new Error('LifeGraph identity unavailable'); };
const text = x => typeof x === 'string' && x.trim() && x.length <= 128;
export function createLifeGraphIdentityAuthority({ issuer, resource, resolveBinding, maxLifetimeSeconds = 900, clock = () => Date.now() }) {
  if (typeof issuer?.inspect !== 'function' || typeof issuer.issuer !== 'string' ||
      typeof resource !== 'string' || typeof resolveBinding !== 'function' ||
      !Number.isInteger(maxLifetimeSeconds) || maxLifetimeSeconds < 1 || maxLifetimeSeconds > 900 || typeof clock !== 'function') fail();
  const responses = new WeakMap();
  const valid = token => {
    const now = Math.floor(clock() / 1000);
    return token?.active === true && token.iss === issuer.issuer && token.aud === resource && token.scope === 'life:recall' &&
      Number.isSafeInteger(token.iat) && token.iat <= now && Number.isSafeInteger(token.exp) && token.exp > now &&
      token.exp > token.iat && token.exp - token.iat <= maxLifetimeSeconds;
  };
  async function resolve({ subject, clientId, grantVersion, signal }) {
    signal?.throwIfAborted();
    if (![subject, clientId, grantVersion].every(text)) fail();
    // The provider must load current canonical roles and the exact current grant
    // binding. No policy/grant creation, mutation or issuer-change lease is allowed
    // here: final resolution already runs inside the daemon release fence.
    const b = structuredClone(await resolveBinding({ subject, clientId, grantVersion, signal }));
    signal?.throwIfAborted();
    if (!b || Object.keys(b).some(k => !['subject','clientId','grantVersion','agentId','roles'].includes(k)) ||
        b.subject !== subject || b.clientId !== clientId || b.grantVersion !== grantVersion || !text(b.agentId) ||
        !Array.isArray(b.roles) || b.roles.length > 32 || !b.roles.every(text) || new Set(b.roles).size !== b.roles.length) fail();
    return { agentId: b.agentId, roles: b.roles };
  }
  return Object.freeze({ resolve,
    bindResponse(request, response) {
      if (responses.has(request) || typeof response?.once !== 'function' || typeof response.destroy !== 'function') fail();
      const binding = { response, expiry: 0, timer: null }; responses.set(request, binding);
      const cleanup = () => { clearTimeout(binding.timer); responses.delete(request); };
      response.once('finish', cleanup); response.once('close', cleanup);
    },
    verifyResponse(request) {
      const binding = responses.get(request);
      return !!binding && !binding.response.destroyed && binding.expiry > clock();
    },
    async resolveCurrent({ request, signal }) {
      signal?.throwIfAborted();
      const authorization = request?.headers?.authorization;
      if (typeof authorization !== 'string' || !/^Bearer [\x21-\x7e]{1,4096}$/.test(authorization)) fail();
      const token = structuredClone(await issuer.inspect(authorization.slice(7), { signal }));
      signal?.throwIfAborted(); if (!valid(token)) fail();
      const identity = await resolve({ subject: token.sub, clientId: token.client_id, grantVersion: token.grant_version, signal });
      if (!valid(token)) fail();
      const binding = responses.get(request);
      if (binding) {
        binding.expiry = token.exp * 1000;
        clearTimeout(binding.timer);
        binding.timer = setTimeout(() => binding.response.destroy(), Math.max(0, binding.expiry - clock()));
        binding.timer.unref?.();
      }
      return { active:true, audience:resource, clientId:token.client_id, subject:token.sub, scope:'life:recall',
        ...identity, grantVersion:token.grant_version };
    },
  });
}
