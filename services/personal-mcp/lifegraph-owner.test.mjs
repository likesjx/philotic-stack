import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:net';
import { createHash } from 'node:crypto';
import { DatabaseSync } from 'node:sqlite';
import { mkdtemp, chmod, writeFile, rm, rename, symlink } from 'node:fs/promises';
import { statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createLifeGraphCanonicalPolicyOwner } from './lifegraph-policy-owner.mjs';
import { loadLifeGraphCatalog } from './lifegraph-catalog.mjs';
import { createLifeGraphOwnerTransport } from './lifegraph-owner-transport.mjs';
import { createLifeGraphStorageAuthority } from './lifegraph-storage.mjs';
import { createLifeGraphOwnedStorage } from './lifegraph-owned-storage.mjs';
import { LIFEGRAPH_NODE_QUERY, lifeGraphContentDigest } from './lifegraph-memgraph.mjs';
async function files(t) {
  const dir = await mkdtemp(join(tmpdir(),'synthetic-owner-')); await chmod(dir,0o700);
  t.after(()=>rm(dir,{recursive:true,force:true})); return dir;
}
async function policy(t) {
  const dir=await files(t), path=join(dir,'privacy.sqlite'), db=new DatabaseSync(path);
  db.exec('PRAGMA user_version=2; CREATE TABLE privacy_revision(singleton INTEGER PRIMARY KEY CHECK(singleton=1),revision INTEGER NOT NULL); INSERT INTO privacy_revision VALUES(1,1); CREATE TABLE privacy_policy(resource TEXT PRIMARY KEY,policy_json TEXT NOT NULL); CREATE TABLE local_authority_receipt(hotel,origin,task,handle,binding,cancelled); CREATE TABLE capture_inbox(producer,event_id,actor,payload,sources_json,policy_revision)');
  await chmod(path,0o600); t.after(()=>db.close()); const s=statSync(path);
  const storeIdentity={device:String(s.dev),inode:String(s.ino)};
  return {dir,path,db,storeIdentity,owner:createLifeGraphCanonicalPolicyOwner({policyDatabase:path,storeIdentity})};
}
test('actual canonical reservation excludes writers without policy/schema mutations', async t=>{
  const f=await policy(t), writer=new DatabaseSync(f.path,{timeout:10}); t.after(()=>writer.close());
  const before=f.db.prepare('SELECT name,sql FROM sqlite_master ORDER BY name').all();
  const lease=await f.owner.pin(); assert.equal(lease.revision,'1'); assert.equal(lease.validate(),true);
  assert.throws(()=>writer.exec('UPDATE privacy_revision SET revision=2'),/locked/);
  assert.throws(()=>f.owner.close(),/unavailable/); assert.equal(lease.validate(),true);
  lease.release(); lease.release(); writer.exec('UPDATE privacy_revision SET revision=2');
  assert.deepEqual(f.db.prepare('SELECT name,sql FROM sqlite_master ORDER BY name').all(),before);
  await assert.rejects(f.owner.pin(),/unavailable/);
});
test('missing, wrong-identity, unsafe permission, wrong version and replaced stores deny', async t=>{
  const f=await policy(t); f.storeIdentity.inode='forged';
  const lease=await f.owner.pin(); lease.release();
  const bad=createLifeGraphCanonicalPolicyOwner({policyDatabase:f.path,storeIdentity:{device:'0',inode:'0'}});
  await assert.rejects(bad.pin(),/unavailable/);
  await chmod(f.path,0o644); await assert.rejects(f.owner.pin(),/unavailable/); await chmod(f.path,0o600);
  f.db.exec('PRAGMA user_version=1'); await assert.rejects(f.owner.pin(),/unavailable/);f.db.exec('PRAGMA user_version=2');
  const pinned=await f.owner.pin(); await rename(f.path,f.path+'.removed');
  assert.equal(pinned.validate(),false);pinned.release(); await assert.rejects(f.owner.pin(),/unavailable/);
  assert.throws(()=>statSync(f.path),/ENOENT/);
});
test('catalog is bounded, digest-pinned, privately owned and rejects symlinks', async t=>{
  const dir=await files(t), path=join(dir,'catalog.json'), bytes=JSON.stringify({version:1,catalogs:[{namespace:'synthetic_dot',nodes:[],edges:[]}]});
  await writeFile(path,bytes,{mode:0o600});const expectedDigest=createHash('sha256').update(bytes).digest('hex');
  assert.equal(loadLifeGraphCatalog({catalogFile:path,expectedDigest})[0].namespace,'synthetic_dot');
  assert.throws(()=>loadLifeGraphCatalog({catalogFile:path,expectedDigest:'0'.repeat(64)}),/unavailable/);
  await chmod(path,0o644);assert.throws(()=>loadLifeGraphCatalog({catalogFile:path,expectedDigest}),/unavailable/);await chmod(path,0o600);
  await symlink(path,join(dir,'link'));assert.throws(()=>loadLifeGraphCatalog({catalogFile:join(dir,'link'),expectedDigest}),/unavailable/);
});
async function socket(t, reply) {
  const dir=await files(t), path=join(dir,'owner.sock'), calls=[];
  const server=createServer(c=>{
    let bytes=Buffer.alloc(0);
    c.on('data',chunk=>{bytes=Buffer.concat([bytes,chunk]);while(bytes.length>=4&&bytes.length>=bytes.readUInt32BE()+4){
      const size=bytes.readUInt32BE(),message=JSON.parse(bytes.subarray(4,4+size));bytes=bytes.subarray(4+size);calls.push(message);
      const frame=reply(message);if(frame===null)continue;
      if(Buffer.isBuffer(frame)){c.write(frame);continue;}
      const body=Buffer.from(JSON.stringify({owner:'synthetic-owner',session:message.session,sequence:message.sequence,ok:true,result:frame}));
      const header=Buffer.alloc(4);header.writeUInt32BE(body.length);c.write(header.subarray(0,2));c.write(Buffer.concat([header.subarray(2),body]));
    }});
  });
  await new Promise(resolve=>server.listen(path,resolve));await chmod(path,0o600);
  t.after(()=>new Promise(resolve=>server.close(resolve)));
  return {path,calls,transport:createLifeGraphOwnerTransport({socketPath:path,ownerIdentity:'synthetic-owner',requestDeadlineMs:100})};
}
test('real private Unix framing restricts queries, correlates replies and commits snapshot', async t=>{
  const f=await socket(t,m=>m.operation==='nodes'?[{id:'goal:synthetic'}]:true);
  const rows=await f.transport.readTransaction({},tx=>tx.query(LIFEGRAPH_NODE_QUERY,{ids:['goal:synthetic']}));
  assert.deepEqual(rows,[{id:'goal:synthetic'}]);assert.deepEqual(f.calls.map(c=>c.operation),['begin_snapshot','nodes','finish_snapshot']);
  await assert.rejects(f.transport.readTransaction({},tx=>tx.query('DELETE n',{})),/unavailable/);
  assert.ok(f.calls.every(c=>!('credential'in c)&&!('role'in c)&&!('query'in c)));
});
test('oversized frame headers and deadline fail closed before allocating a payload', async t=>{
  const header=Buffer.alloc(4);header.writeUInt32BE(1048577);
  const f=await socket(t,()=>header);await assert.rejects(f.transport.readTransaction({},async()=>{}),/unavailable/);
  const g=await socket(t,()=>null);await assert.rejects(g.transport.readTransaction({},async()=>{}),/unavailable/);
});
test('concrete owned storage loads catalog and canonical policy over actual Unix transport', async t=>{
  assert.equal(createLifeGraphOwnedStorage({}),null);
  const f=await policy(t), row={id:'goal:synthetic',labels:['Goal'],summary:'synthetic goal',sources:[]};
  const s=await socket(t,m=>m.operation==='nodes'?[{...row,sources:'[]'}]:true);
  const catalogFile=join(f.dir,'catalog.json'), catalog={version:1,catalogs:[{namespace:'synthetic_dot',nodes:[{id:row.id,kind:'Goal',label:'Goal',sources:[],contentDigest:lifeGraphContentDigest(row)}],edges:[]}]};
  const bytes=JSON.stringify(catalog);await writeFile(catalogFile,bytes,{mode:0o600});
  f.db.prepare('INSERT INTO privacy_policy VALUES(?,?)').run(row.id,JSON.stringify({owner:'operator',creator:'agent-dot',creator_read_grant:true,read_roles:[],private:false,external_operations:['inference'],sources:[]}));
  const args={enabled:true,profile:'synthetic-read-only-v1',policyDatabase:f.path,storeIdentity:{device:String(statSync(f.path).dev),inode:String(statSync(f.path).ino)},catalogFile,catalogDigest:createHash('sha256').update(bytes).digest('hex'),ownerSocket:s.path,ownerIdentity:'synthetic-owner',issuerAuthority:{resolveCurrent:async()=>null}};
  const storage=createLifeGraphOwnedStorage(args);t.after(()=>storage.close());
  const snapshot=await storage.snapshot({namespace:'synthetic_dot'});assert.equal(snapshot.nodes[0].policy.creator,'agent-dot');
  assert.throws(()=>createLifeGraphOwnedStorage({...args,profile:'production'}),/unavailable/);
});

