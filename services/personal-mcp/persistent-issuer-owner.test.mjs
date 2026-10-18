import test from 'node:test';
import assert from 'node:assert/strict';
import {createCanonicalIssuerOwner} from './persistent-issuer-owner.mjs';
import {createPersistentOwnerBoundary} from './persistent-owner-boundary.mjs';
const issuer='https://issuer.fixture.test',resource='https://mcp.fixture.test/mcp';
test('canonical introspection claims reject scope identity audience and time drift',async()=>{
 for(const patch of [{iss:'https://wrong.test'},{aud:[resource]},{scope:'memory:write'},{sub:'other'},{client_id:'other'},
   {iat:101},{exp:100},{iat:0,exp:1000},{exp:'190'},{active:false}]) {
  const owner=await createCanonicalIssuerOwner({issuer,resource,discovery:issuer+'/metadata',introspection:issuer+'/introspect',subjects:['fixture'],
   clients:['dot','claude'],clientId:'fixture-gateway',credential:async()=> 'public-dummy-secret-'.repeat(3),clock:()=>100000,
   fetchImpl:async url=>new Response(JSON.stringify(url.endsWith('/metadata')?
    {issuer,code_challenge_methods_supported:['S256'],response_types_supported:['code'],grant_types_supported:['authorization_code'],authorization_endpoint:issuer+'/authorize',token_endpoint:issuer+'/token',introspection_endpoint:issuer+'/introspect'}:
    {active:true,iss:issuer,aud:resource,sub:'fixture',client_id:'dot',scope:'memory:recall',iat:90,exp:190,...patch}))});
  await assert.rejects(owner.authorizeRecall({accessToken:'A'.repeat(43),resource,scope:'memory:recall'}));
 }
});
test('real issuer adapter composes with boundary without fabricated grant version',async()=>{
 let active=true, calls=0;const token='A'.repeat(43);
 const owner=await createCanonicalIssuerOwner({issuer,resource,discovery:issuer+'/metadata',introspection:issuer+'/introspect',
  subjects:['fixture'],clients:['dot','claude'],clientId:'fixture-gateway',credential:async()=> 'public-dummy-secret-'.repeat(3),clock:()=>100000,
  fetchImpl:async(url,init)=>{
   if(url.endsWith('/metadata'))return new Response(JSON.stringify({issuer,code_challenge_methods_supported:['S256'],response_types_supported:['code'],grant_types_supported:['authorization_code'],authorization_endpoint:issuer+'/authorize',token_endpoint:issuer+'/token',introspection_endpoint:issuer+'/introspect'}));
   calls++;assert.equal(new URLSearchParams(init.body).get('token'),token);
   return new Response(JSON.stringify({active,iss:issuer,aud:resource,sub:'fixture',client_id:'dot',scope:'memory:recall',iat:90,exp:190}));
  }});
 const claim=await owner.authorizeRecall({accessToken:token,resource,scope:'memory:recall'});
 assert.equal(claim.clientVersion,undefined);assert.match(claim.authorizationBinding,/^[a-f0-9]{64}$/);
 const service=createPersistentOwnerBoundary({resource,clients:['dot','claude'],issuer:owner,clock:()=>100000,
  scopeOwner:{resolve:async()=>({vaults:['approved_fixture'],expiresAt:180})},lifecycleOwner:{read:async()=>({phase:'active',credentialExpiresAt:180})},
  privacyAuthority:{filterAndBind:async x=>x,verify:async()=>true},upstream:{recall:async()=>{active=false;return {records:[]};}}});
 await assert.rejects(service.recall({accessToken:token,vault:'approved_fixture',arguments:{context:['synthetic']}}),/denied/);
 assert.equal(calls,4); // one explicit check; initial, pre-fetch and post-fetch checks
});
