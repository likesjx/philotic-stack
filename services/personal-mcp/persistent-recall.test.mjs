import test from 'node:test';
import assert from 'node:assert/strict';
import {preparePersistentRecall, revokeClient, beginRotation, finishRotation, canRecall, withdraw, guardedRecall} from './persistent-recall.mjs';
const state = () => ({phase:'active',subject:'fixture-user',subjectEnabled:true,resource:'https://mcp.fixture.test/mcp',vaults:['approved_fixture'],
  clients:{dot:{enabled:true,version:2},claude:{enabled:true,version:5}},families:[{clientId:'dot',revoked:false},{clientId:'claude',revoked:false}],
  activeCredential:{reference:'secret://fixture/old',expiresAt:200},privacyLeaseExpiresAt:150});
const request = (id='dot') => ({clientId:id,clientVersion:id==='dot'?2:5,subject:'fixture-user',resource:'https://mcp.fixture.test/mcp',scope:'memory:recall',tool:'muninn_recall',readOnly:true,vault:'approved_fixture'});
const manifest = () => ({version:1,mode:'persistent',subject:'fixture-user',resource:'https://mcp.fixture.test/mcp',clients:[{kind:'dot'},{kind:'claude'}]});
test('Claude-first preparation stays blocked without scope and access evidence',()=>{
 const result=preparePersistentRecall({...manifest(),clients:[{kind:'claude'}]});
 assert.equal(result.activationAllowed,false);assert.ok(result.blockers.includes('approved-real-memory-scope'));
 assert.ok(!result.blockers.some(x=>x.startsWith('dot:')));
 for(const clients of [[],[{kind:'claude'},{kind:'claude'}],[{kind:'unknown'}]])assert.throws(()=>preparePersistentRecall({...manifest(),clients}));
});
test('unknown callbacks, scope and evidence remain blocked, even with approval-looking extras',()=>{
 const result=preparePersistentRecall({...manifest(),approved:true});
 assert.equal(result.activationAllowed,false); assert.equal(result.preparationComplete,false);
 assert.ok(result.blockers.includes('dot:callbacks')); assert.ok(result.blockers.includes('approved-real-memory-scope'));
});
test('cross-client IDs and callbacks are rejected',()=>{
 for(const field of ['clientId','callbacks']) {
  const m=manifest(); for(const [i,c] of m.clients.entries()){c.clientId=`client${i}`;c.callbacks=[`https://client${i}.fixture.test/callback`];}
  m.clients[1][field]=m.clients[0][field]; assert.throws(()=>preparePersistentRecall(m));
 }
});
test('revocation preserves other client and global admission',()=>{
 const original=state(), next=revokeClient(original,'dot');
 assert.equal(canRecall(next,request(),100),false);assert.equal(canRecall(next,request('claude'),100),true);
 assert.deepEqual(next.clients.claude,original.clients.claude);assert.equal(next.subjectEnabled,true);
 assert.deepEqual(next.families,[{clientId:'dot',revoked:true},{clientId:'claude',revoked:false}]);assert.equal(original.clients.dot.enabled,true);
});
test('scope, audience, credential and privacy lease checks fail closed',()=>{
 for(const patch of [{vault:'default'},{subject:'other'},{resource:'https://wrong.test'},{scope:'memory:write'},{readOnly:false},{clientVersion:1},{tool:'remember'}]) assert.equal(canRecall(state(),{...request(),...patch},100),false);
 assert.equal(canRecall(state(),request(),150),false);assert.equal(canRecall({...state(),activeCredential:{expiresAt:99}},request(),100),false);
 assert.equal(canRecall(withdraw(state()),request(),100),false);
});
test('rotation withdraws serving until adoption and old-key rejection both pass',()=>{
 const next=beginRotation(state(),{reference:'secret://fixture/new',expiresAt:300},100);
 assert.equal(canRecall(next,request(),100),false);
 assert.throws(()=>finishRotation(next,{reference:next.replacement.reference,adopted:true,positiveRecall:true},100));
 const done=finishRotation(next,{reference:next.replacement.reference,adopted:true,positiveRecall:true,oldCredentialDenied:true},100);
 assert.equal(canRecall(done,request(),100),true); assert.equal(done.activeCredential.reference,'secret://fixture/new');
 assert.throws(()=>finishRotation(next,{reference:next.replacement.reference,adopted:true,positiveRecall:true,oldCredentialDenied:true},300));
});
test('revocation during upstream recall prevents result release',async()=>{
 let current=state(); const authority={filterAndBind:async raw=>raw,verify:async()=>true};
 await assert.rejects(guardedRecall({state:async()=>current,request:request(),now:()=>100,
  fetchRecall:async()=>{current=revokeClient(current,'dot');return {synthetic:true};},privacyAuthority:authority}),/revoked/);
});
test('missing or rejecting privacy authority never releases upstream data',async()=>{
 const base={state:async()=>state(),request:request(),now:()=>100,fetchRecall:async()=>({synthetic:true})};
 await assert.rejects(guardedRecall(base),/absent/);
 await assert.rejects(guardedRecall({...base,privacyAuthority:{filterAndBind:async raw=>raw,verify:async()=>false}}),/invalid/);
});
test('caller cannot swap revoked dot request to Claude during an await',async()=>{
 let current=state(); const r=request();
 await assert.rejects(guardedRecall({state:async()=>current,request:r,now:()=>100,
  fetchRecall:async()=>{current=revokeClient(current,'dot');Object.assign(r,request('claude'));return {records:[]};},
  privacyAuthority:{filterAndBind:async raw=>raw,verify:async()=>true}}),/revoked/);
});
test('verified payload cannot gain unverified records during final state read',async()=>{
 const output={records:[{id:'approved'}]};let reads=0,verified;
 const result=await guardedRecall({state:async()=>{if(++reads===2)output.records.push({id:'excluded'});return state();},
  request:request(),now:()=>100,fetchRecall:async()=>output,
  privacyAuthority:{filterAndBind:async raw=>raw,verify:async bounded=>{verified=bounded;return true;}}});
 assert.deepEqual(result,{records:[{id:'approved'}]});assert.equal(result,verified);
 assert.throws(()=>result.records.push({id:'excluded'}));
});
