//! Offline/local V0.5 shadow operator CLI. Reads OSC_TOKEN from the environment.
use anyhow::{anyhow,Result};
use clap::{Parser,Subcommand};
use openscion_registry::Registry;
use openscion_shadow::ShadowService;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name="osc-shadow",version,about="OpenScion V0.5 non-authoritative Git shadow-native engine")]
struct Args {
    #[arg(long)] data:PathBuf,
    #[command(subcommand)] command:Action,
}
#[derive(Subcommand)]
enum Action {
    Enable{repo_id:String,#[arg(long,default_value="refs/heads/main")]tracked_ref:String},
    Status{repo_id:String},
    Observe{repo_id:String},
    Enqueue{repo_id:String,git_oid:String},
    Work{repo_id:String,#[arg(long,default_value_t=1)]max_jobs:usize},
    Import{repo_id:String,git_oid:String},
    Mapping{repo_id:String,git_oid:String},
    Verify{repo_id:String,#[arg(long,default_value_t=10)]limit:usize},
    Jobs{repo_id:String},
    Retry{repo_id:String,job_id:String},
    Rebuild{repo_id:String,git_oid:String},
}
fn main()->Result<()> {
    let args=Args::parse();
    let registry=Registry::open(&args.data)?;
    let token=std::env::var("OSC_TOKEN").map_err(|_|anyhow!("OSC_TOKEN is required"))?;
    let who=registry.authenticate(&token)?.ok_or_else(||anyhow!("invalid OSC_TOKEN"))?;
    let service=ShadowService::new(registry);
    let result=match args.command {
        Action::Enable{repo_id,tracked_ref}=>serde_json::to_value(service.enable(&who,&repo_id,&tracked_ref)?)?,
        Action::Status{repo_id}=>serde_json::to_value(service.status(&who,&repo_id)?)?,
        Action::Observe{repo_id}=>serde_json::to_value(service.observe(&who,&repo_id)?)?,
        Action::Enqueue{repo_id,git_oid}=>serde_json::to_value(service.enqueue(&who,&repo_id,&git_oid)?)?,
        Action::Work{repo_id,max_jobs}=>serde_json::to_value(service.work(&who,&repo_id,max_jobs)?)?,
        Action::Import{repo_id,git_oid}=>serde_json::to_value(service.import_history(&who,&repo_id,&git_oid)?)?,
        Action::Mapping{repo_id,git_oid}=>serde_json::to_value(service.mapping(&who,&repo_id,&git_oid)?)?,
        Action::Verify{repo_id,limit}=>serde_json::to_value(service.verify_sample(&who,&repo_id,limit)?)?,
        Action::Jobs{repo_id}=>serde_json::to_value(service.jobs(&who,&repo_id)?)?,
        Action::Retry{repo_id,job_id}=>serde_json::to_value(service.retry_failed(&who,&repo_id,&job_id)?)?,
        Action::Rebuild{repo_id,git_oid}=>serde_json::to_value(service.rebuild_derived(&who,&repo_id,&git_oid)?)?,
    };
    println!("{}",serde_json::to_string_pretty(&result)?);
    Ok(())
}
