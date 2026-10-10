// Concrete source composition: protected catalog file, canonical policy lease,
// fixed owner transport and response-lifetime coordinator. No live registration
// or implicit production admission. Issuer integration remains its owner's seam.
import { loadLifeGraphCatalog } from './lifegraph-catalog.mjs';
import { createLifeGraphCanonicalPolicyOwner } from './lifegraph-policy-owner.mjs';
import { createLifeGraphOwnerTransport } from './lifegraph-owner-transport.mjs';
import { createLifeGraphMemgraphReader } from './lifegraph-memgraph.mjs';
import { createLifeGraphReleaseCoordinator } from './lifegraph-release.mjs';
import { createLifeGraphStorageAuthority } from './lifegraph-storage.mjs';
export function createLifeGraphOwnedStorage({ enabled = false, profile, policyDatabase, storeIdentity,
  catalogFile, catalogDigest, ownerSocket, ownerIdentity, issuerAuthority }) {
  if (enabled === false) return null;
  if (enabled !== true || profile !== 'synthetic-read-only-v1' || typeof issuerAuthority?.resolveCurrent !== 'function')
    throw new Error('LifeGraph owner composition unavailable');
  const catalogs = loadLifeGraphCatalog({ catalogFile, expectedDigest: catalogDigest });
  const policyOwner = createLifeGraphCanonicalPolicyOwner({ policyDatabase, storeIdentity });
  let coordinator, storage;
  try {
    const transport = createLifeGraphOwnerTransport({ socketPath: ownerSocket, ownerIdentity });
    const reader = createLifeGraphMemgraphReader({ transport, catalogs });
    coordinator = createLifeGraphReleaseCoordinator({ issuerAuthority, policyAuthority: policyOwner,
      graphAuthority: { readSnapshot: c => reader.readSnapshot(c), currentRevision: async c => (await reader.readSnapshot(c)).revision,
        async pinRelease(context) {
          const lease = await transport.pinRelease(context);
          const pinnedReader = createLifeGraphMemgraphReader({ transport: lease, catalogs });
          return Object.freeze({ validate: () => lease.validate(), release: () => lease.release(),
            currentRevision: async c => (await pinnedReader.readSnapshot(c)).revision });
        } } });
    storage = createLifeGraphStorageAuthority({ policyDatabase, graphReader: coordinator, releaseBarrier: coordinator, storeIdentity });
    return Object.freeze({ snapshot: c => storage.snapshot(c), authorizeRelease: a => storage.authorizeRelease(a),
      bindResponse: (req,res) => storage.bindResponse(req,res), verifyResponse: (req,result) => storage.verifyResponse(req,result),
      participateIssuerChange() { throw new Error('LifeGraph issuer must use its daemon enrollment'); },
      participateGraphChange() { throw new Error('LifeGraph mutations are disabled'); },
      close() { coordinator.close(); storage.close(); policyOwner.close(); } });
  } catch (error) { coordinator?.close(); storage?.close(); policyOwner.close(); throw error; }
}
