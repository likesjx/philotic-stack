// Test-only stdin contains a disposable synthetic token, never a live credential.
import { pathToFileURL } from 'node:url';
const { frontdoorAdapter, recallArguments } = await import(pathToFileURL(process.argv[2]));
let raw = '';
for await (const chunk of process.stdin) {
  raw += chunk;
  if (raw.length > 4096) throw new Error('fixture input limit');
}
const fixture = JSON.parse(raw);
if (fixture.vault !== 'exact_observe_a' || fixture.marker !== 'SYNTHETIC_AMBER_OBSERVE_7381')
  throw new Error('synthetic fixture required');
const upstream = frontdoorAdapter({ enabledTools: ['muninn_recall'], endpoints: {
  muninn_recall: {url: 'http://127.0.0.1:18475/mcp', backendProtocol: 'muninn-unary-json',
    credential: async () => fixture.token}
}});
const args = recallArguments('muninn_recall', {context: [fixture.marker], limit: 10, threshold: 0}, fixture.vault);
const result = await upstream.call('muninn_recall', args, {signal: AbortSignal.timeout(8000)});
if (result.isError || !JSON.stringify(result).includes(fixture.marker) ||
    JSON.stringify(result).includes('SYNTHETIC_COBALT_PRIVATE_9426')) throw new Error('recall isolation failed');
let overrideDenied = false, writeDenied = false;
try { recallArguments('muninn_recall', {context: [fixture.marker], vault: 'exact_observe_b'}, fixture.vault); }
catch (error) { overrideDenied = error.status === 403 && error.code === 'vault_not_granted'; }
try { await upstream.call('muninn_remember', {content: 'SYNTHETIC_DENIED_WRITE'}); }
catch (error) { writeDenied = error.status === 403 && error.code === 'tool_not_enabled'; }
if (!overrideDenied || !writeDenied) throw new Error('gateway scope failure');
console.log(JSON.stringify({actualGatewayAdapter: 'pass', vaultOverride: 'denied', writeTool: 'denied'}));
