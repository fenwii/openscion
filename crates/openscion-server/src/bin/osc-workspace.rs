//! Privileged same-host CLI. OS filesystem access to service data is required.
//! For untrusted remote agents use authenticated HTTP API instead.
use anyhow::Result;
use clap::{Parser,Subcommand};
use openscion_registry::{Principal,Registry};
use openscion_workspace::{WorkspaceManager,CreateRequest,CommitRequest};
use std::{path::PathBuf,io::{self,Write}};
use uuid::Uuid;
#[derive(Parser)]
#[command(name="osc-workspace",version,about="OpenScion V0.3 trusted local workspace utility")]
struct Opt {#[arg(long)]data:PathBuf, #[command(subcommand)]cmd:Action}
#[derive(Subcommand)]
enum Action {
 Create{#[arg(long)]repo_id:String,#[arg(long)]base:String,#[arg(long)]task:Option<String>,#[arg(long,default_value_t=86400)]lease:u64},
 List,
 Show{ id:String },
 Status{ id:String },
 Diff{ id:String },
 Commit{ id:String, #[arg(long)]expected_head:String,#[arg(long)]message:String,#[arg(long="path",required=true)]paths:Vec<String>},
 Renew{ id:String,#[arg(long,default_value_t=86400)]lease:u64 },
 Dispose{ id:String },
 Recover,
 Expire,
}
fn main()->Result<()> {
 let o=Opt::parse();
 let manager=WorkspaceManager::new(Registry::open(&o.data)?);
 let admin=Principal{id:"admin".into(),role:"admin".into()};
 match o.cmd {
  Action::Create{repo_id,base,task,lease}=>println!("{}",serde_json::to_string_pretty(&manager.create(&admin,CreateRequest{repository_id:repo_id,base_revision:base,task_id:task,lease_seconds:Some(lease),idempotency_key:Uuid::new_v4().to_string()})?)?),
  Action::List=>println!("{}",serde_json::to_string_pretty(&manager.list(&admin)?)?),
  Action::Show{id}=>println!("{}",serde_json::to_string_pretty(&manager.get(&admin,&id)?)?),
  Action::Status{id}=>println!("{}",serde_json::to_string_pretty(&manager.status(&admin,&id)?)?),
  Action::Diff{id}=>{io::stdout().write_all(&manager.diff(&admin,&id)?)?;},
  Action::Commit{id,expected_head,message,paths}=>println!("{}",serde_json::to_string_pretty(&manager.commit(&admin,&id,CommitRequest{expected_head,message,selected_paths:paths,idempotency_key:Uuid::new_v4().to_string()})?)?),
  Action::Renew{id,lease}=>println!("{}",serde_json::to_string_pretty(&manager.renew(&admin,&id,lease)?)?),
  Action::Dispose{id}=>println!("{}",serde_json::to_string_pretty(&manager.dispose(&admin,&id)?)?),
  Action::Recover=>println!("{}",serde_json::to_string_pretty(&manager.recover_all()?)?),
  Action::Expire=>println!("expired {}",manager.expire_all()?),
 }
 Ok(())
}
