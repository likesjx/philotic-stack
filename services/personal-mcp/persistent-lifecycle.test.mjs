import test from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {EventEmitter} from 'node:events';
import {PersistentLifecycle,servePersistentRecall} from './persistent-lifecycle.mjs';
function fixture(t){const dir=fs.mkdtempSync(path.join(os.tmpdir(),'persistent-lifecycle-'));fs.chmodSync(dir,0o700);t.after(()=>fs.rmSync(dir,{recursive:true,force:true}));return dir;}
class Response extends EventEmitter {
  destroyed=false;writableEnded=false;body=null;
  writeHead(status){this.status=status;}
  end(body){this.body=body;this.writableEnded=true;}
  destroy(){this.destroyed=true;this.emit('close');}
}
const credential={reference:'secret://public-fixture/observe',expiresAt:200};
test('durable owner is exclusive; close/reopen stays withdrawn with monotonic revision',async t=>{
 const dir=fixture(t),owner=new PersistentLifecycle(dir,{clock:()=>100000});
 assert.throws(()=>new PersistentLifecycle(dir),/EEXIST/);
 owner.activate(0,credential);assert.equal(JSON.parse(fs.readFileSync(path.join(dir,'state.json'))).phase,'active');
 assert.throws(()=>owner.activate(0,credential));await owner.close();
 const next=new PersistentLifecycle(dir,{clock:()=>100000});assert.equal(next.read().phase,'withdrawn');assert.equal(next.read().revision,2);await next.close();
});
test('withdraw before recall completes prevents any body and is durable',async t=>{
 const dir=fixture(t),owner=new PersistentLifecycle(dir,{clock:()=>100000});owner.activate(0,credential);
 const response=new Response();let resolve;
 const serving=servePersistentRecall({lifecycle:owner,response,recall:()=>new Promise(r=>{resolve=r;})});
 await owner.withdraw(1);resolve({synthetic:true});await assert.rejects(serving,/withdrawn/);
 assert.equal(response.body,null);assert.equal(response.destroyed,true);assert.equal(owner.read().phase,'withdrawn');await owner.close();
});
test('lease lasts through response completion, and rotation aborts unfinished delivery',async t=>{
 const owner=new PersistentLifecycle(fixture(t),{clock:()=>100000});owner.activate(0,credential);
 const response=new Response();await servePersistentRecall({lifecycle:owner,response,recall:async()=>({synthetic:true})});
 assert.equal(response.destroyed,false);const revision=await owner.withdraw(1,{rotation:true});assert.equal(response.destroyed,true);
 assert.throws(()=>owner.completeRotation(revision,{reference:'secret://public-fixture/new',expiresAt:300},{adoptedReference:'secret://public-fixture/new',oldCredentialDenied:false}));
 owner.completeRotation(revision,{reference:'secret://public-fixture/new',expiresAt:300},{adoptedReference:'secret://public-fixture/new',oldCredentialDenied:true});
 assert.equal(owner.read().phase,'active');await owner.close();
});
test('expired credential cannot serve or silently reactivate',async t=>{
 let now=100000;const owner=new PersistentLifecycle(fixture(t),{clock:()=>now});owner.activate(0,credential);now=200000;
 assert.equal(owner.read().phase,'withdrawn');assert.throws(()=>owner.acquireResponse(new Response()));assert.throws(()=>owner.activate(1,{...credential,expiresAt:300}));await owner.close();
});
test('observed expiry remains withdrawn after wall-clock rollback',async t=>{
 let now=100000;const owner=new PersistentLifecycle(fixture(t),{clock:()=>now});owner.activate(0,credential);
 now=200000;assert.equal(owner.read().phase,'withdrawn');now=199000;
 assert.equal(owner.read().phase,'withdrawn');assert.throws(()=>owner.acquireResponse(new Response()));await owner.close();
});
test('timer expiry persists withdrawal and aborts response',async t=>{
 let now=199990;const dir=fixture(t),owner=new PersistentLifecycle(dir,{clock:()=>now});owner.activate(0,credential);
 const response=new Response();const lease=owner.acquireResponse(response);now=200000;
 await new Promise(resolve=>setTimeout(resolve,30));now=199000;
 assert.equal(lease.signal.aborted,true);assert.equal(response.destroyed,true);
 assert.equal(JSON.parse(fs.readFileSync(path.join(dir,'state.json'))).phase,'withdrawn');assert.equal(owner.read().phase,'withdrawn');await owner.close();
});
test('ordinary disconnect aborts pending recall before releasing lease',async t=>{
 const owner=new PersistentLifecycle(fixture(t),{clock:()=>100000});owner.activate(0,credential);
 const response=new Response();const lease=owner.acquireResponse(response);response.emit('close');
 assert.equal(lease.signal.aborted,true);await owner.withdraw(1);await owner.close();
});
test('stale pending transaction poisons owner and retains recovery interlock',t=>{
 const dir=fixture(t);fs.writeFileSync(path.join(dir,'state.pending'),'public interrupted transaction',{mode:0o600});
 assert.throws(()=>new PersistentLifecycle(dir));assert.equal(fs.existsSync(path.join(dir,'owner.lock')),true);
});
test('active state without lock still refuses restart and preserves interlock',t=>{
 const dir=fixture(t);fs.writeFileSync(path.join(dir,'state.json'),JSON.stringify({schema:1,revision:10,phase:'active'}),{mode:0o600});
 assert.throws(()=>new PersistentLifecycle(dir),/recovery/);assert.equal(fs.existsSync(path.join(dir,'owner.lock')),true);
});
test('response error destroys transport but cannot release lease before close',async t=>{
 const owner=new PersistentLifecycle(fixture(t),{clock:()=>100000});owner.activate(0,credential);
 const response=new Response();response.destroy=()=>{response.destroyed=true;};
 owner.acquireResponse(response);response.emit('error',new Error('public fixture'));
 assert.equal(response.destroyed,true);
 await assert.rejects(owner.withdraw(1,{timeoutMs:10}),/drain incomplete/);
 assert.equal(owner.read().phase,'withdrawn');assert.throws(()=>owner.activate(2,credential),/quiescent/);
 response.emit('close');await owner.close();
});
test('withdrawal cannot reactivate during a pending drain',async t=>{
 const owner=new PersistentLifecycle(fixture(t),{clock:()=>100000});owner.activate(0,credential);
 const response=new Response();response.destroy=()=>{response.destroyed=true;};owner.acquireResponse(response);
 const draining=owner.withdraw(1,{timeoutMs:100});assert.throws(()=>owner.activate(2,credential),/quiescent/);
 response.emit('close');await draining;owner.activate(2,credential);await owner.close();
});
