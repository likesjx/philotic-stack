// Explicit issuer-owned delegated binding, or an embedding owner's canonical
// read-only resolver. Never load executable adapters/roles from client claims.
import { createLifeGraphIdentityAuthority } from './lifegraph-identity.mjs';
import { createLifeGraphOwnedStorage } from './lifegraph-owned-storage.mjs';
import { createLifeGraphGateway } from './lifegraph-gateway.mjs';
export async function createApprovedLifeGraphGateway({ config, issuer, resolveBinding }) {
  if (resolveBinding === undefined && typeof issuer?.resolveBinding === 'function')
    resolveBinding = c => issuer.resolveBinding({ ...c, resource: config.resource });
  if (!config || config.profile !== 'approved-read-only-v1' || typeof resolveBinding !== 'function' || !config.storage)
    throw new Error('Explicit canonical LifeGraph identity and storage required');
  const identityAuthority = createLifeGraphIdentityAuthority({ issuer, resource: config.resource, resolveBinding });
  return createLifeGraphGateway({ enabled: true, config, issuer, identityAuthority,
    storageAuthorityFactory: () => createLifeGraphOwnedStorage({ ...config.storage,
      enabled: true, profile: config.profile, issuerAuthority: identityAuthority }) });
}
