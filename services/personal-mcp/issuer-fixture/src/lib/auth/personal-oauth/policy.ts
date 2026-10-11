import { createHash, randomBytes } from 'node:crypto';

export const hash = (v: string) => createHash('sha256').update(v).digest('hex');
export const challenge = (v: string) => createHash('sha256').update(v).digest('base64url');
export const opaque = () => randomBytes(32).toString('base64url');
export class OAuthError extends Error {
  constructor(public code: string, public status = 400) { super(code); }
}
const deny = (code = 'invalid_grant'): never => { throw new OAuthError(code); };
export type Client = { clientId: string; callbacks: string[] };
export type Config = { issuer: string; resource: string; clientId?: string; callbacks?: string[]; clients?: Client[] };
export const clients = (c: Config): Client[] => c.clients ?? [{ clientId: c.clientId || '', callbacks: c.callbacks || [] }];
export type Identity = { provider: string; providerId: string };
export type Admission = { subject: string; version: number; enabled: boolean };
export type Binding = { clientId: string; redirectUri: string; resource: string; scopes: string[]; state: string; challenge: string; grantVersion?: number };
export type Pending = Binding & { key: string; sessionHash: string; csrfHash: string; subject: string; version: number; expiresAt: Date };
export type Code = Binding & { key: string; subject: string; version: number; expiresAt: Date };
export type Access = { key: string; issuedAt: Date; expiresAt: Date };
export type Family = { key: string; clientId: string; subject: string; version: number; grantVersion?: number; resource: string; scopes: string[];
  refreshHash: string; used: string[]; access: Access[]; revoked: boolean; expiresAt: Date; generation: number };
