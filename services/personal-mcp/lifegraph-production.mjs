// Embedding entry point: the owner supplies a canonical, read-only identity
// resolver. Never load executable adapters or role mappings from JSON config.
import { createLifeGraphIdentityAuthority } from './lifegraph-identity.mjs';
import { createLifeGraphOwnedStorage } from './lifegraph-owned-storage.mjs';
import { createLifeGraphGateway } from './lifegraph-gateway.mjs';
export async function createApprovedLifeGraphGateway({ config, issuer, resolveBinding }) {
  if (!config || config.profile !== 'approved-read-only-v1' || typeof resolveBinding !== 'function' || !config.storage)
    throw new Error('Explicit canonical LifeGraph identity and storage required');
  const identityAuthority = createLifeGraphIdentityAuthority({ issuer, resource: config.resource, resolveBinding });
  return createLifeGraphGateway({ enabled: true, config, issuer, identityAuthority,
    storageAuthorityFactory: () => createLifeGraphOwnedStorage({ ...config.storage,
      enabled: true, profile: config.profile, issuerAuthority: identityAuthority }) });
}
