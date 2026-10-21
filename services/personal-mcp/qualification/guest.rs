use anyhow::{Result,ensure};
use philotic_client::{PhiloticClient,GuestIdentity,IpcRequest,IpcResponse};
use serde_json::{json,Value};
use std::{collections::HashSet,time::Duration};
use tokio::time::timeout;
async fn connect(socket:&str,id:&str,role:&str)->Result<PhiloticClient>{
 timeout(Duration::from_secs(4),PhiloticClient::connect_at(socket,GuestIdentity{guest_id:id.into(),role:role.into(),supported_tools:vec![]})).await?
}
async fn request(c:&mut PhiloticClient,r:IpcRequest)->Result<IpcResponse>{
 c.send_request_with_timeout(r,Duration::from_secs(4)).await
}
async fn config(c:&mut PhiloticClient,key:&str)->Result<Value>{
 match request(c,IpcRequest::GetConfig{key:key.into()}).await? {
 IpcResponse::ConfigData{key:returned,value_json:Some(v)}=>{ensure!(returned==key);Ok(serde_json::from_str(&v)?)},
 other=>anyhow::bail!("missing config response: {other:?}")
 }
}
async fn secret(c:&mut PhiloticClient,reference:&str,expected:&str)->Result<()>{
 match request(c,IpcRequest::GetSecret{secret_ref:reference.into()}).await? {
 IpcResponse::SecretData{secret_ref,value_json:Some(v)}=>{ensure!(secret_ref==reference);ensure!(serde_json::from_str::<String>(&v)?==expected);Ok(())},
 other=>anyhow::bail!("credential not usable: {other:?}")
 }
}
async fn denied(c:&mut PhiloticClient,reference:&str)->Result<()>{
 let response=request(c,IpcRequest::GetSecret{secret_ref:reference.into()}).await?;
 ensure!(!matches!(response,IpcResponse::SecretData{value_json:Some(_),..}),"unauthorized credential returned");Ok(())
}
async fn hop(from:&mut PhiloticClient,to:&mut PhiloticClient,node:&str,role:&str,guest:&str,stage:&str,label:&str,ids:&mut HashSet<String>)->Result<()>{
 let payload=json!({"source":"qualification","session_id":format!("qualification:{label}"),"turn_id":format!("turn:{label}"),"stage":stage,"content":format!("public-{stage}"),"reply_guest_id":guest});
 ensure!(matches!(request(from,IpcRequest::EmitTask{target_node:node.into(),target_role:role.into(),target_guest_id:Some(guest.into()),task_json:payload.to_string()}).await?,IpcResponse::Standard{ok:true,..}));
 loop {
 match timeout(Duration::from_secs(4),to.recv_task()).await?? {
 IpcResponse::InboundTask{task_id,task_json,source_node}=>{
 ensure!(source_node==node,"wrong source node");
 ensure!(ids.insert(task_id.to_string()),"duplicate task id");
 let got:Value=serde_json::from_str(&task_json)?;
 for key in ["session_id","turn_id","stage","content"]{ensure!(got[key]==payload[key],"changed payload field {key}: {got}");}
 return Ok(())},
 IpcResponse::MuninnStatus{..}|IpcResponse::NetworkState{..}=>{},
 other=>anyhow::bail!("unexpected push {other:?}")
 }
 }
}
#[tokio::main]
async fn main()->Result<()>{
 let a:Vec<String>=std::env::args().collect();ensure!(a.len()==4);
 let (socket,node,label)=(&a[1],&a[2],&a[3]);
 let mut m=connect(socket,"qualification-membrane","membrane").await?;
 let mut agent=connect(socket,"qualification-agent","agent").await?;
 let mut model=connect(socket,"qualification-model","model").await?;
 ensure!(config(&mut m,"qualification-marker").await?==json!({"public":"preserve","version":1}));
 let telegram=config(&mut m,"telegram_bot_token_qualification").await?.as_str().unwrap().to_owned();
 let provider=config(&mut model,"openrouter_api_key_ref").await?.as_str().unwrap().to_owned();
 secret(&mut m,&telegram,"public-telegram-token").await?;
 secret(&mut model,&provider,"public-provider-key").await?;
 denied(&mut agent,&telegram).await?;denied(&mut agent,&provider).await?;
 let mut ids=HashSet::new();
 hop(&mut m,&mut agent,node,"agent","qualification-agent","input",label,&mut ids).await?;
 hop(&mut agent,&mut model,node,"model","qualification-model","request",label,&mut ids).await?;
 hop(&mut model,&mut agent,node,"agent","qualification-agent","model-reply",label,&mut ids).await?;
 hop(&mut agent,&mut m,node,"membrane","qualification-membrane","final-reply",label,&mut ids).await?;
 drop(model);tokio::time::sleep(Duration::from_millis(100)).await;
 let mut model=connect(socket,"qualification-model","model").await?;
 hop(&mut agent,&mut model,node,"model","qualification-model","reconnect",label,&mut ids).await?;
 ensure!(ids.len()==5);
 println!("{}",json!({"historical_client_roundtrip":true,"source_proxy_only":true,"label":label,"task_count":ids.len(),"secret_acl_allow_deny":true,"reconnect":true}));
 Ok(())
}
