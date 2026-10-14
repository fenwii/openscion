//! Local operator/client helper for change management; token read from OSC_TOKEN.
use anyhow::{anyhow,Result};
use clap::{Parser,Subcommand};
use openscion_change::{ChangeService,SubmitChange};
use openscion_land::{LandService,PreviewRequest,TrustedAttestation};
use openscion_registry::Registry;
use std::path::PathBuf;
#[derive(Parser)]#[command(name="osc-change",version,about="OpenScion V0.4 Git-authoritative change coordination")]
struct Args{#[arg(long)]data:PathBuf,#[command(subcommand)]command:Action}
#[derive(Subcommand)]enum Action{
 Submit{#[arg(long)]repo_id:String,#[arg(long)]workspace_id:String,#[arg(long)]source:String,#[arg(long,default_value="refs/heads/main")]target:String,#[arg(long)]title:String,#[arg(long,default_value="")]description:String,#[arg(long)]key:String},
 List,Get{change_id:String},
 Preview{change_id:String,#[arg(long)]key:String},
 GetOp{operation_id:String},Validate{operation_id:String},
 Attest{operation_id:String,#[arg(long)]check:String,#[arg(long)]candidate:String,#[arg(long)]tree:String,#[arg(long)]suite_digest:String,#[arg(long)]runner_digest:String,#[arg(long,default_value_t=false)]passed:bool},
 Review{operation_id:String,#[arg(long,default_value="APPROVE")]decision:String},
 Land{operation_id:String},Reconcile{operation_id:String},Repreview{operation_id:String},
}
fn main()->Result<()>{
 let args=Args::parse();let reg=Registry::open(&args.data)?;
 let token=std::env::var("OSC_TOKEN").map_err(|_|anyhow!("set OSC_TOKEN to an authorized token"))?;
 let who=reg.authenticate(&token)?.ok_or_else(||anyhow!("invalid credential"))?;
 let changes=ChangeService::new(reg.clone());let land=LandService::new(reg);
 let value=match args.command{
 Action::Submit{repo_id,workspace_id,source,target,title,description,key}=>serde_json::to_value(changes.submit(&who,SubmitChange{repository_id:repo_id,source_workspace_id:workspace_id,source_commit_oid:source,target_ref:target,title,description,idempotency_key:key})?)?,
 Action::List=>serde_json::to_value(changes.list(&who)?)?,
 Action::Get{change_id}=>serde_json::to_value(changes.get(&who,&change_id)?)?,
 Action::Preview{change_id,key}=>serde_json::to_value(land.start(&who,PreviewRequest{change_id,idempotency_key:key})?)?,
 Action::GetOp{operation_id}=>serde_json::to_value(land.get(&who,&operation_id)?)?,
 Action::Validate{operation_id}=>serde_json::to_value(land.validate(&who,&operation_id)?)?,
 Action::Attest{operation_id,check,candidate,tree,suite_digest,runner_digest,passed}=>serde_json::to_value(land.attest(&who,&operation_id,TrustedAttestation{check_name:check,candidate_oid:candidate,tree_oid:tree,suite_digest,runner_digest,passed})?)?,
 Action::Review{operation_id,decision}=>serde_json::to_value(land.review(&who,&operation_id,&decision)?)?,
 Action::Land{operation_id}=>serde_json::to_value(land.land(&who,&operation_id)?)?,
 Action::Reconcile{operation_id}=>serde_json::to_value(land.reconcile(&who,&operation_id)?)?,
 Action::Repreview{operation_id}=>serde_json::to_value(land.repreview(&who,&operation_id)?)?,
 };
 println!("{}",serde_json::to_string_pretty(&value)?);Ok(())
}
