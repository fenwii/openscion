//! V0.6 privileged offline operations. Native preview is deliberately gated.
use anyhow::{bail,Result};
use clap::{Parser,Subcommand};
use openscion_authority::AuthorityService;
use openscion_native_ref::{NativeRefService,CasRequest};
use openscion_gateway::GatewayService;
use openscion_operations::BackupService;
use openscion_release::{ReleaseReport,service_db_doctor};
use openscion_registry::{Principal,Registry};
use std::path::PathBuf;
#[derive(Parser)]
#[command(name="osc-ops",version,about="OpenScion 0.6 local privileged operator operations")]
struct Args{#[arg(long)]data:PathBuf,#[command(subcommand)]cmd:Command}
#[derive(Subcommand)]
enum Command{
 Doctor,
 Status{#[arg(long)]repo:String},
 Qualify{#[arg(long)]repo:String},
 Preflight{#[arg(long)]repo:String,#[arg(long,default_value="refs/heads/main")]git_ref:String},
 FeaturePreview{#[arg(long,action=clap::ArgAction::Set)]enable:bool},
 Freeze{#[arg(long)]repo:String,#[arg(long)]epoch:i64,#[arg(long)]git_oid:String,#[arg(long)]native_id:String,#[arg(long)]backup_id:String,#[arg(long,action=clap::ArgAction::Set)]external_writers_fenced:bool},
 Bootstrap{#[arg(long)]repo:String,#[arg(long,default_value="refs/heads/main")]ref_name:String,#[arg(long)]epoch:i64},
 Promote{#[arg(long)]repo:String,#[arg(long)]epoch:i64,#[arg(long,action=clap::ArgAction::Set)]external_writers_fenced:bool},
 Quarantine{#[arg(long)]repo:String,#[arg(long)]reason:String},
 Ref{#[arg(long)]repo:String,#[arg(long,default_value="refs/heads/main")]ref_name:String},
 Cas{#[arg(long)]repo:String,#[arg(long,default_value="refs/heads/main")]ref_name:String,#[arg(long)]expected:String,#[arg(long)]generation:i64,#[arg(long)]epoch:i64,#[arg(long)]new_target:String,#[arg(long)]idempotency_key:String,#[arg(long)]policy_evidence:String},
 Mirror{#[arg(long)]repo:String,#[arg(long,default_value="refs/heads/main")]ref_name:String},
 Fallback{#[arg(long)]repo:String,#[arg(long)]epoch:i64,#[arg(long,action=clap::ArgAction::Set)]external_writers_fenced:bool},
 Backup{#[arg(long)]output:PathBuf,#[arg(long,action=clap::ArgAction::Set)]all_writers_stopped:bool},
 BackupVerify{#[arg(long)]backup:PathBuf},
 RestorePlan{#[arg(long)]backup:PathBuf},
 ReleaseVerify{#[arg(long)]evidence:PathBuf},
}
fn main()->Result<()>{
 let a=Args::parse();
 let who=Principal{id:"admin".into(),role:"admin".into()};
 if let Command::ReleaseVerify{evidence}= &a.cmd {
  let report=ReleaseReport::from_file(evidence)?;
  println!("{}",serde_json::to_string_pretty(&report)?);
  if !report.qualified(){bail!("COMMERCIAL_RELEASE_NOT_QUALIFIED");}return Ok(());
 }
 if let Command::BackupVerify{backup}= &a.cmd {println!("{}",serde_json::to_string_pretty(&BackupService::verify(backup)?)?);return Ok(());}
 if let Command::RestorePlan{backup}= &a.cmd {println!("{}",serde_json::to_string_pretty(&BackupService::restore_plan(backup)?)?);return Ok(());}
 let reg=Registry::open(&a.data)?;
 let authority=AuthorityService::new(reg.clone());let native=NativeRefService::new(reg.clone());
 match a.cmd {
  Command::Doctor=>println!("{}",serde_json::to_string_pretty(&service_db_doctor(&reg)?)?),
  Command::Status{repo}=>println!("{}",serde_json::to_string_pretty(&authority.read(&who,&repo)?)?),
  Command::Qualify{repo}=>println!("{}",authority.qualify_preview(&who,&repo)?),
  Command::Preflight{repo,git_ref}=>{let (git,native)=authority.preflight(&who,&repo,&git_ref)?;println!("git={git} native={native}");},
  Command::FeaturePreview{enable}=>{authority.set_preview_flag(&who,enable)?;println!("native preview enabled={enable}");},
  Command::Freeze{repo,epoch,git_oid,native_id,backup_id,external_writers_fenced}=>println!("{}",serde_json::to_string_pretty(&authority.freeze(&who,&repo,epoch,&git_oid,&native_id,&backup_id,external_writers_fenced)?)?),
  Command::Bootstrap{repo,ref_name,epoch}=>println!("{}",serde_json::to_string_pretty(&native.bootstrap(&who,&repo,&ref_name,epoch)?)?),
  Command::Promote{repo,epoch,external_writers_fenced}=>println!("{}",serde_json::to_string_pretty(&authority.promote_preview(&who,&repo,epoch,external_writers_fenced)?)?),
  Command::Quarantine{repo,reason}=>println!("{}",serde_json::to_string_pretty(&authority.quarantine(&who,&repo,&reason)?)?),
  Command::Ref{repo,ref_name}=>println!("{}",serde_json::to_string_pretty(&native.read(&who,&repo,&ref_name)?)?),
  Command::Cas{repo,ref_name,expected,generation,epoch,new_target,idempotency_key,policy_evidence}=>{
   let req=CasRequest{repo_id:repo,ref_name,expected_target:expected,expected_generation:generation,authority_epoch:epoch,new_target,idempotency_key,policy_evidence};
   println!("{}",serde_json::to_string_pretty(&native.cas(&who,req)?)?);
  },
  Command::Fallback{repo,epoch,external_writers_fenced}=>println!("{}",serde_json::to_string_pretty(&GatewayService::new(reg).fallback_to_git(&who,&repo,epoch,external_writers_fenced)?)?),
  Command::Mirror{repo,ref_name}=>println!("{}",serde_json::to_string_pretty(&GatewayService::new(reg).sync_verified(&who,&repo,&ref_name)?)?),
  Command::Backup{output,all_writers_stopped}=>println!("{}",serde_json::to_string_pretty(&BackupService::new(reg).create_offline(&who,&output,all_writers_stopped)?)?),
  Command::BackupVerify{..}|Command::RestorePlan{..}|Command::ReleaseVerify{..}=>unreachable!(),
 }
 Ok(())
}
