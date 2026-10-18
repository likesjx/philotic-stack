// Composition boundary only. Canonical issuer owns admissions; canonical scope/
// privacy owner owns real-record eligibility. No local second grant store.
import { guardedRecall } from './persistent-recall.mjs';
import { recallArguments } from './gateway.mjs';
export function createPersistentOwnerBoundary({ resource, clients, issuer, scopeOwner, lifecycleOwner, privacyAuthority, upstream, clock = Date.now }) {
  if (!Array.isArray(clients) || clients.length < 1 || clients.length > 2 || new Set(clients).size !== clients.length ||
      clients.some(x => typeof x !== 'string' || !x.trim()) ||
      typeof issuer?.authorizeRecall !== 'function' || typeof scopeOwner?.resolve !== 'function' ||
      typeof lifecycleOwner?.read !== 'function' || typeof upstream?.recall !== 'function' ||
      typeof privacyAuthority?.filterAndBind !== 'function' || typeof privacyAuthority?.verify !== 'function')
    throw new Error('persistent owners missing');
  const allowed = new Set(clients);
  return Object.freeze({
    async recall({ accessToken, vault, arguments: args, signal }) {
      signal?.throwIfAborted();
      // Token remains an opaque private value. Only issuer receives it.
      const query = recallArguments('muninn_recall', structuredClone(args), vault);
      const authorization = await issuer.authorizeRecall({ accessToken, resource, scope: 'memory:recall', signal });
      signal?.throwIfAborted();
      if (!authorization || !allowed.has(authorization.clientId) || typeof authorization.subject !== 'string' ||
          !(typeof authorization.authorizationBinding === 'string' && /^[a-f0-9]{64}$/.test(authorization.authorizationBinding) ||
            Number.isSafeInteger(authorization.clientVersion) && authorization.clientVersion >= 0))
        throw new Error('authorization denied');
      const bound = Object.freeze({ subject: authorization.subject, clientId: authorization.clientId,
        ...(authorization.authorizationBinding !== undefined ? {authorizationBinding:authorization.authorizationBinding} : {clientVersion:authorization.clientVersion}),
        resource, vault, scope:'memory:recall', tool:'muninn_recall', readOnly:true });
      const readState = async () => {
        signal?.throwIfAborted();
        const scope = structuredClone(await scopeOwner.resolve(bound, {signal}));
        signal?.throwIfAborted();
        const lifecycle = structuredClone(await lifecycleOwner.read());
        signal?.throwIfAborted();
        // Issuer check is last: revocation during either owner read must not use
        // an earlier admission. Atomic transport release still needs owner fencing.
        const current = await issuer.authorizeRecall({ accessToken, resource, scope:'memory:recall', signal });
        signal?.throwIfAborted();
        if (!current || current.subject !== bound.subject || current.clientId !== bound.clientId ||
            current.clientVersion !== bound.clientVersion || current.authorizationBinding !== bound.authorizationBinding || !Number.isSafeInteger(current.expiresAt) ||
            !Number.isSafeInteger(scope.expiresAt)) throw new Error('authorization changed');
        // Scope owner, not request arguments, selects eligible vaults. Lifecycle
        // owner provides the adopted credential's expiry and serving phase.
        return { subject:bound.subject,resource,subjectEnabled:current.enabled === true,
          clients:{[bound.clientId]:{enabled:current.enabled === true,version:current.clientVersion,authorizationBinding:current.authorizationBinding}},
          vaults:scope.vaults,privacyLeaseExpiresAt:Math.min(scope.expiresAt,current.expiresAt),
          phase:lifecycle.phase,activeCredential:{expiresAt:lifecycle.credentialExpiresAt} };
      };
      return guardedRecall({state:readState,request:bound,now:()=>Math.floor(clock()/1000),privacyAuthority,signal,
        fetchRecall:()=>upstream.recall({arguments:query,vault:bound.vault,readOnly:true,...(signal ? {signal} : {})})});
    }
  });
}
