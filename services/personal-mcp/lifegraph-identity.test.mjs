import { test } from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { createLifeGraphIdentityAuthority } from './lifegraph-identity.mjs';
import { createLifeGraphReleaseCoordinator } from './lifegraph-release.mjs';
test('canonical bindings are refreshed, fenced, client-bound and never upgrade memory scope', async () => {
  const now=Math.floor(Date.now()/1000), resource='https://synthetic.example/life/mcp';
  let claims={active:true,iss:'https://issuer.example',aud:resource,sub:'synthetic-subject',client_id:'dot',
    scope:'life:recall',grant_version:'1',iat:now-1,exp:now+300};
  let binding={subject:'synthetic-subject',clientId:'dot',grantVersion:'1',agentId:'synthetic-dot',roles:['reader']};
  const authority=createLifeGraphIdentityAuthority({issuer:{issuer:claims.iss,inspect:async()=>claims},resource,resolveBinding:async()=>binding});
  const req={headers:{authorization:'Bearer synthetic-token'}}, input={subject:claims.sub,clientId:'dot',grantVersion:'1'};
  const first=await authority.resolveCurrent({request:req}); assert.deepEqual(first.roles,['reader']);
  claims={...claims,roles:['admin'],agent_id:'forged-agent'};
  const ignored=await authority.resolveCurrent({request:req});assert.equal(ignored.agentId,'synthetic-dot');assert.deepEqual(ignored.roles,['reader']);
  binding={...binding,roles:[]}; assert.deepEqual((await authority.resolveCurrent({request:req})).roles,[]);
  assert.deepEqual(first.roles,['reader']);
  await assert.rejects(authority.resolve({...input,clientId:'claude'}),/unavailable/);
  await assert.rejects(authority.resolve({...input,grantVersion:'old'}),/unavailable/);
  for(const bad of [{scope:'memory:recall'},{scope:'life:recall memory:recall'},{active:false},{iss:'https://other.example'},
    {aud:'https://other.example/mcp'},{exp:now-1},{grant_version:'old'}]){
    const previous=claims; claims={...claims,...bad}; await assert.rejects(authority.resolveCurrent({request:req}),/unavailable/);claims=previous;
  }
  binding=null; await assert.rejects(authority.resolveCurrent({request:req}),/unavailable/);
  const controller=new AbortController();controller.abort();await assert.rejects(authority.resolve({...input,signal:controller.signal}));
});
test('expiry during awaited final graph validation denies and releases reservations', async () => {
  let time=1000000;const now=1000, resource='https://synthetic.example/life/mcp', request={headers:{authorization:'Bearer synthetic'}};
  const authority=createLifeGraphIdentityAuthority({clock:()=>time,issuer:{issuer:'https://issuer.example',inspect:async()=>({active:true,iss:'https://issuer.example',aud:resource,sub:'subject',client_id:'dot',scope:'life:recall',grant_version:'1',iat:now-1,exp:now+1})},resource,
    resolveBinding:async()=>({subject:'subject',clientId:'dot',grantVersion:'1',agentId:'agent-dot',roles:[]})});
  let releases=0;
  const coordinator=createLifeGraphReleaseCoordinator({issuerAuthority:authority,policyAuthority:{pin:async()=>({release:async()=>{releases++;}})},
    graphAuthority:{readSnapshot:async()=>({}),currentRevision:async()=>{time+=1000;return 'revision';}}});
  const response=new EventEmitter();response.destroyed=false;response.destroy=()=>{response.destroyed=true;response.emit('close');};
  coordinator.bindResponse(request,response);
  const actor={active:true,audience:resource,clientId:'dot',subject:'subject',scope:'life:recall',agentId:'agent-dot',roles:[],grantVersion:'1'};
  assert.equal(await coordinator.admit({request,actor,namespace:'synthetic_dot',responseDigest:'a'.repeat(64)},()=>true),false);
  assert.equal(authority.verifyResponse(request),false);assert.equal(releases,1);response.emit('close');coordinator.close();
});
test('delayed canonical binding and HTTP handoff cannot outlive token expiry', async () => {
  const resource='https://synthetic.example/life/mcp', request={headers:{authorization:'Bearer synthetic'}},now=1000;
  let time=now*1000,delay=false, claims={active:true,iss:'https://issuer.example',aud:resource,sub:'subject',client_id:'dot',scope:'life:recall',grant_version:'1',iat:now-1,exp:now+1};
  const authority=createLifeGraphIdentityAuthority({clock:()=>time,issuer:{issuer:claims.iss,inspect:async()=>claims},resource,resolveBinding:async()=>{
    if(delay)time+=1000;return {subject:'subject',clientId:'dot',grantVersion:'1',agentId:'agent-dot',roles:[]};
  }});
  const response=new EventEmitter();response.destroyed=false;response.destroy=()=>{response.destroyed=true;response.emit('close');};
  authority.bindResponse(request,response);await authority.resolveCurrent({request});assert.equal(authority.verifyResponse(request),true);
  await new Promise(r=>setTimeout(r,1100));assert.equal(response.destroyed,true);assert.equal(authority.verifyResponse(request),false);
  delay=true;
  await assert.rejects(authority.resolveCurrent({request}),/unavailable/);
});
