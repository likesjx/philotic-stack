// TEST ONLY: actual issuer-issued LifeGraph and memory tokens over disposable
// stores. Never imported by a production entrypoint; no claims are relabeled.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { existsSync, readFileSync, writeFileSync, renameSync, statSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { MongoClient } from 'mongodb';
import { mongoStore, ensureIndexes, changePersonalLifeGraphAdmission, changePersonalAdmission, revokeProviderSessions } from '__ISSUER__/src/lib/auth/personal-oauth/mongo';
import { PersonalOAuth, challenge, hash } from '__ISSUER__/src/lib/auth/personal-oauth/policy';
import { handlers } from '__ISSUER__/src/lib/auth/personal-oauth/handlers';
import { withLifeGraphIssuerChange } from '__ISSUER__/src/lib/auth/personal-oauth/lifegraph-coordination';
import { createLifeGraphGateway } from '../lifegraph-gateway.mjs';
import { createApprovedLifeGraphGateway } from '../lifegraph-production.mjs';
import { issuerAdapter } from '../gateway.mjs';
import { createLifeGraphOwnedStorage } from '../lifegraph-owned-storage.mjs';
import { createLifeGraphIdentityAuthority } from '../lifegraph-identity.mjs';
import { lifeGraphContentDigest } from '../lifegraph-memgraph.mjs';
const dir=process.env.SYNTHETIC_DIR!, role=process.env.SYNTHETIC_ROLE!;
process.on('uncaughtException',error=>{writeFileSync(`${dir}/fixture-error-${role}`,String(error.stack),{mode:0o600});process.exit(1);});
process.on('unhandledRejection',error=>{writeFileSync(`${dir}/fixture-error-${role}`,String(error),{mode:0o600});process.exit(1);});
let markerSequence=0;
const put=(name:string,value:any='synthetic')=>{
 const staged=`${dir}/.fixture-${process.pid}-${++markerSequence}`;
 writeFileSync(staged,typeof value==='string'?value:JSON.stringify(value),{mode:0o600});
 renameSync(staged,`${dir}/${name}`);
};
const wait=async(name:string)=>{for(let i=0;i<1000;i++){if(existsSync(`${dir}/${name}`))return;await new Promise(r=>setTimeout(r,5));}throw Error('synthetic deadline');};
const resource='https://synthetic.example/life/mcp', issuerUrl='https://synthetic-issuer.example';
if(role==='issuer'){
 const client=await new MongoClient(process.env.SYNTHETIC_MONGO_URI!).connect(),db=client.db(process.env.SYNTHETIC_DATABASE!);
 await ensureIndexes(db);const store=mongoStore(db),identity={provider:'google',providerId:'synthetic-composed'};
 await db.collection('users').insertOne({...identity,roles:['admin']});const subject=await store.resolve(identity);
 await db.collection('personal_oauth_admissions').insertOne({subject,enabled:true,version:0});
 await db.collection('personal_oauth_client_admissions').insertMany(['dot','claude'].map(clientId=>({subject,clientId,enabled:true,version:0})));
 await db.collection('personal_oauth_resource_admissions').insertMany(['dot','claude'].map(clientId=>({subject,clientId,resource,scope:'life:recall',enabled:true,version:0,binding:{agentId:'agent-'+clientId,roles:['reader']}})));
 const config={issuer:issuerUrl,resource:'https://synthetic.example/memory/mcp',lifeGraphResource:resource,clients:['dot','claude'].map(clientId=>({clientId,callbacks:[`https://${clientId}.example/callback`]}))};
 const auth=new PersonalOAuth(config,store),session='synthetic-session',credentials={client:'synthetic-gateway',verifier:hash('Basic '+Buffer.from('synthetic-gateway:'+'s'.repeat(43)).toString('base64'))};
 const h=handlers(auth,async()=>({identity,session}),credentials),tokens=new Map<string,any>();
 async function mintToken(clientId:string,life:boolean){
  const audience=life?resource:config.resource;
  const verifier='v'.repeat(64),q=new URLSearchParams({client_id:clientId,redirect_uri:`https://${clientId}.example/callback`,resource:audience,response_type:'code',scope:life?'life:recall':'memory:recall',state:'synthetic',code_challenge_method:'S256',code_challenge:challenge(verifier)});
  const begin=await h.authorize(new Request(issuerUrl+'/oauth/authorize?'+q));assert.equal(begin.status,200);
  const html=await begin.text(),transaction=html.match(/name="transaction" value="([^"]+)"/)![1],csrf=html.match(/name="csrf" value="([^"]+)"/)![1];
  const post=(path:string,p:URLSearchParams)=>new Request(issuerUrl+path,{method:'POST',headers:{'Content-Type':'application/x-www-form-urlencoded',Origin:issuerUrl},body:p});
  const consent=await h.consent(post('/oauth/consent',new URLSearchParams({transaction,csrf,decision:'approve'})));assert.equal(consent.status,303);
  const code=new URL(consent.headers.get('location')!).searchParams.get('code')!;
  const response=await h.token(post('/oauth/token',new URLSearchParams({grant_type:'authorization_code',client_id:clientId,redirect_uri:`https://${clientId}.example/callback`,resource:audience,code,code_verifier:verifier})));assert.equal(response.status,200);
  return response.json();
 }
 async function mint(clientId:string){
  const life=await mintToken(clientId,true),memory=await mintToken(clientId,false);tokens.set(clientId,life);
  return {cap:life.access_token,memory:memory.access_token};
 }
 const minted=Object.fromEntries(await Promise.all(['dot','claude'].map(async c=>[c,await mint(c)])));
 async function inspect(presented:string){
  const response=await h.introspect(new Request(issuerUrl+'/oauth/introspect',{method:'POST',headers:{'Content-Type':'application/x-www-form-urlencoded',Authorization:'Basic '+Buffer.from('synthetic-gateway:'+'s'.repeat(43)).toString('base64')},body:new URLSearchParams({token:presented})}));
  return response.json();
 }
 const server=createServer(async(req,res)=>{try{
  const url=new URL(req.url!,'http://127.0.0.1');let result:any;
  if(['/oauth/introspect','/oauth/lifegraph-binding','/.well-known/oauth-authorization-server'].includes(url.pathname)){
   const chunks:Buffer[]=[];for await(const chunk of req)chunks.push(Buffer.from(chunk));
   const request=new Request(issuerUrl+url.pathname,{method:req.method,headers:{
    'Content-Type':String(req.headers['content-type']??''),Authorization:String(req.headers.authorization??'')},
    ...(req.method==='POST'?{body:Buffer.concat(chunks).toString('utf8')}:{})});
   const response=url.pathname==='/oauth/introspect'?await h.introspect(request):url.pathname==='/oauth/lifegraph-binding'?await h.lifeGraphBinding(request):await h.discovery();
   res.writeHead(response.status,Object.fromEntries(response.headers));res.end(await response.text());return;
  }
  if(url.pathname==='/inspect')result=await inspect(url.searchParams.get('token')!);
  else if(url.pathname==='/binding'){
   const response=await h.lifeGraphBinding(new Request(issuerUrl+'/oauth/lifegraph-binding',{method:'POST',
    headers:{'Content-Type':'application/x-www-form-urlencoded',Authorization:'Basic '+Buffer.from('synthetic-gateway:'+'s'.repeat(43)).toString('base64')},
    body:new URLSearchParams({subject:url.searchParams.get('subject')!,client_id:url.searchParams.get('clientId')!,resource,grant_version:url.searchParams.get('grantVersion')!})}));
   if(response.status!==200)throw Error('binding denied');result=await response.json();
  }else if(url.pathname==='/control'){
   const operation=url.searchParams.get('op')!,clientId=url.searchParams.get('client')??'dot';
   if(operation==='revoke-client'||operation==='hold-revoke'){
    if(operation==='hold-revoke')put('hold-mongo');
    await changePersonalLifeGraphAdmission(db,subject,clientId,resource,false);
   }else if(operation==='reenable')await changePersonalLifeGraphAdmission(db,subject,clientId,resource,true);
   else if(operation==='mint')result=await mint(clientId);
   else if(operation==='global')await changePersonalAdmission(db,subject,false);
   else if(operation==='logout')await revokeProviderSessions(db,identity.provider,identity.providerId);
   else if(operation==='token-revoke')await auth.revoke(tokens.get(clientId).access_token,clientId);
   else if(operation==='role-revoke')await changePersonalLifeGraphAdmission(db,subject,clientId,resource,true,{agentId:'agent-'+clientId,roles:[]});
   else if(operation==='unknown')await withLifeGraphIssuerChange(async()=>{await db.collection('personal_oauth_resource_admissions').updateOne({subject,clientId,resource,scope:'life:recall'},{$inc:{version:1}});throw Error('synthetic unknown commit acknowledgement');});
   else throw Error('unknown test operation');
   result??={done:true};
  }else throw Error('unknown fixture path');
  res.setHeader('Content-Type','application/json');res.end(JSON.stringify(result));
 }catch{res.statusCode=503;res.end('{}');}});
 await new Promise<void>(r=>server.listen(0,'127.0.0.1',r));put('issuer.json',{port:(server.address() as any).port,subject,minted});
 // Observe first actual authority write, proving fence exclusion before persistence.
 const collections=new Map(), originalCollection=db.collection.bind(db);
 for(const name of ['personal_oauth_resource_admissions','personal_oauth_admissions']){
  const c=originalCollection(name), original=c.findOneAndUpdate.bind(c);c.findOneAndUpdate=(async(...args:any[])=>{put('first-mongo-write');if(existsSync(`${dir}/hold-mongo`)){put('issuer-held');await wait('finish-issuer');}return original(...args as any);}) as any;collections.set(name,c);
 }
 db.collection=((name:string,...args:any[])=>collections.get(name)??originalCollection(name,...args)) as any;
}else{
 await wait('issuer.json');const data=JSON.parse(readFileSync(`${dir}/issuer.json`,'utf8')),base=`http://127.0.0.1:${data.port}`;
 const issuer=issuerAdapter({issuer:issuerUrl,discovery:issuerUrl+'/.well-known/oauth-authorization-server',
  introspection:issuerUrl+'/oauth/introspect',binding:issuerUrl+'/oauth/lifegraph-binding',clientId:'synthetic-gateway',clientSecret:'s'.repeat(43),
  fetchImpl:async(url:any,init:any)=>{
   const pinned=new URL(url);assert.equal(pinned.origin,issuerUrl);
   assert.ok(['/.well-known/oauth-authorization-server','/oauth/introspect','/oauth/lifegraph-binding'].includes(pinned.pathname));
   return fetch(base+pinned.pathname,init);
  }});
 const authority=createLifeGraphIdentityAuthority({issuer,resource,resolveBinding:(c:any)=>issuer.resolveBinding!({...c,resource})});
 const specs:any[]=[['goal:synthetic-root','Goal','Goal','synthetic goal',[]],['goal:synthetic-variant','Goal','Goal','synthetic goal variant',['goal:synthetic-root']],['idea:synthetic','Idea','GrowthHypothesis','synthetic idea',['person:synthetic']],['openloop:synthetic','OpenLoop','OpenLoop','synthetic open loop',[]],['person:synthetic','Person','Person','synthetic person',[]],['place:synthetic','Place','Place','synthetic place',[]],['asset:synthetic','Thing','Asset','synthetic thing',[]]];
 const node=([id,kind,label,summary,sources]:any[])=>({id,kind,label,sources,contentDigest:lifeGraphContentDigest({id,labels:[label],summary,sources}),...(id==='goal:synthetic-variant'?{canonicalId:'goal:synthetic-root',rootBinding:{type:'verifiedKey',key:'synthetic-key'}}:{})});
 const catalogs=[{namespace:'approved_dot',nodes:specs.map(node),edges:[{id:'edge:synthetic',from:'goal:synthetic-root',to:'person:synthetic',relation:'RELATED_TO',sources:[]}]},{namespace:'approved_claude',nodes:[node(['goal:synthetic-other','Goal','Goal','synthetic claude goal',[]])],edges:[]}];
 const path=`${dir}/privacy.sqlite`,db=new DatabaseSync(path);
 db.exec('PRAGMA user_version=2; CREATE TABLE privacy_revision(singleton INTEGER PRIMARY KEY,revision INTEGER); INSERT INTO privacy_revision VALUES(1,1); CREATE TABLE privacy_policy(resource TEXT PRIMARY KEY,policy_json TEXT NOT NULL); CREATE TABLE local_authority_receipt(hotel,origin,task,handle,binding,cancelled); CREATE TABLE capture_inbox(producer,event_id,actor,payload,sources_json,policy_revision)');
 for(const catalog of catalogs)for(const record of [...catalog.nodes,...catalog.edges])db.prepare('INSERT INTO privacy_policy VALUES(?,?)').run(record.id,JSON.stringify({owner:'synthetic-other-owner',creator:record.id==='asset:synthetic'?'agent-other':catalog.namespace==='approved_dot'?'agent-dot':'agent-claude',creator_read_grant:true,read_roles:record.id==='asset:synthetic'?['reader']:[],private:record.id==='person:synthetic',external_operations:record.id==='place:synthetic'?[]:['inference'],sources:record.sources}));
 db.close(); const {chmodSync}=await import('node:fs');chmodSync(path,0o600);const stat=statSync(path);
 const bytes=JSON.stringify({version:1,catalogs});put('catalog.json',bytes);const {createHash}=await import('node:crypto');
 const storageConfig={policyDatabase:path,storeIdentity:{device:String(stat.dev),inode:String(stat.ino)},catalogFile:`${dir}/catalog.json`,catalogDigest:createHash('sha256').update(bytes).digest('hex'),ownerSocket:process.env.PERSONAL_OAUTH_LIFEGRAPH_OWNER_SOCKET,ownerIdentity:process.env.PERSONAL_OAUTH_LIFEGRAPH_OWNER_ID};
 const config={resource,profile:'approved-read-only-v1',allowedClients:['dot','claude'],allowedSubjects:[data.subject],clientPolicies:['dot','claude'].map(clientId=>({clientId,subjects:[data.subject],namespace:`approved_${clientId}`,scopes:['life:recall']})),requestDeadlineMs:5000};
 if(process.env.SYNTHETIC_PRODUCTION_COMPOSITION==='1'){
  const server=await createApprovedLifeGraphGateway({config:{...config,storage:storageConfig},issuer});
  await new Promise<void>(r=>server.listen(0,'127.0.0.1',r));put('gateway.json',{port:(server.address() as any).port,productionComposition:true});
 }else{
 const storage=createLifeGraphOwnedStorage({...storageConfig,enabled:true,profile:'approved-read-only-v1',issuerAuthority:authority});
 const wrapped={...storage,async authorizeRelease(admission:any){if(existsSync(`${dir}/pause-final`)){put('before-final');await wait('finish-final');}return storage.authorizeRelease(admission);},bindResponse(req:any,res:any){storage.bindResponse(req,res);const end=res.end.bind(res);res.end=(...args:any[])=>{if(existsSync(`${dir}/pause-handoff`)){put('handoff-held');void wait('finish-handoff').then(()=>end(...args)).catch(()=>res.destroy());return res;}return end(...args);};}};
 const server=await createLifeGraphGateway({enabled:true,config,issuer,identityAuthority:authority,storageAuthorityFactory:async()=>wrapped});
 await new Promise<void>(r=>server.listen(0,'127.0.0.1',r));put('gateway.json',{port:(server.address() as any).port});
 }
}
