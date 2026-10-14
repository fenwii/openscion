//! Local privileged operator CLI. Never exposes host paths to normal API users.
use anyhow::Result;
use clap::{Parser,Subcommand};
use openscion_registry::Registry;
use openscion_workspace::WorkspaceManager;
use openscion_land::LandService;
use openscion_policy::Policy;
use std::path::PathBuf;
#[derive(Parser)]
#[command(name="osc-admin",version,about="OpenScion V0.4 local service administration")]
struct Args {#[command(subcommand)] command:Command}
#[derive(Subcommand)]
enum Command {
 Init {#[arg(long)]data:PathBuf,#[arg(long)]allow_root:PathBuf},
 Register {#[arg(long)]data:PathBuf,#[arg(long)]name:String,#[arg(long)]path:PathBuf},
 CreateToken {#[arg(long)]data:PathBuf,#[arg(long)]principal:String,#[arg(long,default_value="reader")]role:String},
 Grant {#[arg(long)]data:PathBuf,#[arg(long)]repo:String,#[arg(long)]principal:String},
 GrantWorkspace {#[arg(long)]data:PathBuf,#[arg(long)]repo:String,#[arg(long)]principal:String,#[arg(long)]permission:String},
 GrantChange {#[arg(long)]data:PathBuf,#[arg(long)]repo:String,#[arg(long)]principal:String,#[arg(long)]permission:String},
 PolicyInstall {#[arg(long)]data:PathBuf,#[arg(long)]repo:String,#[arg(long,default_value="refs/heads/main")]target:String,#[arg(long,default_value_t=0)]approvals:u32,#[arg(long="check")]checks:Vec<String>},
 RecoverWorkspaces {#[arg(long)]data:PathBuf},
 ExpireWorkspaces {#[arg(long)]data:PathBuf},
 RevokePrincipal {#[arg(long)]data:PathBuf,#[arg(long)]principal:String},
 List {#[arg(long)]data:PathBuf},
}
fn main()->Result<()> {
 let args=Args::parse();
 match args.command {
  Command::Init{data,allow_root}=>{
   let (_,token)=Registry::init(&data,&allow_root)?;
   println!("Service initialized. ADMIN TOKEN (displayed once): {token}");
  }
  Command::Register{data,name,path}=>{
   let reg=Registry::open(&data)?;let r=reg.register(&name,&path)?;
   println!("Registered {} id={} format={}",r.name,r.id,r.object_format);
  }
  Command::CreateToken{data,principal,role}=>{
   let reg=Registry::open(&data)?;let token=reg.add_principal(&principal,&role)?;
   println!("TOKEN for {principal} (displayed once): {token}");
  }
  Command::Grant{data,repo,principal}=>{
   let reg=Registry::open(&data)?;reg.grant(&repo,&principal)?;
   println!("Granted repository read access: {repo} => {principal}");
  }
  Command::GrantChange{data,repo,principal,permission}=>{
   let reg=Registry::open(&data)?;reg.grant_change(&repo,&principal,&permission)?;
   println!("Granted change permission {permission}: {repo} => {principal}");
  }
  Command::PolicyInstall{data,repo,target,approvals,checks}=>{
   let reg=Registry::open(&data)?;
   let mut policy=Policy::development(&target);policy.required_approvals=approvals;policy.required_checks.extend(checks);
   let digest=LandService::new(reg).install_policy(&repo,policy)?;
   println!("Installed DEVELOPMENT-ONLY git-tree-integrity policy digest={digest}; not a substitute for application CI");
  }
  Command::GrantWorkspace{data,repo,principal,permission}=>{
   Registry::open(&data)?.grant_workspace(&repo,&principal,&permission)?;
   println!("Granted workspace {permission}: {repo} => {principal}");
  }
  Command::ExpireWorkspaces{data}=>{
   let manager=WorkspaceManager::new(Registry::open(&data)?);
   println!("Expired {} workspace(s)",manager.expire_all()?);
  }
  Command::RecoverWorkspaces{data}=>{
   let manager=WorkspaceManager::new(Registry::open(&data)?);
   for (id,state) in manager.recover_all()? {println!("{id} {state}");}
  }
  Command::RevokePrincipal{data,principal}=>{
   let reg=Registry::open(&data)?;println!("Revoked {} token(s)",reg.revoke_principal(&principal)?);
  }
  Command::List{data}=>{
   let reg=Registry::open(&data)?;
   let admin=openscion_registry::Principal{id:"admin".into(),role:"admin".into()};
   for repo in reg.list(&admin)? {println!("{} {} {}",repo.id,repo.name,repo.object_format);}
  }
 }
 Ok(())
}
