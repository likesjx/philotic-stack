// Authority-owned, process-local release coordination. No grants are created or
// cached here. Every issuer/graph writer must participate in this same owner;
// an external issuer that only provides introspection cannot use this boundary.
import { performance } from 'node:perf_hooks';
import { createHash } from 'node:crypto';
const unavailable = () => { throw new Error('LifeGraph release unavailable'); };
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);

export function createLifeGraphReleaseCoordinator({ issuerAuthority, policyAuthority, graphAuthority,
  maxLeaseMs = 5000, maxPending = 32 }) {
  if (typeof issuerAuthority?.resolveCurrent !== 'function' || typeof policyAuthority?.pin !== 'function' ||
      typeof graphAuthority?.readSnapshot !== 'function' || typeof graphAuthority?.currentRevision !== 'function' ||
      !Number.isInteger(maxLeaseMs) || maxLeaseMs < 1 || maxLeaseMs > 5000 ||
      !Number.isInteger(maxPending) || maxPending < 1 || maxPending > 64) unavailable();
  const responses = new WeakMap(), queue = [], active = new Set(), controllers = new Set();
  let held = false, stopped = false;
  function acquire(signal) {
    signal.throwIfAborted();
    if (stopped || queue.length >= maxPending) unavailable();
    return new Promise((resolve, reject) => {
      const item = { grant() {
        signal.removeEventListener('abort', cancel);
        if (signal.aborted || stopped) { reject(new Error('LifeGraph release unavailable')); next(); return; }
        held = true;
        let released = false;
        resolve(() => { if (!released) { released = true; held = false; next(); } });
      } };
      const cancel = () => {
        const i = queue.indexOf(item);
        if (i !== -1) queue.splice(i, 1);
        reject(new Error('LifeGraph release unavailable'));
      };
      signal.addEventListener('abort', cancel, { once: true });
      queue.push(item); next();
    });
  }
  function next() { if (!held && queue.length) queue.shift().grant(); }
  function scope(parent) {
    const controller = new AbortController(), deadline = performance.now() + maxLeaseMs;
    controllers.add(controller);
    const cancel = () => controller.abort();
    parent?.addEventListener('abort', cancel, { once: true });
    if (parent?.aborted) cancel();
    const timer = setTimeout(cancel, maxLeaseMs); timer.unref?.();
    return { signal: controller.signal, check() {
      controller.signal.throwIfAborted();
      if (stopped || performance.now() >= deadline) unavailable();
    }, clear() { controllers.delete(controller); clearTimeout(timer); parent?.removeEventListener('abort', cancel); } };
  }
  async function exclusive(context, work) {
    const s = scope(context?.signal); let unlock;
    try {
      unlock = await acquire(s.signal); s.check();
      const value = await work({ ...context, signal: s.signal }); s.check(); return value;
    } finally { s.clear(); unlock?.(); }
  }
  const api = {
    bindResponse(request, response) {
      if (stopped || !request || responses.has(request) || typeof response?.once !== 'function' ||
          typeof response.destroy !== 'function') unavailable();
      const binding = { response, finished: false, used: false, cleanup: null };
      responses.set(request, binding);
      const finish = () => {
        if (binding.finished) return;
        binding.finished = true; responses.delete(request);
        binding.cleanup?.();
      };
      response.once('finish', finish); response.once('close', finish);
    },
    readSnapshot(context) { return exclusive(context, c => graphAuthority.readSnapshot(c)); },
    verifyResponse(request, result) {
      const binding = responses.get(request);
      if (!binding || !active.has(binding) || binding.finished || binding.response.destroyed ||
          !Array.isArray(result?.content) || result.content.length !== 1 || result.content[0]?.type !== 'text' ||
          typeof result.content[0].text !== 'string') return false;
      return createHash('sha256').update(result.content[0].text).digest('hex') === binding.responseDigest;
    },
    // These methods wrap the canonical owner's existing mutation transaction.
    // They do not implement or authorize a grant or graph mutation themselves.
    participateIssuerChange(context, mutation) {
      if (typeof context?.clientId !== 'string' || !context.clientId || typeof mutation !== 'function') unavailable();
      return exclusive(context, mutation);
    },
    participateGraphChange(context, mutation) {
      if (typeof mutation !== 'function') unavailable();
      return exclusive(context, mutation);
    },
    async admit(admission, validateCurrentPolicy) {
      const binding = responses.get(admission.request);
      if (!binding || binding.used || binding.finished || typeof validateCurrentPolicy !== 'function' ||
          !/^[a-f0-9]{64}$/.test(admission.responseDigest ?? '')) return false;
      binding.used = true;
      const s = scope(admission.signal); let unlock, lease, transferred = false;
      const cleanup = async () => {
        s.clear();
        // Keep the gate until the real reservation is released. A hung owner
        // fails closed; its supervisor must terminate/recover that owner.
        try { await lease?.release(); }
        catch (error) { stopped = true; throw error; }
        finally { active.delete(binding); unlock?.(); }
      };
      try {
        unlock = await acquire(s.signal); s.check();
        const actor = await issuerAuthority.resolveCurrent({ request: admission.request,
          actor: structuredClone(admission.actor), signal: s.signal }); s.check();
        if (!same(actor, admission.actor)) return false;
        lease = await policyAuthority.pin({ actor, namespace: admission.namespace, signal: s.signal });
        if (typeof lease?.release !== 'function') unavailable();
        s.check();
        const graphRevision = await graphAuthority.currentRevision({ actor,
          namespace: admission.namespace, signal: s.signal }); s.check();
        if (validateCurrentPolicy({ actor, graphRevision }) !== true) return false;
        s.check();
        if (binding.finished || binding.response.destroyed || binding.response.writableEnded) return false;
        // This request-bound single-use permit retains the issuer/graph gate and
        // actual policy reservation through HTTP finish/close, not merely until
        // an asynchronous boolean validator returns.
        binding.cleanup = () => { binding.cleanup = null; void cleanup().catch(() => { stopped = true; }); };
        const abort = () => binding.response.destroy();
        s.signal.addEventListener('abort', abort, { once: true });
        const prior = binding.cleanup;
        binding.cleanup = () => { s.signal.removeEventListener('abort', abort); prior(); };
        binding.responseDigest = admission.responseDigest;
        active.add(binding); transferred = true; return true;
      } catch { return false; }
      finally { if (!transferred) await cleanup(); }
    },
    close() { stopped = true; for (const controller of controllers) controller.abort();
      for (const binding of active) binding.response.destroy(); next(); },
  };
  return Object.freeze(api);
}
