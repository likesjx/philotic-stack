// Gateway composition over the existing supervised owner; no new grant store.
import {createHash} from 'node:crypto';
const hash=s=>createHash('sha256').update(s,'utf8').digest('hex');
const requireValue=(v)=>{if(!v)throw new Error('memory release denied');};
const integer=v=>Number.isSafeInteger(v)&&v>=0;
export function createMemoryReleaseAdapter({fence,inspect,currentPolicyRevision,clock=Date.now,timeoutMs=4500}) {
 requireValue(typeof fence?.pinRelease==='function'&&typeof inspect==='function'&&typeof currentPolicyRevision==='function'&&integer(timeoutMs)&&timeoutMs>0&&timeoutMs<5000);
 return async function withMemoryRelease({token,binding,policyRevision,signal},prepare,handoff) {
  const b=Object.freeze(structuredClone(binding));
  requireValue(b.schema===1&&b.scope==='memory:recall'&&['issuer','subject','clientId','resource','collection','privacyLabel'].every(k=>typeof b[k]==='string'&&b[k].trim())&&
   /^[a-f0-9]{64}$/.test(policyRevision)&&b.policyRevision===policyRevision&&typeof token==='string'&&/^[A-Za-z0-9_-]{43}$/.test(token)&&typeof prepare==='function'&&typeof handoff==='function');
  const controller=new AbortController(),stop=()=>controller.abort();
  signal?.throwIfAborted();signal?.addEventListener('abort',stop,{once:true});
  const timer=setTimeout(stop,timeoutMs);timer.unref?.();let lease;
  const check=()=>{controller.signal.throwIfAborted();requireValue(lease?.validate()===true);};
  // Race uncooperative callbacks too; abort also closes the native owner channel.
  const bounded=work=>new Promise((resolve,reject)=>{
   const aborted=()=>reject(new Error('memory release abandoned'));
   controller.signal.addEventListener('abort',aborted,{once:true});
   Promise.resolve().then(()=>{controller.signal.throwIfAborted();return work();}).then(resolve,reject)
    .finally(()=>controller.signal.removeEventListener('abort',aborted));
  });
  const authorize=async()=>{
   check();requireValue(await bounded(()=>currentPolicyRevision(b,{signal:controller.signal}))===policyRevision);check();
   const c=await bounded(()=>inspect(token,{signal:controller.signal}));check();
   requireValue(c?.active===true&&c.iss===b.issuer&&c.sub===b.subject&&c.client_id===b.clientId&&c.aud===b.resource&&c.scope===b.scope&&
    integer(c.exp)&&c.exp*1000>clock()&&integer(c.admission_revision?.subject)&&integer(c.admission_revision?.client));
   return {expiresAt:c.exp,subject:c.admission_revision.subject,client:c.admission_revision.client};
  };
  try {
   lease=await bounded(()=>fence.pinRelease({signal:controller.signal}));check();
   const admitted=await authorize();
   // prepare is trusted record-owner integration: validate all records/provenance
   // under this fence or immutable revision BEFORE deriving any output.
   const serialized=await bounded(()=>prepare(b,{signal:controller.signal}));check();
   requireValue(typeof serialized==='string'&&Buffer.byteLength(serialized,'utf8')<=262144);
   const envelope=JSON.parse(serialized);
   requireValue(envelope?.jsonrpc==='2.0'&&Object.hasOwn(envelope,'id')&&
    (envelope.id===null||typeof envelope.id==='string'||Number.isSafeInteger(envelope.id))&&Object.hasOwn(envelope,'result')&&!Object.hasOwn(envelope,'error'));
   const final=await authorize();requireValue(JSON.stringify(final)===JSON.stringify(admitted));check();
   const release=Object.freeze({schema:1,binding:b,admissionRevision:Object.freeze({subject:final.subject,client:final.client}),expiresAt:final.expiresAt,
    tokenSha256:hash(token),responseSha256:hash(serialized),responseLength:Buffer.byteLength(serialized,'utf8'),serialized});
   await bounded(()=>handoff(release,{signal:controller.signal}));check();
   await bounded(()=>lease.release());
   return release.responseSha256;
  } catch(error) {controller.abort();throw error;}
  finally {clearTimeout(timer);signal?.removeEventListener('abort',stop);}
 };
}

export function handoffMemoryResponse(response,release,{signal,clock=Date.now}={}) {
 return new Promise((resolve,reject)=>{
  let done=false;
  const cleanup=()=>{for(const [e,f] of [['finish',finish],['close',failed],['error',failed]])response.removeListener(e,f);signal?.removeEventListener('abort',failed);clearTimeout(expiry);};
  const failed=()=>{if(done)return;done=true;cleanup();response.destroy();reject(new Error('memory transport incomplete'));};
  const finish=()=>{if(done)return;if(signal?.aborted||release.expiresAt*1000<=clock())return failed();done=true;cleanup();resolve();};
  const expiry=setTimeout(failed,Math.max(0,Math.min(release.expiresAt*1000-clock(),2147483647)));expiry.unref?.();
  response.once('finish',finish);response.once('close',failed);response.once('error',failed);signal?.addEventListener('abort',failed,{once:true});
  try {
   requireValue(!signal?.aborted&&!response.destroyed&&!response.writableEnded&&!response.headersSent&&release.expiresAt*1000>clock()&&
    typeof release.serialized==='string'&&release.responseLength===Buffer.byteLength(release.serialized,'utf8')&&release.responseLength<=262144&&hash(release.serialized)===release.responseSha256);
   response.writeHead(200,{'Content-Type':'application/json','Cache-Control':'no-store','Content-Length':release.responseLength});response.end(release.serialized);
  } catch {failed();}
 });
}

// Trusted route composition only; request parsing and authoritative record owner
// remain required integrations. Successful finish must not abort native end_release.
export async function serveFencedMemory({withMemoryRelease,lifecycle,response,token,binding,policyRevision,prepare,signal,clock=Date.now}) {
 const lease=lifecycle.acquireResponse(response);
 const combined=signal?AbortSignal.any([signal,lease.signal]):lease.signal;
 try {
  return await withMemoryRelease({token,binding,policyRevision,signal:combined},prepare,(release,context)=>{
   lease.assertServing();return handoffMemoryResponse(response,release,{...context,clock});
  });
 } catch(error) {lease.abort();throw error;}
}
