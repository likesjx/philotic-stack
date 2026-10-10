// Explicit opt-in real Bolt acceptance. Not part of default service unit tests.
// Run only with the pinned disposable container created by the documented recipe.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { createInterface } from 'node:readline';
import { EventEmitter } from 'node:events';
import { DatabaseSync } from 'node:sqlite';
import { Worker } from 'node:worker_threads';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createLifeGraphMemgraphReader, lifeGraphContentDigest, LIFEGRAPH_NODE_QUERY } from './lifegraph-memgraph.mjs';
import { createLifeGraphReleaseCoordinator } from './lifegraph-release.mjs';
import { createLifeGraphStorageAuthority } from './lifegraph-storage.mjs';
import { createLifeGraphGateway } from './lifegraph-gateway.mjs';
const container = 'lifegraph-synthetic-3101';
const inspected = JSON.parse(execFileSync('docker', ['inspect', container], { encoding: 'utf8' }))[0];
assert.equal(inspected.Config.Image, 'memgraph/memgraph@sha256:bd01a159023283b56b807943ed28225c8f662920b64317f26da7d9f0f5b19de4');
assert.equal(inspected.HostConfig.NetworkMode, 'none'); assert.deepEqual(inspected.HostConfig.PortBindings, {});
function bridge() {
  const child = spawn('docker', ['exec', '-i', container, '/tmp/lifegraph-synthetic-bolt-fixture'], { stdio: ['pipe','pipe','pipe'] });
  const pending = []; let error = '';
  const failPending = e => { for (const next of pending.splice(0)) { clearTimeout(next.timer); next.reject(e); } };
  child.stderr.on('data', c => { error = (error + c).slice(-65536); });
  child.on('error', failPending); child.stdin.on('error', failPending);
  createInterface({ input: child.stdout }).on('line', line => {
    const next = pending.shift(); if (!next) return;
    clearTimeout(next.timer);
    try { const v = JSON.parse(line); if (v.error) next.reject(new Error(v.error)); else next.resolve(v.result); }
    catch (e) { next.reject(e); }
  });
  child.on('exit', () => failPending(new Error(`fixture exited ${error}`)));
  const command = v => new Promise((resolve,reject) => {
    const timer = setTimeout(() => { failPending(new Error('fixture deadline')); child.kill(); }, 8000);
    pending.push({ resolve,reject,timer }); child.stdin.write(JSON.stringify(v)+'\n');
  });
  return { command, close: () => { child.stdin.end(); }, transport: {
    async readTransaction({ signal }, work) {
      signal?.throwIfAborted(); await command({ op: 'begin' });
      try {
        const value = await work({ query: async (query, params) => { signal?.throwIfAborted(); return command({ op:'query', query, params }); } });
        signal?.throwIfAborted(); await command({ op: 'commit' }); return value;
      } catch (e) { await command({ op: 'rollback' }); throw e; }
    },
  } };
}
const specs = [
  ['goal:synthetic-root','Goal','Goal','synthetic goal',[]],
  ['goal:synthetic-variant','Goal','Goal','synthetic goal variant',['goal:synthetic-root']],
  ['idea:synthetic','Idea','GrowthHypothesis','synthetic idea',[]],
  ['openloop:synthetic','OpenLoop','OpenLoop','synthetic open loop',[]],
  ['person:synthetic','Person','Person','synthetic person',[]],
  ['place:synthetic','Place','Place','synthetic place',[]],
  ['asset:synthetic','Thing','Asset','synthetic thing',[]],
];
const node = ([id,kind,label,summary,sources]) => ({ id,kind,label,sources,
  contentDigest: lifeGraphContentDigest({ id,labels:[label],summary,sources }),
  ...(id==='goal:synthetic-variant' ? { canonicalId:'goal:synthetic-root', rootBinding:{ type:'verifiedKey',key:'synthetic-key' } } : {}) });
const catalogs = [{ namespace:'synthetic_dot',nodes:specs.map(node),edges:[{ id:'edge:synthetic',from:'goal:synthetic-root',to:'person:synthetic',relation:'RELATED_TO',sources:[] }] },
  { namespace:'synthetic_claude',nodes:[node(['goal:synthetic-other','Goal','Goal','synthetic claude goal',[]])],edges:[] }];
