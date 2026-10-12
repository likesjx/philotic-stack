// Explicit opt-in composition for the separately scoped LifeGraph resource.
// Does not bind a port, initialize an issuer/broker or provision a credential.
import { createPersonalMcp } from './gateway.mjs';
export async function createLifeGraphGateway({ enabled = false, config, issuer, identityAuthority, storageAuthorityFactory }) {
  if (enabled === false) return null;
  if (enabled !== true || !config || typeof storageAuthorityFactory !== 'function') throw new Error('LifeGraph admission unavailable');
  const storageAuthority = await storageAuthorityFactory();
  try {
    const server = await createPersonalMcp({ resource: config.resource, issuer,
      allowedSubjects: new Set(config.allowedSubjects), allowedClients: new Set(config.allowedClients),
      enabledTools: ['life.recall'], requestDeadlineMs: config.requestDeadlineMs,
      lifeGraph: { enabled: true, profile: config.profile, clientPolicies: config.clientPolicies, identityAuthority, storageAuthority } });
    server.once('close', () => storageAuthority.close?.());
    return server;
  } catch (error) { storageAuthority.close?.(); throw error; }
}
