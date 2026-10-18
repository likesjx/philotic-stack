import test from 'node:test';
import assert from 'node:assert/strict';
import {createPersistentOwnerBoundary} from './persistent-owner-boundary.mjs';
function fixture() {
 const grants={dot:{subject:'fixture',clientId:'dot',clientVersion:1,enabled:true,expiresAt:200},claude:{subject:'fixture',clientId:'claude',clientVersion:4,enabled:true,expiresAt:200}};
 const calls=[];let phase='active';let hook=()=>{};
 const options={resource:'https://mcp.fixture.test/mcp',clients:['dot','claude'],clock:()=>100000,
  issuer:{authorizeRecall:async({accessToken})=>structuredClone(grants[accessToken])},
  scopeOwner:{resolve:async()=>({vaults:['approved_fixture'],expiresAt:150})},
  lifecycleOwner:{read:async()=>({phase,credentialExpiresAt:180})},
  privacyAuthority:{filterAndBind:async raw=>raw,verify:async()=>true},
  upstream:{recall:async value=>{calls.push(value);hook();return {records:[{id:'synthetic'}]};}}};
 return {grants,calls,options,service:createPersistentOwnerBoundary(options),withdraw:()=>{phase='withdrawn';},hook:fn=>{hook=fn;}};
}
const request=client=>({accessToken:client,vault:'approved_fixture',arguments:{context:['synthetic']}});
test('Claude-only configuration does not authorize dot',async()=>{
 const f=fixture();const service=createPersistentOwnerBoundary({...f.options,clients:['claude']});
 await service.recall(request('claude'));await assert.rejects(service.recall(request('dot')),/authorization denied/);
 assert.equal(f.calls.length,1);
});
test('already cancelled request never calls issuer or upstream',async()=>{
 const f=fixture(),controller=new AbortController();let calls=0;
 f.options.issuer.authorizeRecall=async()=>{calls++;throw new Error('unexpected');};controller.abort();
 await assert.rejects(f.service.recall({...request('claude'),signal:controller.signal}),{name:'AbortError'});
 assert.equal(calls,0);assert.equal(f.calls.length,0);
});
test('disconnect cancellation reaches upstream and prevents privacy work',async()=>{
 const f=fixture(),controller=new AbortController();let entered;const ready=new Promise(r=>{entered=r;});let privacyCalls=0;
 f.options.privacyAuthority.filterAndBind=async()=>{privacyCalls++;};
 f.options.upstream.recall=({signal})=>new Promise((resolve,reject)=>{
  assert.equal(signal,controller.signal);signal.addEventListener('abort',()=>reject(signal.reason),{once:true});entered();
 });
 const pending=f.service.recall({...request('claude'),signal:controller.signal});await ready;controller.abort();
 await assert.rejects(pending,{name:'AbortError'});assert.equal(privacyCalls,0);
});
test('cancellation while privacy owner awaits prevents verification and release',async()=>{
 const f=fixture(),controller=new AbortController();let verify=0;
 f.options.privacyAuthority.filterAndBind=async(raw,bound,{signal})=>{assert.equal(signal,controller.signal);controller.abort();return raw;};
 f.options.privacyAuthority.verify=async()=>{verify++;return true;};
 await assert.rejects(f.service.recall({...request('claude'),signal:controller.signal}),{name:'AbortError'});assert.equal(verify,0);
});
test('uses canonical per-client admission and never sends OAuth token upstream',async()=>{
 const f=fixture();await f.service.recall(request('dot'));f.grants.dot.enabled=false;f.grants.dot.clientVersion++;
 await assert.rejects(f.service.recall(request('dot')));await f.service.recall(request('claude'));
 assert.equal(f.calls.length,2);assert.deepEqual(Object.keys(f.calls[0]).sort(),['arguments','readOnly','vault']);
});
test('canonical owner revocation during upstream withholds output',async()=>{
 const f=fixture();f.hook(()=>{f.grants.dot.clientVersion++;});
 await assert.rejects(f.service.recall(request('dot')),/authorization changed/);
});
test('unapproved vault and withdrawn lifecycle cannot reach upstream',async()=>{
 const f=fixture();await assert.rejects(f.service.recall({...request('dot'),vault:'default'}));
 f.withdraw();await assert.rejects(f.service.recall(request('claude')));assert.equal(f.calls.length,0);
});
test('missing owners refuse construction; unknown client cannot reach scope/upstream',async()=>{
 const f=fixture();assert.throws(()=>createPersistentOwnerBoundary({...f.options,privacyAuthority:null}));
 f.grants.dot.clientId='unknown';await assert.rejects(f.service.recall(request('dot')));assert.equal(f.calls.length,0);
});
test('reserved routing and write arguments are rejected before upstream',async()=>{
 for(const extra of [{vault:'unapproved'},{readOnly:false},{read_only:false},{tool:'remember'},{limit:21},{threshold:Infinity}]) {
  const f=fixture();await assert.rejects(f.service.recall({...request('dot'),arguments:{context:['synthetic'],...extra}}));
  assert.equal(f.calls.length,0);
 }
});
test('revocation during final scope or lifecycle read withholds response',async()=>{
 for(const owner of ['scopeOwner','lifecycleOwner']) {
  const f=fixture();const method=owner==='scopeOwner'?'resolve':'read';const original=f.options[owner][method];let reads=0;
  f.options[owner][method]=async (...args)=>{if(++reads===2){f.grants.dot.enabled=false;f.grants.dot.clientVersion++;}return original(...args);};
  await assert.rejects(f.service.recall(request('dot')),/authorization changed/);
 }
});