export interface Store {
  resolve(identity: Identity): Promise<string>;
  admission(subject: string): Promise<Admission | null>;
  clientAdmission?(subject: string, clientId: string): Promise<Admission | null>;
  putPending(p: Pending): Promise<void>;
  takePending(key: string, sessionHash: string, csrfHash: string, subject: string, at: Date): Promise<Pending | null>;
  putCode(c: Code): Promise<void>;
  takeCode(key: string, clientId: string, redirectUri: string, resource: string, challenge: string, at: Date): Promise<Code | null>;
  putFamily(f: Family): Promise<void>;
  findRefresh(key: string, clientId: string): Promise<Family | null>;
  findAccess(key: string): Promise<Family | null>;
  rotate(key: string, refreshHash: string, generation: number, next: Family, at: Date): Promise<boolean>;
  revoke(key: string): Promise<void>;
}
export const scopes = ['memory:recall'];
export function validateConfig(c: Config) {
  if (c.clients !== undefined && (!Array.isArray(c.clients) || c.clients.some(v => !v || typeof v.clientId !== 'string' || !Array.isArray(v.callbacks) || v.callbacks.some(cb => typeof cb !== 'string')))) deny('invalid_configuration');
  const registered = clients(c);
  if (c.clients && (c.clientId !== undefined || c.callbacks !== undefined)) deny('invalid_configuration');
  if (!registered.length || registered.length > 16 || new Set(registered.map(v => v.clientId)).size !== registered.length) deny('invalid_configuration');
  const callbacks = registered.flatMap(v => v.callbacks);
  if (new Set(callbacks).size !== callbacks.length || registered.some(v => !/^[A-Za-z0-9_-]{1,128}$/.test(v.clientId) || !v.callbacks.length || v.callbacks.length > 8)) deny('invalid_configuration');
  for (const v of [c.issuer, c.resource, ...callbacks]) {
    const u = new URL(v); if (u.protocol !== 'https:' || u.username || u.password || u.hash) deny('invalid_configuration');
  }
  if (new URL(c.issuer).pathname !== '/' || new URL(c.issuer).search || c.issuer.endsWith('/')) deny('invalid_configuration');
}
export class PersonalOAuth {
  constructor(public config: Config, private store: Store, private clock = () => new Date()) { validateConfig(config); }
  metadata() { const c = this.config; return { issuer: c.issuer, authorization_endpoint: c.issuer + '/oauth/authorize',
    token_endpoint: c.issuer + '/oauth/token', revocation_endpoint: c.issuer + '/oauth/revoke', introspection_endpoint: c.issuer + '/oauth/introspect',
    response_types_supported: ['code'], grant_types_supported: ['authorization_code', 'refresh_token'],
    token_endpoint_auth_methods_supported: ['none'], introspection_endpoint_auth_methods_supported: ['client_secret_basic'],
    code_challenge_methods_supported: ['S256'], scopes_supported: scopes, authorization_response_iss_parameter_supported: true }; }
  binding(input: URLSearchParams): Binding {
    for (const k of input.keys()) if (input.getAll(k).length !== 1) deny('invalid_request');
    const get = (k: string) => input.get(k) || '';
    if (!clients(this.config).find(c => c.clientId === get('client_id'))?.callbacks.includes(get('redirect_uri'))) deny('invalid_request');
    if (get('resource') !== this.config.resource) deny('invalid_target');
    if (get('response_type') !== 'code' || get('code_challenge_method') !== 'S256' || !/^[A-Za-z0-9_-]{43}$/.test(get('code_challenge'))) deny('invalid_request');
    const selected = get('scope').split(' ').filter(Boolean);
    if (!selected.length || selected.some(s => !scopes.includes(s)) || new Set(selected).size !== selected.length) deny('invalid_scope');
    if (!get('state') || get('state').length > 1024 || /[\r\n]/.test(get('state'))) deny('invalid_request');
    return { clientId: get('client_id'), redirectUri: get('redirect_uri'), resource: get('resource'), scopes: selected, state: get('state'), challenge: get('code_challenge') };
  }
  private async admitted(subject: string, version?: number) {
    const a = await this.store.admission(subject);
    if (!a?.enabled || (version !== undefined && a.version !== version)) deny('access_denied');
    return a!;
  }
  private async grant(subject: string, clientId: string, version?: number) {
    if (!clients(this.config).some(c => c.clientId === clientId)) deny('access_denied');
    if (!this.config.clients && version === undefined) return undefined; // Legacy grants only; never downgrade a versioned family.
    const a = await this.store.clientAdmission?.(subject, clientId);
    if (!a?.enabled || !Number.isSafeInteger(a.version) || a.version < 0 || (version !== undefined && a.version !== version)) deny('access_denied');
    return a!.version;
  }
  async begin(b: Binding, identity: Identity, session: string) {
    const subject = await this.store.resolve(identity); const a = await this.admitted(subject);
    const grantVersion = await this.grant(subject, b.clientId);
    const transaction = opaque(), csrf = opaque();
    await this.store.putPending({ ...b, ...(grantVersion === undefined ? {} : { grantVersion }), key: hash(transaction), csrfHash: hash(csrf), sessionHash: hash(session), subject, version: a.version,
      expiresAt: new Date(this.clock().getTime() + 300_000) });
    return { transaction, csrf, scopes: b.scopes };
  }
  async consent(transaction: string, csrf: string, session: string, identity: Identity, origin: string | null, approve: boolean) {
    if (origin !== this.config.issuer || !session || !/^[A-Za-z0-9_-]{43}$/.test(transaction) || !/^[A-Za-z0-9_-]{43}$/.test(csrf)) deny('access_denied');
    const subject = await this.store.resolve(identity);
    const p = await this.store.takePending(hash(transaction), hash(session), hash(csrf), subject, this.clock());
    if (!p) deny(); await this.admitted(subject, p!.version);
    await this.grant(subject, p!.clientId, p!.grantVersion);
    if (!clients(this.config).find(c => c.clientId === p!.clientId)?.callbacks.includes(p!.redirectUri) || p!.resource !== this.config.resource) deny();
    const target = new URL(p!.redirectUri); target.searchParams.set('state', p!.state); target.searchParams.set('iss', this.config.issuer);
    if (!approve) { target.searchParams.set('error', 'access_denied'); return target.href; }
    const code = opaque();
    const { sessionHash: _s, csrfHash: _c, ...binding } = p!;
    await this.store.putCode({ ...binding, key: hash(code), expiresAt: new Date(this.clock().getTime() + 60_000) });
    target.searchParams.set('code', code); return target.href;
  }
  private mint(f: Family) {
    const access = opaque(), refresh = opaque(), now = this.clock();
    const next = { ...f, refreshHash: hash(refresh), access: [...f.access, { key: hash(access), issuedAt: now, expiresAt: new Date(now.getTime() + 900_000) }] };
    return { next, response: { access_token: access, token_type: 'Bearer', expires_in: 900, refresh_token: refresh, scope: f.scopes.join(' ') } };
  }
  async exchange(input: URLSearchParams) {
    for (const k of input.keys()) if (input.getAll(k).length !== 1) deny('invalid_request');
    const client = input.get('client_id') || '', resource = input.get('resource') || '';
    if (input.has('client_secret')) deny('invalid_client');
    const registered = clients(this.config).find(c => c.clientId === client);
    if (!registered) deny('invalid_client');
    if (resource !== this.config.resource) deny('invalid_target');
    if (input.get('grant_type') === 'refresh_token') return this.refresh(input.get('refresh_token') || '', client, resource);
    if (input.get('grant_type') !== 'authorization_code') deny('unsupported_grant_type');
    const verifier = input.get('code_verifier') || '', code = input.get('code') || '', redirect = input.get('redirect_uri') || '';
    if (!/^[A-Za-z0-9._~-]{43,128}$/.test(verifier) || !/^[A-Za-z0-9_-]{43}$/.test(code) || !registered!.callbacks.includes(redirect)) deny();
    const c = await this.store.takeCode(hash(code), client, redirect, resource, challenge(verifier), this.clock());
    if (!c || c.scopes.length !== 1 || c.scopes[0] !== 'memory:recall') deny(); await this.admitted(c!.subject, c!.version);
    if (this.config.clients && c!.grantVersion === undefined) deny();
    await this.grant(c!.subject, client, c!.grantVersion);
    const f: Family = { key: opaque(), clientId: client, subject: c!.subject, version: c!.version, resource,
      scopes: c!.scopes, ...(c!.grantVersion === undefined ? {} : { grantVersion: c!.grantVersion }), refreshHash: '', used: [], access: [], revoked: false, generation: 0,
      expiresAt: new Date(this.clock().getTime() + 7 * 86400_000) };
    const minted = this.mint(f); await this.store.putFamily(minted.next); return minted.response;
  }
  private async refresh(token: string, client: string, resource: string) {
    if (!/^[A-Za-z0-9_-]{43}$/.test(token)) deny();
    const h = hash(token), f = await this.store.findRefresh(h, client);
    if (!f || f.revoked || f.expiresAt <= this.clock() || f.resource !== resource || f.scopes.length !== 1 || f.scopes[0] !== 'memory:recall') deny();
    if (f!.used.includes(h) || f!.generation >= 128) { await this.store.revoke(f!.key); deny(); }
    await this.admitted(f!.subject, f!.version);
    if (this.config.clients && f!.grantVersion === undefined) deny();
    await this.grant(f!.subject, client, f!.grantVersion);
    const minted = this.mint({ ...f!, generation: f!.generation + 1, used: [...f!.used, h], access: f!.access.filter(a => a.expiresAt > this.clock()) });
    if (!await this.store.rotate(f!.key, h, f!.generation, minted.next, this.clock())) { await this.store.revoke(f!.key); deny(); }
    return minted.response;
  }
  async introspect(token: string) {
    if (!/^[A-Za-z0-9_-]{43}$/.test(token)) return { active: false };
    const h = hash(token), f = await this.store.findAccess(h);
    if (!f || f.revoked || f.expiresAt <= this.clock() || f.scopes.length !== 1 || f.scopes[0] !== 'memory:recall') return { active: false };
    const a = f.access.find(a => a.key === h && a.expiresAt > this.clock());
    const admission = await this.store.admission(f.subject);
    if (!a || !admission?.enabled || admission.version !== f.version) return { active: false };
    if (f.resource !== this.config.resource || (this.config.clients && f.grantVersion === undefined)) return { active: false };
    try { await this.grant(f.subject, f.clientId, f.grantVersion); } catch (e) { if (e instanceof OAuthError) return { active: false }; throw e; }
    return { active: true, iss: this.config.issuer, aud: f.resource, sub: f.subject, client_id: f.clientId,
      iat: Math.floor(a.issuedAt.getTime() / 1000), exp: Math.floor(a.expiresAt.getTime() / 1000), scope: f.scopes.join(' ') };
  }
  async revoke(token: string, client: string) {
    if (!clients(this.config).some(c => c.clientId === client)) deny('invalid_client');
    const h = hash(token), f = await this.store.findRefresh(h, client) || await this.store.findAccess(h);
    if (f?.clientId === client) await this.store.revoke(f.key);
  }
}
