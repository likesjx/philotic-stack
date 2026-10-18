//! Persistent operator-only adapter. Reuses hotel encryption/storage and protected key handoff.
mod vault;
mod private_start;
use anyhow::{ensure,Result};
use std::{io::{self,Read},sync::Arc};
use ansible_mesh_core::{domain::GraphDomain,sqlite_storage::SqliteGraphStorage};
const ROLE:&str="percival-personal-recall";
const GUEST:&str="percival-personal-gateway";
fn request_scope(req:&serde_json::Value)->Result<(&str,&str)> {
 let kind=req["kind"].as_str().unwrap_or("");let scope=req["scope"].as_str().unwrap_or("");
 ensure!(req["allowed_roles"]==serde_json::json!([ROLE]) && req["allowed_guests"]==serde_json::json!([GUEST]),"acl");
 match kind {
  "percival-muninn-observe"=>ensure!(!scope.is_empty() && scope.len()<=64 && scope.bytes().all(|b|(b.is_ascii_lowercase() || b.is_ascii_digit()) || b==b'_' || b==b'-'),"scope"),
  "percival-issuer-introspection"=>ensure!(scope=="percival_gateway_persistent_recall_v1","scope"),
  _=>anyhow::bail!("kind")
 };Ok((kind,scope))
}
fn run()->Result<()> {
 ensure!(std::env::args().nth(1).as_deref()==Some("--apply-scoped-persistent"),"approval");
 ensure!(std::env::args().nth(2).as_deref()==Some("--root-private-start"),"private start");
 private_start::enter()?;
 private_start::receive_key(std::env::args().nth(3).ok_or_else(||anyhow::anyhow!("channel"))?.parse()?)?;
 ensure!(std::env::var_os("PHILOTIC_VAULT_MASTER_KEY").is_none() && std::env::var("PHILOTIC_VAULT_KEYCHAIN").as_deref()==Ok("0"),"key contract");
 let mut raw=String::new();io::stdin().take(16385).read_to_string(&mut raw)?;ensure!(raw.len()<=16384,"size");
 let req:serde_json::Value=serde_json::from_str(&raw)?;let (kind,scope)=request_scope(&req)?;
 ensure!(req["operation"]=="create","operation");
 let secret=req["secret"].as_str().unwrap_or("");ensure!((32..4096).contains(&secret.len()) && !secret.bytes().any(|b|b<=32 || b==127),"secret");
 let db="/opt/philotic/data/aiua_context.db";ensure!(std::path::Path::new(db).is_file(),"existing db required");
 let storage=SqliteGraphStorage::open_existing(db)?;let graph=GraphDomain::new(Arc::new(storage.adapter()));
 let reference=vault::store_secret(&graph,vault::SecretInput{secret_kind:kind.into(),scope:scope.into(),allowed_roles:vec![ROLE.into()],allowed_guests:vec![GUEST.into()],plaintext:secret.into()})?;
 let row=graph.get_secret(&reference)?.ok_or_else(||anyhow::anyhow!("readback"))?;
 ensure!(row.secret_kind==kind && row.scope==scope && row.allowed_roles==[ROLE] && row.allowed_guests==[GUEST],"readback");
 ensure!(vault::resolve_secret_exact_acl(&graph,&reference,&vault::SecretAccess{role:ROLE.into(),guest_id:GUEST.into()})?.as_deref()==Some(secret),"readback");
 println!("{}",serde_json::json!({"reference":reference}));Ok(())
}
fn main(){if run().is_err(){eprintln!("persistent broker stopped; reconcile journal");std::process::exit(1)}}
#[cfg(test)]mod tests {
 use super::*;
 #[test]fn persistent_wire_scope_and_acl(){
  let mut r=serde_json::json!({"kind":"percival-muninn-observe","scope":"approved_fixture","allowed_roles":[ROLE],"allowed_guests":[GUEST]});
  assert!(request_scope(&r).is_ok());r["allowed_guests"]=serde_json::json!([GUEST,"other"]);assert!(request_scope(&r).is_err());
  r["allowed_guests"]=serde_json::json!([GUEST]);r["scope"]=serde_json::json!("../default");assert!(request_scope(&r).is_err());
  r["kind"]=serde_json::json!("percival-issuer-introspection");r["scope"]=serde_json::json!("percival_gateway_persistent_recall_v1");assert!(request_scope(&r).is_ok());
  r["scope"]=serde_json::json!("claude_percival_synthetic_recall_v1");assert!(request_scope(&r).is_err());
 }
 #[test] #[ignore] fn persistent_record_child(){
  let dir=std::env::var("PUBLIC_PERSISTENCE_FIXTURE").expect("fixture only");
  let graph=GraphDomain::new(Arc::new(SqliteGraphStorage::open(std::path::Path::new(&dir).join("fixture.db")).unwrap().adapter()));
  let reference=vault::store_secret(&graph,vault::SecretInput{secret_kind:"percival-muninn-observe".into(),scope:"fixture_vault".into(),allowed_roles:vec![ROLE.into()],allowed_guests:vec![GUEST.into()],plaintext:"PUBLIC_FIXTURE_NOT_A_CREDENTIAL_7381".into()}).unwrap();
  std::fs::write(std::path::Path::new(&dir).join("reference"),reference).unwrap();
 }
 #[test] fn encrypted_record_outlives_helper_process_without_automatic_expiry(){
  use base64::Engine;
  let dir=std::env::temp_dir().join(format!("persistent-public-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&dir).unwrap();
  let key=base64::engine::general_purpose::STANDARD.encode([7u8;32]);
  let result=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","tests::persistent_record_child","--ignored"]).env("PUBLIC_PERSISTENCE_FIXTURE",&dir).env("PHILOTIC_VAULT_MASTER_KEY",&key).env("PHILOTIC_VAULT_KEYCHAIN","0").output().unwrap();
  assert!(result.status.success());
  unsafe{std::env::set_var("PHILOTIC_VAULT_MASTER_KEY",key);std::env::set_var("PHILOTIC_VAULT_KEYCHAIN","0");}
  let reference=std::fs::read_to_string(dir.join("reference")).unwrap();
  {let graph=GraphDomain::new(Arc::new(SqliteGraphStorage::open_existing(dir.join("fixture.db")).unwrap().adapter()));
   let row=graph.get_secret(&reference).unwrap().unwrap();assert_eq!(row.scope,"fixture_vault");assert_ne!(row.ciphertext_b64,"PUBLIC_FIXTURE_NOT_A_CREDENTIAL_7381");
   assert_eq!(vault::resolve_secret_exact_acl(&graph,&reference,&vault::SecretAccess{role:ROLE.into(),guest_id:GUEST.into()}).unwrap().as_deref(),Some("PUBLIC_FIXTURE_NOT_A_CREDENTIAL_7381"));
   assert!(vault::resolve_secret_exact_acl(&graph,&reference,&vault::SecretAccess{role:"other".into(),guest_id:GUEST.into()}).is_err());
  }
  std::fs::remove_dir_all(dir).unwrap();
 }

}
