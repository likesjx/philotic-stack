// Explicit composition entry point. Nothing starts when this module is imported.
// Production TLS, issuer/client registration and scoped hotel grants are
// operator-owned deployment steps; this program only binds literal loopback.
import { readFile, open } from 'node:fs/promises';
import { vaultCredential } from './vault.mjs';
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
  const credential = async value => {
    const choices = [value.credentialEnv, value.credentialSecretRef, value.credentialSecretRefFile].filter(v => v !== undefined);
    if (choices.length !== 1) throw new Error('Exactly one credential source required');
    if (value.credentialEnv !== undefined) return async () => secret(value.credentialEnv);
    let secretRef = value.credentialSecretRef;
    if (value.credentialSecretRefFile !== undefined) {
      const file = await open(value.credentialSecretRefFile, 'r');
      try {
        const bytes = Buffer.alloc(4097);
        const { bytesRead } = await file.read(bytes, 0, bytes.length, 0);
        if (bytesRead > 4096) throw new Error('Oversized secret reference file');
        secretRef = bytes.subarray(0, bytesRead).toString('utf8').trim();
      } finally { await file.close(); }
    }
    return vaultCredential({ socketPath: config.vault?.socketPath, secretRef });
  };
  const endpoints = Object.fromEntries(await Promise.all(enabledTools.map(async tool => {
    const value = config.upstreams?.[tool];
    if (!value) throw new Error('Selected upstream required');
    return [tool, { url: value.url, credential: await credential(value) }];
  })));
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
