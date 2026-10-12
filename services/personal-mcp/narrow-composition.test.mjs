import { test } from 'node:test';
import assert from 'node:assert/strict';
import { start } from './server.mjs';
const config = { port: 8799, mode: 'active', resource: 'https://mcp.example/personal/mcp',
  allowedSubjects: ['synthetic'], allowedClients: ['dot', 'claude'],
  clientPolicies: [], muninnVault: 'percival_connection_test', enabledTools: ['muninn_recall'],
  issuer: { credentialBrokerSocketPath: '/run/percival-personal-vault-broker.sock' },
  upstreams: { muninn_recall: { url: 'http://127.0.0.1:8750/mcp', backendProtocol: 'muninn-unary-json',
    credentialBrokerSocketPath: '/run/percival-personal-vault-broker.sock' } } };
test('narrow active composition denies private scope, LifeGraph and alternate credential/network paths before access', async () => {
  for (const mutate of [v => v.muninnVault = 'user_likesjx', v => v.enabledTools = ['life.recall'],
    v => delete v.clientPolicies, v => v.issuer.credentialBrokerSocketPath = '/run/general.sock',
    v => v.upstreams.muninn_recall.credentialEnv = 'PRIVATE_SECRET',
    v => v.upstreams.muninn_recall.credentialSecretRef = 'secret://hotel/default/private/key',
    v => v.upstreams.muninn_recall.credentialBrokerSocketPath = '/run/general.sock',
    v => v.upstreams.muninn_recall.url = 'https://remote.example/mcp',
    v => v.upstreams.muninn_recall.url = 'http://127.0.0.1:8750/mcp?private=1',
    v => v.upstreams.muninn_recall.backendProtocol = 'sse']) {
    const value = structuredClone(config); mutate(value);
    await assert.rejects(start(value, {}), /Synthetic-only native profile required/);
  }
});
