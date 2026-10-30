import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'node:net';
import {mkdtemp,chmod,rm,symlink} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {EventEmitter} from 'node:events';
import {createAcceptanceHandoff} from './gateway.mjs';
const env={PHILOTIC_ACCEPTANCE_HANDOFF:'response-boundary-v1'};
async function fixture(action) {
 const dir=await mkdtemp(join(tmpdir(),'handoff-'));await chmod(dir,0o700);
 // macOS tmpdir may be an alias. The production contract requires canonical paths.
 const {realpath}=await import('node:fs/promises');const path=join(await realpath(dir),'gate.sock');
 const peers=new Set();const server=createServer(s=>{peers.add(s);s.on('error',()=>{});s.on('close',()=>peers.delete(s));let text='';s.on('data',b=>{text+=b;if(text.includes('\n'))action(s,JSON.parse(text));});});
 await new Promise((r,j)=>{server.once('error',j);server.listen(path,r);});await chmod(path,0o600);
 const config={mode:'response-boundary-v1',socket:path};
 const gate=await createAcceptanceHandoff(config,env);
 const response=new EventEmitter();response.destroyed=false;response.writableEnded=false;response.destroy=()=>{response.destroyed=true;response.emit('close');};
 return {gate,response,config,path,dir,controller:new AbortController(),async close(){for(const p of peers)p.destroy();await new Promise(r=>server.close(r));await rm(dir,{recursive:true,force:true});}};
}
test('acceptance disabled by default; marker and explicit exact config both required',async()=>{
 assert.equal(await createAcceptanceHandoff(undefined,{}),undefined);
 await assert.rejects(createAcceptanceHandoff(undefined,env));
 await assert.rejects(createAcceptanceHandoff({mode:'response-boundary-v1',socket:'/tmp/missing'},{}));
 const f=await fixture((s,q)=>s.end(JSON.stringify({nonce:q.nonce,resume:true})));
 try{
  await assert.rejects(createAcceptanceHandoff({...f.config,extra:true},env));
  await chmod(f.path,0o666);await assert.rejects(createAcceptanceHandoff(f.config,env));await chmod(f.path,0o600);
  const link=join(f.dir,'alias');await symlink(f.path,link);await assert.rejects(createAcceptanceHandoff({...f.config,socket:link},env));
 }finally{await f.close();}
});
test('exact single-use resume succeeds and sends only a fresh nonce',async()=>{
 const seen=[];const f=await fixture((s,q)=>{assert.deepEqual(Object.keys(q),['nonce']);assert.match(q.nonce,/^[a-f0-9]{64}$/);seen.push(q.nonce);s.end(JSON.stringify({nonce:q.nonce,resume:true}));});
 try{for(let n=0;n<2;n++)await f.gate({signal:f.controller.signal,response:f.response});assert.equal(new Set(seen).size,2);assert.equal(f.response.destroyed,false);}finally{await f.close();}
});
test('wrong nonce, duplicate frames, extra fields and disconnect fail closed',async()=>{
 for(const action of [(s,q)=>s.end(JSON.stringify({nonce:'stale',resume:true})),(s,q)=>s.end(JSON.stringify({nonce:q.nonce,resume:true})+'\n{}'),(s,q)=>s.end(JSON.stringify({nonce:q.nonce,resume:true,extra:1})),s=>s.destroy()]){
  const f=await fixture(action);try{await assert.rejects(f.gate({signal:f.controller.signal,response:f.response}));assert.equal(f.response.destroyed,true);}finally{await f.close();}
 }
});
test('bounded timeout and cancellation destroy the response; late resume cannot succeed',async()=>{
 const f=await fixture(()=>{});try{const start=Date.now();await assert.rejects(f.gate({signal:f.controller.signal,response:f.response}));assert.ok(Date.now()-start<2000);assert.equal(f.response.destroyed,true);}finally{await f.close();}
 const g=await fixture(()=>{});try{const waiting=g.gate({signal:g.controller.signal,response:g.response});g.controller.abort();await assert.rejects(waiting);assert.equal(g.response.destroyed,true);}finally{await g.close();}
});
test('socket permission drift and response disconnection reject before release',async()=>{
 const f=await fixture(()=>{});try{await chmod(f.path,0o666);await assert.rejects(f.gate({signal:f.controller.signal,response:f.response}));}finally{await f.close();}
 const g=await fixture(()=>{});try{const waiting=g.gate({signal:g.controller.signal,response:g.response});g.response.destroy();await assert.rejects(waiting);}finally{await g.close();}
});

test('permission changes during resume reject; finish observation is nonce-bound',async()=>{
 const f=await fixture(async(s,q)=>{await chmod(f.path,0o666);s.end(JSON.stringify({nonce:q.nonce,resume:true}));});
 try{await assert.rejects(f.gate({signal:f.controller.signal,response:f.response}));assert.equal(f.response.destroyed,true);}finally{await f.close();}
 let nonce,resolve;const observed=new Promise(r=>resolve=r);
 const g=await fixture((s,q)=>{if(q.phase==='finish'){resolve(q);s.end();}else{nonce=q.nonce;s.end(JSON.stringify({nonce:q.nonce,resume:true}));}});
 try{await g.gate({signal:g.controller.signal,response:g.response});g.response.emit('finish');const q=await observed;assert.equal(q.nonce,nonce);assert.match(q.finished_ns,/^[0-9]+$/);assert.deepEqual(Object.keys(q).sort(),['finished_ns','nonce','phase']);}finally{await g.close();}
});
