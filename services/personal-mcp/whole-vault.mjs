// Operator-owned configuration, never a request field or an approval substitute.
export const persistentClients = Object.freeze(['claude_muninn_recall_v1', 'chatgpt_muninn_recall_v1']);
export function validateWholeVaultAccess(access, { resource, muninnVault, allowedSubjects, allowedClients, clientPolicies, tools }) {
  const exact = (a, b) => Array.isArray(a) && a.length === b.length && new Set(a).size === a.length && b.every(x => a.includes(x));
  if (!access || Object.keys(access).sort().join(',') !== 'includeFutureRecords,subject,vault,version' ||
      access.version !== 1 || access.includeFutureRecords !== true || access.vault !== muninnVault ||
      typeof muninnVault !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(muninnVault) ||
      typeof access.subject !== 'string' || !access.subject ||
      !exact([...allowedSubjects], [access.subject]) || !exact([...allowedClients], persistentClients) ||
      !exact(tools, ['muninn_recall']) || !Array.isArray(clientPolicies) || clientPolicies.length !== 2 ||
      !exact(clientPolicies.map(p => p?.clientId), persistentClients) ||
      clientPolicies.some(p => !p || p.vault !== muninnVault || !exact(p.subjects, [access.subject]) ||
        !exact(p.scopes, ['memory:recall']))) throw new Error('Explicit whole-vault two-client mapping required');
  const url = new URL(resource);
  if (url.protocol !== 'https:' || url.username || url.password || url.search || url.hash || !url.pathname.endsWith('/mcp'))
    throw new Error('Exact HTTPS MCP resource required');
  return structuredClone(access);
}
