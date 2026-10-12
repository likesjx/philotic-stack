import { timingSafeEqual, createHash } from 'node:crypto';
import { OAuthError } from './policy';
export const headers = { 'Cache-Control': 'no-store', Pragma: 'no-cache', 'Referrer-Policy': 'no-referrer',
  'X-Content-Type-Options': 'nosniff', 'Content-Security-Policy': "default-src 'none'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'" };
export async function form(req: Request) {
  if (!/^application\/x-www-form-urlencoded(?:;|$)/i.test(req.headers.get('content-type') || '')) throw new OAuthError('invalid_request', 415);
  const reader = req.body?.getReader(); if (!reader) throw new OAuthError('invalid_request');
  const chunks: Uint8Array[] = []; let size = 0;
  for (;;) { const { value, done } = await reader.read(); if (done) break; size += value.length;
    if (size > 8192) { await reader.cancel(); throw new OAuthError('invalid_request', 413); } chunks.push(value); }
  const p = new URLSearchParams(Buffer.concat(chunks).toString('utf8'));
  for (const k of p.keys()) if (p.getAll(k).length !== 1) throw new OAuthError('invalid_request');
  return p;
}
export function introspectionVerifier(client: string, secret: string) {
  if (!client || /[:\r\n]/.test(client) || secret.startsWith('mk_') || !/^[A-Za-z0-9_-]{43,128}$/.test(secret)) throw new OAuthError('invalid_client');
  return createHash('sha256').update('Basic ' + Buffer.from(encodeURIComponent(client) + ':' + encodeURIComponent(secret)).toString('base64')).digest('hex');
}
export function confidential(req: Request, client: string, verifier: string) {
  // SHA256 of the complete expected Basic header. Only a verifier is configured
  // in the issuer; the raw random secret belongs to the encrypted hotel vault.
  if (!client || /[:\r\n]/.test(client) || !/^[a-f0-9]{64}$/.test(verifier)) throw new OAuthError('temporarily_unavailable', 503);
  const supplied = req.headers.get('authorization') || '';
  if (supplied.length > 8192 || !supplied.startsWith('Basic ')) throw new OAuthError('invalid_client', 401);
  let username: string;
  try { username = decodeURIComponent(Buffer.from(supplied.slice(6), 'base64').toString('utf8').split(':')[0]); }
  catch { throw new OAuthError('invalid_client', 401); }
  if (username !== client) throw new OAuthError('invalid_client', 401);
  const actual = createHash('sha256').update(supplied).digest();
  if (!timingSafeEqual(actual, Buffer.from(verifier, 'hex'))) throw new OAuthError('invalid_client', 401);
}
export async function respond(action: () => Promise<Response | unknown>) {
  try { const r = await action(); return r instanceof Response ? r : Response.json(r, { headers }); }
  catch (e) { return Response.json({ error: e instanceof OAuthError ? e.code : 'temporarily_unavailable' }, { status: e instanceof OAuthError ? e.status : 503, headers }); }
}
export const escapeHtml = (s: string) => s.replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]!));