const resource = 'https://mcp.example.test/life/mcp', issuerUrl='https://identity.example.test';
const actor = clientId => ({ active:true,audience:resource,clientId,subject:'synthetic-operator',scope:'life:recall',agentId:`agent-${clientId}`,roles:['reader'],grantVersion:'1' });
test('real Memgraph ontology, HTTP privacy, policy reservation and coordinated revocation', { timeout: 30000 }, async t => {
  const main = bridge(), writer = bridge(); t.after(() => { main.close(); writer.close(); });
  const reader = createLifeGraphMemgraphReader({ transport:main.transport,catalogs });
  const initial = await reader.readSnapshot({ namespace:'synthetic_dot' });
  assert.deepEqual(new Set(initial.nodes.map(n=>n.kind)),new Set(['Goal','Idea','OpenLoop','Person','Place','Thing']));
  assert.equal(initial.nodes.find(n=>n.id==='goal:synthetic-variant').canonicalId,'goal:synthetic-root');
  const aliasCatalogs=structuredClone(catalogs);
  aliasCatalogs[0].nodes.find(n=>n.id==='goal:synthetic-variant').rootBinding={type:'approvedAlias',key:'synthetic-alias'};
  const aliasReader=createLifeGraphMemgraphReader({transport:main.transport,catalogs:aliasCatalogs});
  await aliasReader.readSnapshot({namespace:'synthetic_dot'});
  await writer.command({op:'change',change:'ambiguousAlias'});
  await assert.rejects(aliasReader.readSnapshot({namespace:'synthetic_dot'}),/unavailable/);
  const dir=await mkdtemp(join(tmpdir(),'synthetic-bolt-policy-')), path=join(dir,'privacy.sqlite'), db=new DatabaseSync(path);
  db.exec('PRAGMA user_version=2; CREATE TABLE privacy_revision(singleton INTEGER PRIMARY KEY,revision INTEGER); INSERT INTO privacy_revision VALUES(1,1); CREATE TABLE privacy_policy(resource TEXT PRIMARY KEY,policy_json TEXT NOT NULL)');
  for (const c of catalogs) for (const record of [...c.nodes,...c.edges]) {
    db.prepare('INSERT INTO privacy_policy VALUES(?,?)').run(record.id,JSON.stringify({ owner:'synthetic-operator',creator:'agent-dot',creator_read_grant:true,read_roles:['reader'],private:record.id==='person:synthetic',external_operations:['inference'],sources:record.sources }));
  }
  const revoked=new Set(); let pins=0,releases=0;
  const coordinator=createLifeGraphReleaseCoordinator({ maxLeaseMs:1000,
    issuerAuthority:{ resolveCurrent:async c => {
      const client=c.request.headers.authorization.slice('Bearer '.length); return revoked.has(client)?null:actor(client);
    } },
    policyAuthority:{ pin:async () => {
      const reservation=new DatabaseSync(path,{timeout:50}); reservation.exec('BEGIN IMMEDIATE'); pins++;
      return { release:async () => { reservation.exec('ROLLBACK'); reservation.close(); releases++; } };
    } },
    graphAuthority:{ readSnapshot:c=>reader.readSnapshot(c),currentRevision:async c=>(await reader.readSnapshot(c)).revision } });
  const storage=createLifeGraphStorageAuthority({ policyDatabase:path,graphReader:coordinator,releaseBarrier:coordinator });
  const issuer={ issuer:issuerUrl,ready:async()=>{},inspect:async client=>({ active:!revoked.has(client),iss:issuerUrl,aud:resource,sub:'synthetic-operator',client_id:client,scope:'life:recall',grant_version:'1',iat:Math.floor(Date.now()/1000)-1,exp:Math.floor(Date.now()/1000)+600 }) };
  const config={ resource,profile:'synthetic-read-only-v1',allowedClients:['dot','claude'],allowedSubjects:['synthetic-operator'],clientPolicies:['dot','claude'].map(clientId=>({ clientId,subjects:['synthetic-operator'],namespace:`synthetic_${clientId}`,scopes:['life:recall'] })) };
  const server=await createLifeGraphGateway({ enabled:true,config,issuer,identityAuthority:{ resolve:async c=>({agentId:`agent-${c.clientId}`,roles:['reader']}) },storageAuthorityFactory:async()=>storage });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  t.after(async()=>{ coordinator.close(); await new Promise(resolve=>server.close(resolve)); db.close(); await rm(dir,{recursive:true,force:true}); });
  const rpc=client=>fetch(`http://127.0.0.1:${server.address().port}/life/mcp`,{method:'POST',headers:{'content-type':'application/json',authorization:`Bearer ${client}`},body:JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/call',params:{name:'life.recall',arguments:{query_text:'synthetic',max_context_packets:12}}})});
  const dot=await rpc('dot'); assert.equal(dot.status,200); const dotText=await dot.text();
  assert.doesNotMatch(dotText,/person:synthetic|synthetic person|edge:synthetic|synthetic-other/);
  assert.match(dotText,/idea:synthetic/); assert.equal((dotText.match(/goal:synthetic-root/g)||[]).length,1);
  const claude=await rpc('claude'); assert.equal(claude.status,200); const claudeText=await claude.text();
  assert.match(claudeText,/goal:synthetic-other/); assert.doesNotMatch(claudeText,/idea:synthetic|synthetic-root/);
  assert.equal(pins,releases);

  // A real second SQLite connection cannot change policy while a response's
  // reservation is held. Worker execution avoids blocking this event loop.
  const req={headers:{authorization:'Bearer dot'}}, res=new EventEmitter(); res.destroyed=false;res.writableEnded=false;
  res.destroy=()=>{res.destroyed=true;res.emit('close');}; coordinator.bindResponse(req,res);
  const admission={request:req,actor:actor('dot'),namespace:'synthetic_dot',responseDigest:'a'.repeat(64)};
  assert.equal(await coordinator.admit(admission,({graphRevision})=>graphRevision===initial.revision),true);
  const worker=new Worker(`const {parentPort,workerData}=require('node:worker_threads');const {DatabaseSync}=require('node:sqlite');const db=new DatabaseSync(workerData,{timeout:100});try{db.exec('UPDATE privacy_revision SET revision=revision+1');parentPort.postMessage('unexpected-write');}catch{parentPort.postMessage('locked');}finally{db.close();}`,{eval:true,workerData:path});
  assert.equal(await new Promise((resolve,reject)=>{worker.once('message',resolve);worker.once('error',reject);}), 'locked');
  let revokeDone=false, graphDone=false;
  const revoke=coordinator.participateIssuerChange({clientId:'dot'},async()=>{revoked.add('dot');revokeDone=true;});
  const mutation=coordinator.participateGraphChange({},async()=>{await writer.command({op:'change',change:'summary'});graphDone=true;});
  await new Promise(resolve=>setTimeout(resolve,20)); assert.equal(revokeDone,false);assert.equal(graphDone,false);
  res.writableEnded=true;res.emit('finish');await Promise.all([revoke,mutation]); assert.equal(pins,releases);
  assert.equal((await rpc('dot')).status,401);assert.equal((await rpc('claude')).status,200);
  db.exec('UPDATE privacy_revision SET revision=revision+1');
  await assert.rejects(reader.readSnapshot({namespace:'synthetic_dot'}),/unavailable/);

  // Negative control: MVCC retains an old read while another connection commits.
  await main.command({op:'begin'});
  const params={ids:['goal:synthetic-root']};
  const before=await main.command({op:'query',query:LIFEGRAPH_NODE_QUERY,params});
  await writer.command({op:'change',change:'privateSource'});
  const stale=await main.command({op:'query',query:LIFEGRAPH_NODE_QUERY,params});
  assert.deepEqual(stale,before); await main.command({op:'rollback'});
  await main.command({op:'begin'});const fresh=await main.command({op:'query',query:LIFEGRAPH_NODE_QUERY,params});await main.command({op:'rollback'});
  assert.notDeepEqual(fresh,before);
});