test('readonly policy authority cannot split from its approved reservation inode', async t=>{
  const f=await policy(t), other=await policy(t);
  const graphReader={readSnapshot:async()=>({namespace:'synthetic_dot',revision:'r1',nodes:[],edges:[]})};
  const releaseBarrier={admit:async()=>true};
  assert.throws(()=>createLifeGraphStorageAuthority({policyDatabase:other.path,storeIdentity:f.storeIdentity,graphReader,releaseBarrier}),/unavailable/);
  const storage=createLifeGraphStorageAuthority({policyDatabase:f.path,storeIdentity:f.storeIdentity,graphReader,releaseBarrier});t.after(()=>storage.close());
  await storage.snapshot({namespace:'synthetic_dot'});
  await rename(f.path,f.path+'.original');await rename(other.path,f.path);
  await assert.rejects(storage.snapshot({namespace:'synthetic_dot'}),/unavailable/);
  assert.equal(storage.verifyResponse({},{}),false);
});
test('daemon release lease retains one correlated channel through nested read and handoff', async t=>{
  const f=await socket(t,m=>m.operation==='nodes'?[{id:'goal:synthetic'}]:true);
  const lease=await f.transport.pinRelease({});assert.equal(lease.validate(),true);
  const rows=await lease.readTransaction({},tx=>tx.query(LIFEGRAPH_NODE_QUERY,{ids:['goal:synthetic']}));
  assert.deepEqual(rows,[{id:'goal:synthetic'}]);
  assert.deepEqual(f.calls.map(c=>c.operation),['begin_release','start_read','nodes','finish_read']);
  assert.equal(lease.validate(),true);await lease.release();assert.equal(lease.validate(),false);await lease.release();
  assert.equal(f.calls.at(-1).operation,'end_release');assert.equal(new Set(f.calls.map(c=>c.session)).size,1);
});
