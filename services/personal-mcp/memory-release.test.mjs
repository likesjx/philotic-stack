import test from 'node:test';
import assert from 'node:assert/strict';
import {EventEmitter} from 'node:events';
import {createMemoryReleaseAdapter,handoffMemoryResponse,serveFencedMemory} from './memory-release.mjs';
import {PersistentLifecycle} from './persistent-lifecycle.mjs';
import * as fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
const binding={schema:1,issuer:'https://issuer.fixture.test',subject:'fixture',clientId:'claude',resource:'https://mcp.fixture.test/mcp',scope:'memory:recall',collection:'approved_fixture',privacyLabel:'approved_nonprivate',policyRevision:'a'.repeat(64)};
const args=()=>({token:'A'.repeat(43),binding,policyRevision:binding.policyRevision});
const bytes=JSON.stringify({jsonrpc:'2.0',id:17,result:{content:[{type:'text',text:'PUBLIC FIXTURE'}]}});
class Response extends EventEmitter {
 destroyed=false;writableEnded=false;headersSent=false;
 writeHead(status,headers){this.status=status;this.headers=headers;this.headersSent=true;}
 end(body){this.body=body;this.writableEnded=true;}
 destroy(){this.destroyed=true;this.emit('close');}
}
function fixture(options={}) {
 const events=[];let active=true,revision=1,ownerSignal;
 const adapter=createMemoryReleaseAdapter({clock:()=>100000,...options,
  fence:{pinRelease:async({signal})=>{ownerSignal=signal;events.push('begin');return {validate:()=>!signal.aborted,release:async()=>{events.push('end');if(options.badAck)throw new Error('unknown ack');}};}},
  currentPolicyRevision:async()=>binding.policyRevision,
  inspect:async()=>{events.push('inspect');return {active,iss:binding.issuer,sub:binding.subject,client_id:binding.clientId,aud:binding.resource,scope:binding.scope,exp:200,admission_revision:{subject:1,client:revision}};}});
 return {adapter,events,signal:()=>ownerSignal,revoke:()=>{active=false;},changeRevision:()=>{revision++;}};
}
test('exact bytes and ID stay leased until actual writer finish',async()=>{
 const f=fixture(),response=new Response();let started;const writing=new Promise(r=>{started=r;});
 const pending=f.adapter(args(),async()=>bytes,(release,{signal})=>{assert.ok(Object.isFrozen(release));const p=handoffMemoryResponse(response,release,{signal,clock:()=>100000});started();return p;});
 await writing;assert.equal(response.body,bytes);assert.equal(JSON.parse(response.body).id,17);assert.ok(!f.events.includes('end'));
 response.emit('finish');await pending;assert.equal(f.events.at(-1),'end');assert.equal(f.signal().aborted,false);
});
test('revocation before final authorization prevents prepare and handoff',async()=>{
 const f=fixture();f.revoke();let prepared=false;
 await assert.rejects(f.adapter(args(),async()=>{prepared=true;return bytes;},async()=>assert.fail()));
 assert.equal(prepared,false);assert.equal(f.signal().aborted,true);assert.ok(!f.events.includes('end'));
});
test('revision change during prepare abandons without writing',async()=>{
 const f=fixture();await assert.rejects(f.adapter(args(),async()=>{f.changeRevision();return bytes;},async()=>assert.fail()));
 assert.equal(f.signal().aborted,true);assert.ok(!f.events.includes('end'));
});
test('missing/crossed record provenance refuses before any output derivation',async()=>{
 for(const record of [{},{owner:'other',collection:binding.collection,privacy:'approved_nonprivate',id:'fixture',revision:'1'},
  {owner:binding.subject,collection:binding.collection,privacy:'private',id:'fixture',revision:'1'}]) {
  const f=fixture();let derived=false;
  await assert.rejects(f.adapter(args(),async b=>{
   assert.ok(record.owner===b.subject&&record.collection===b.collection&&record.privacy===b.privacyLabel&&record.id&&record.revision);
   derived=true;return bytes;
  },async()=>assert.fail()));assert.equal(derived,false);assert.ok(!f.events.includes('end'));
 }
});
test('early close destroys response and abandons native lease',async()=>{
 const f=fixture(),response=new Response();let started;const writing=new Promise(r=>{started=r;});
 const pending=f.adapter(args(),async()=>bytes,(r,{signal})=>{const p=handoffMemoryResponse(response,r,{signal,clock:()=>100000});started();return p;});
 await writing;response.emit('close');await assert.rejects(pending);assert.equal(response.destroyed,true);assert.equal(f.signal().aborted,true);assert.ok(!f.events.includes('end'));
});
test('uncertain end acknowledgment remains failure after local finish',async()=>{
 const f=fixture({badAck:true});await assert.rejects(f.adapter(args(),async()=>bytes,async()=>{}),/unknown ack/);assert.equal(f.signal().aborted,true);
});
test('timeout abandons even an uncooperative prepare callback',async()=>{
 const f=fixture({timeoutMs:10});const keepAlive=setTimeout(()=>{},100);
 try{await assert.rejects(f.adapter(args(),()=>new Promise(()=>{}),async()=>assert.fail()),/abandoned/);assert.equal(f.signal().aborted,true);assert.ok(!f.events.includes('end'));}finally{clearTimeout(keepAlive);}
});
test('durable lifecycle and shared fence both complete on transport finish',async t=>{
 const dir=fs.mkdtempSync(path.join(os.tmpdir(),'memory-fence-'));fs.chmodSync(dir,0o700);t.after(()=>fs.rmSync(dir,{recursive:true,force:true}));
 const lifecycle=new PersistentLifecycle(dir,{clock:()=>100000});lifecycle.activate(0,{reference:'secret://fixture/observe',expiresAt:200});
 const f=fixture(),response=new Response();const originalEnd=response.end.bind(response);
 response.end=body=>{originalEnd(body);queueMicrotask(()=>response.emit('finish'));};
 await serveFencedMemory({withMemoryRelease:f.adapter,lifecycle,response,...args(),prepare:async()=>bytes,clock:()=>100000});
 assert.equal(f.events.at(-1),'end');assert.equal(f.signal().aborted,false);assert.equal(response.destroyed,false);await lifecycle.close();
});
