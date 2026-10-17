import { createHash } from 'node:crypto';
import { issuerAdapter } from './gateway.mjs';

// Uses the existing confidential introspection protocol. The canonical issuer
// checks admission/family versions internally; memory introspection does not
// expose grant_version. This binding identifies the same token, NOT a grant epoch.
export async function createCanonicalIssuerOwner({ issuer, discovery, introspection,
  resource, subjects, clients, clientId, credential, fetchImpl, clock = Date.now }) {
  if (!Array.isArray(subjects) || !subjects.length || !Array.isArray(clients) || clients.length !== 2 ||
      new Set(clients).size !== 2 || [...subjects,...clients].some(v=>typeof v!=='string'||!v.trim()) ||
      typeof credential !== 'function') throw new Error('exact issuer scope and broker credential provider required');
  const allowedSubjects=new Set(subjects),allowedClients=new Set(clients);
  const adapter=issuerAdapter({issuer,discovery,introspection,clientId,credential,fetchImpl});
  await adapter.ready({allowPreregistered:true});
  return Object.freeze({async authorizeRecall({accessToken,resource:requested,scope}) {
    if(requested!==resource || scope!=='memory:recall' || typeof accessToken!=='string' ||
       !/^[A-Za-z0-9_-]{43}$/.test(accessToken)) throw new Error('recall authorization denied');
    const c=await adapter.inspect(accessToken);const now=Math.floor(clock()/1000);
    if(c.active!==true || c.iss!==issuer || c.aud!==resource || c.scope!=='memory:recall' ||
       !allowedSubjects.has(c.sub) || !allowedClients.has(c.client_id) ||
       !Number.isSafeInteger(c.exp)||!Number.isSafeInteger(c.iat)||c.iat<0||c.iat>now||
       c.exp<=now||c.exp<=c.iat||c.exp-c.iat>900) throw new Error('recall authorization denied');
    return {subject:c.sub,clientId:c.client_id,enabled:true,expiresAt:c.exp,
      authorizationBinding:createHash('sha256').update(accessToken).digest('hex')};
  }});
}
