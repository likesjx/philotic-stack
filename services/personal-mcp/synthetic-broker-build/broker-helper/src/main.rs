//! Uses the reviewed hotel vault implementation unchanged. No SQL/crypto substitute.
mod vault;
mod private_start;
use std::{io::{self,Read},sync::Arc};
use anyhow::{Result,ensure};
use ansible_mesh_core::{domain::GraphDomain,sqlite_storage::SqliteGraphStorage};
fn run()->Result<()> {
 let private_start = std::env::args().nth(2).as_deref()==Some("--root-private-start");
 if private_start { private_start::enter()?; private_start::receive_key(std::env::args().nth(3).ok_or_else(||anyhow::anyhow!("channel"))?.parse()?)?; }
 ensure!(std::env::args().nth(1).as_deref()==Some("--apply-scoped-synthetic"),"approval");
 ensure!(std::env::var_os("PHILOTIC_VAULT_MASTER_KEY").is_none(),"env disallowed");
 ensure!(unsafe { libc::geteuid() } == 999,"uid");
 let db="/opt/philotic/data/aiua_context.db";
 ensure!(std::path::Path::new(&db).is_file(),"existing db required");
 let mut input=String::new();io::stdin().take(16385).read_to_string(&mut input)?;ensure!(input.len()<=16384,"size");
 let mut req:serde_json::Value=serde_json::from_str(&input)?;input.clear();
 let kind=req["kind"].as_str().unwrap_or("").to_owned();
 let scope=match kind.as_str(){"percival-muninn-observe"=>"percival_connection_test","percival-issuer-introspection"=>"claude_percival_synthetic_recall_v1",_=>anyhow::bail!("kind")};
 let storage=SqliteGraphStorage::open_existing(db)?;let graph=GraphDomain::new(Arc::new(storage.adapter()));
 let role="percival-personal-recall";let guest="percival-personal-gateway";
 match req["operation"].as_str().unwrap_or("") {
 "create"=>{
 ensure!(private_start && std::env::var("PHILOTIC_VAULT_KEYCHAIN").as_deref()==Ok("0"),"protected private start required");
 let secret=req["secret"].take().as_str().unwrap_or("").to_owned();ensure!(secret.len()>=32 && secret.len()<4096,"secret");
 let reference=vault::store_secret(&graph,vault::SecretInput{secret_kind:kind,scope:scope.into(),allowed_roles:vec![role.into()],allowed_guests:vec![guest.into()],plaintext:secret.clone()})?;
 let verified=vault::resolve_secret_exact_acl(&graph,&reference,&vault::SecretAccess{role:role.into(),guest_id:guest.into()})?;
 ensure!(verified.as_deref()==Some(&secret),"readback");
 println!("{}",serde_json::json!({"reference":reference}));
 },
 "delete"=>{
 let reference=req["reference"].as_str().unwrap_or("");let prefix=format!("secret://hotel/default/{kind}/");ensure!(reference.starts_with(&prefix),"reference");
 let record=graph.get_secret(reference)?.ok_or_else(||anyhow::anyhow!("missing"))?;
 ensure!(record.secret_kind==kind && record.scope==scope && record.allowed_roles==[role] && record.allowed_guests==[guest],"acl");
 graph.delete_secret(reference)?;ensure!(graph.get_secret(reference)?.is_none(),"readback");println!("{{\"deleted\":true}}");
 }, _=>anyhow::bail!("operation") }
 Ok(())
}
fn main(){if run().is_err(){eprintln!("scoped broker transaction stopped; reconcile journal");std::process::exit(1)}}
