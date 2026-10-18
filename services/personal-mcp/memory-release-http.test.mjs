// Real HTTP transport on an explicitly enabled disposable Linux runner only.
// Native owner remains a fixture here; this is not composed native qualification.
import test from 'node:test';
import assert from 'node:assert/strict';
import {createServer,get} from 'node:http';
import {once} from 'node:events';
import {createMemoryReleaseAdapter,handoffMemoryResponse} from './memory-release.mjs';
const enabled=process.platform==='linux'&&process.env.RUN_MEMORY_HTTP_TESTS==='1';
const binding={schema:1,issuer:'https://issuer.fixture.test',subject:'public-fixture',clientId:'claude',resource:'https://mcp.fixture.test/mcp',scope:'memory:recall',collection:'synthetic',privacyLabel:'approved_nonprivate',policyRevision:'a'.repeat(64)};
const args={token:'A'.repeat(43),binding,policyRevision:binding.policyRevision};
function adapter(events){return createMemoryReleaseAdapter({
 fence:{pinRelease:async({signal})=>({validate:()=>!signal.aborted,release:async()=>{events.push('end_release');}})},
 inspect:async()=>({active:true,iss:binding.issuer,sub:binding.subject,client_id:binding.clientId,aud:binding.resource,scope:binding.scope,exp:Math.floor(Date.now()/1000)+60,admission_revision:{subject:1,client:1}}),
 currentPolicyRevision:async()=>binding.policyRevision});}
async function listen(t,handler){const server=createServer(handler);server.listen(0,'127.0.0.1');await once(server,'listening');t.after(()=>{server.closeAllConnections();server.close();});return `http://127.0.0.1:${server.address().port}`;}
test('real HTTP preserves exact bytes/id and releases only after local finish',{skip:!enabled},async t=>{
 const events=[],run=adapter(events),bytes=JSON.stringify({jsonrpc:'2.0',id:'public-id',result:{content:[{type:'text',text:'PUBLIC FIXTURE'}]}});
 let complete;const completed=new Promise(r=>{complete=r;});
 const url=await listen(t,(req,res)=>{res.once('finish',()=>events.push('finish'));run(args,async()=>bytes,(release,context)=>handoffMemoryResponse(res,release,context)).then(()=>complete(),e=>complete(e));});
 const response=await fetch(url);assert.equal(await response.text(),bytes);assert.equal(response.headers.get('content-length'),String(Buffer.byteLength(bytes)));
 assert.equal(await completed,undefined);assert.deepEqual(events,['finish','end_release']);
});
test('real client disconnect abandons preparation with no end_release',{skip:!enabled},async t=>{
 const events=[],run=adapter(events);let entered,complete;
 const ready=new Promise(r=>{entered=r;}),completed=new Promise(r=>{complete=r;});
 const url=await listen(t,(req,res)=>{
  const controller=new AbortController();res.once('close',()=>controller.abort());
  run({...args,signal:controller.signal},async(b,{signal})=>{entered();return new Promise((resolve,reject)=>signal.addEventListener('abort',()=>reject(new Error('cancelled')),{once:true}));},(release,context)=>handoffMemoryResponse(res,release,context)).then(()=>complete(false),()=>complete(true));
 });
 const request=get(url);request.on('error',()=>{});await ready;request.destroy();
 assert.equal(await completed,true);assert.deepEqual(events,[]);
});
