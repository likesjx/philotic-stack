// Explicit composition entry point. Nothing starts when this module is imported.
// Production TLS, issuer/client registration and scoped hotel grants are
// operator-owned deployment steps; this program only binds literal loopback.
import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { createPersonalMcp, issuerAdapter, frontdoorAdapter, createDiscoveryBootstrap, selectedTools } from './gateway.mjs';

export async function start(config, env = process.env) {
  const secret = name => {
    if (typeof name !== 'string' || !/^[A-Z][A-Z0-9_]*$/.test(name) || typeof env[name] !== 'string' || !env[name]) throw new Error('Required secret environment reference unavailable');
    return env[name];
  };
  if (!Number.isInteger(config.port) || config.port < 1024 || config.port > 65535) throw new Error('Explicit unprivileged port required');
  const enabledTools = selectedTools(config.enabledTools);
  if (config.mode !== undefined && !['active', 'discovery-only'].includes(config.mode)) throw new Error('Unknown mode');
  if (config.mode === 'discovery-only') {
    const server = createDiscoveryBootstrap({ resource: config.resource, issuer: config.issuer.issuer, enabledTools });
    await new Promise((resolve, reject) => { server.once('error', reject); server.listen(config.port, '127.0.0.1', resolve); });
    return server;
  }
  if (!Array.isArray(config.allowedSubjects) || !Array.isArray(config.allowedClients)) throw new Error('Explicit operator/client arrays required');
  const endpoints = Object.fromEntries(Object.entries(config.upstreams || {}).map(([tool, value]) => [tool,
    { url: value.url, credential: async () => secret(value.credentialEnv) }]));
  const issuer = issuerAdapter({ ...config.issuer, clientSecret: secret(config.issuer.clientSecretEnv) });
  const upstream = frontdoorAdapter({ endpoints, enabledTools });
  const server = await createPersonalMcp({ resource: config.resource, issuer, upstream,
    allowedSubjects: new Set(config.allowedSubjects), allowedClients: new Set(config.allowedClients), muninnVault: config.muninnVault, enabledTools });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(config.port, '127.0.0.1', resolve); });
  return server;
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    if (process.argv.length !== 3) throw new Error('Configuration path required');
    const config = JSON.parse(await readFile(process.argv[2], 'utf8'));
    const server = await start(config);
    for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => server.close());
    console.info('Personal MCP recall gateway listening on loopback');
  } catch {
    console.error('Personal MCP startup failed; check nonsecret configuration and issuer readiness');
    process.exitCode = 1;
  }
}
