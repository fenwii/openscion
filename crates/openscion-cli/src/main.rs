//! OpenScion V0.1 local CLI (`osc`).
use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use openscion_cas::ObjectReader;
use openscion_core::Repository;
use openscion_tree::{load_tree, resolve};
use openscion_types::{ObjectId, ObjectKind, RepoName};
use serde_json::json;
use std::{fs, path::{Component, Path, PathBuf}, str::FromStr};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

#[derive(Parser)]
#[command(name = "osc", version, about = "OpenScion V0.1 — local immutable source snapshots")]
struct Cli {
    #[arg(long, global=true, help="Print JSON where supported")]
    json: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Init { #[arg(default_value=".")] path: PathBuf },
    Repo { #[command(subcommand)] command: RepoCommand },
    Object { #[command(subcommand)] command: ObjectCommand },
    Tree { #[command(subcommand)] command: TreeCommand },
    Snapshot { #[command(subcommand)] command: SnapshotCommand },
    Doctor { #[arg(long, default_value=".")] repo: PathBuf },
    Fsck { #[arg(long, default_value=".")] repo: PathBuf, #[arg(long)] all: bool },
}
#[derive(Subcommand)]
enum RepoCommand {
    Info { #[arg(long,default_value=".")] repo: PathBuf },
}
#[derive(Subcommand)]
enum ObjectCommand {
    Put { file: PathBuf, #[arg(long, default_value=".")] repo: PathBuf },
    Get { id: String, #[arg(long)] output: PathBuf, #[arg(long,default_value=".")] repo: PathBuf },
    Verify { id: String, #[arg(long,default_value=".")] repo: PathBuf },
    Info { id: String, #[arg(long,default_value=".")] repo: PathBuf },
}
#[derive(Subcommand)]
enum TreeCommand {
    Show { id: String, #[arg(long, default_value=".")] repo: PathBuf },
    Resolve { id: String, path: PathBuf, #[arg(long,default_value=".")] repo: PathBuf },
    Verify { id: String, #[arg(long,default_value=".")] repo: PathBuf },
}
#[derive(Subcommand)]
enum SnapshotCommand {
    Create { #[arg(long)] from: PathBuf, #[arg(long)] name: String, #[arg(long,default_value=".")] repo: PathBuf },
    List { #[arg(long,default_value=".")] repo: PathBuf },
    Show { name: String, #[arg(long,default_value=".")] repo: PathBuf },
    Export { name: String, #[arg(long)] to: PathBuf, #[arg(long,default_value=".")] repo: PathBuf },
}
fn main() {
    if let Err(e) = run() { eprintln!("openscion error: {e:#}"); std::process::exit(1); }
}
fn emit(is_json: bool, structured: serde_json::Value, text: String) {
    if is_json { println!("{structured}"); } else { println!("{text}"); }
}
fn run() -> Result<()> {
    let args = Cli::parse();
    let use_json = args.json;
    match args.command {
        Command::Init { path } => {
            let r = Repository::init(&path)?;
            emit(use_json,json!({"operation":"init","repository":r.root}),format!("Initialized OpenScion at {}",r.root.display()));
        }
        Command::Repo { command: RepoCommand::Info { repo } } => {
            let r = Repository::open(&repo)?;
            let config = fs::read_to_string(r.metadata_dir().join("config.toml"))?;
            emit(use_json,json!({"repository":r.root,"config":config}),format!("Repository: {}\n{}",r.root.display(),config));
        }
        Command::Object { command } => match command {
            ObjectCommand::Put { file, repo } => {
                let r = Repository::open(&repo)?;
                let id = r.store.put_blob_file(&file)?;
                emit(use_json,json!({"object_id":id.to_string(),"kind":"blob"}),id.to_string());
            }
            ObjectCommand::Get { id, output, repo } => {
                let r=Repository::open(&repo)?;
                let id=ObjectId::from_str(&id)?;
                if id.kind != ObjectKind::Blob { bail!("object get only exports Blob objects"); }
                if fs::symlink_metadata(&output).is_ok() { bail!("output already exists; refusing to overwrite"); }
                let bytes=r.store.get(id)?;
                let mut f=fs::OpenOptions::new().create_new(true).write(true).open(&output)?;
                use std::io::Write;
                f.write_all(&bytes)?; f.sync_all()?;
                emit(use_json,json!({"object_id":id.to_string(),"output":output}),format!("Saved {id} to {}",output.display()));
            }
            ObjectCommand::Verify { id, repo } => {
                let r=Repository::open(&repo)?; let id=ObjectId::from_str(&id)?;
                r.store.verify(id)?;
                emit(use_json,json!({"object_id":id.to_string(),"valid":true}),format!("VALID {id}"));
            }
            ObjectCommand::Info { id, repo } => {
                let r=Repository::open(&repo)?; let id=ObjectId::from_str(&id)?;
                let data=r.store.get(id)?;
                emit(use_json,json!({"object_id":id.to_string(),"kind":id.kind.tag(),"size":data.len()}),
                    format!("{id} kind={} size={} bytes",id.kind.tag(),data.len()));
            }
        },
        Command::Tree { command } => match command {
            TreeCommand::Show { id, repo } => {
                let r=Repository::open(&repo)?; let id=ObjectId::from_str(&id)?;
                let tree=load_tree(&r.store,id)?;
                let entries: Vec<_>=tree.entries.iter().map(|e| json!({"name_hex":hex::encode(e.name.as_bytes()),"mode":format!("{:o}",e.mode.number()),"object_id":e.id.to_string()})).collect();
                let output=tree.entries.iter().map(|e|format!("{:o} {} {}",e.mode.number(),e.id,String::from_utf8_lossy(e.name.as_bytes()))).collect::<Vec<_>>().join("\n");
                emit(use_json,json!({"tree":id.to_string(),"entries":entries}),output);
            }
            TreeCommand::Resolve { id, path, repo } => {
                let r=Repository::open(&repo)?; let root=ObjectId::from_str(&id)?;
                let components=parse_repo_path(&path)?;
                let e=resolve(&r.store,root,&components)?;
                emit(use_json,json!({"id":e.id.to_string(),"mode":e.mode.number()}),format!("{} mode={:o}",e.id,e.mode.number()));
            }
            TreeCommand::Verify { id, repo } => {
                let r=Repository::open(&repo)?; let id=ObjectId::from_str(&id)?;
                load_tree(&r.store,id)?;
                emit(use_json,json!({"tree":id.to_string(),"valid":true}),format!("VALID {id}"));
            }
        },
        Command::Snapshot { command } => match command {
            SnapshotCommand::Create { from, name, repo } => {
                let mut r=Repository::open(&repo)?;
                let snap=r.create_snapshot(&from,&name)?;
                emit(use_json,json!({"snapshot_id":snap.id,"name":snap.name,"root_tree":snap.root_tree.to_string()}),
                    format!("snapshot={} name={} root={}",snap.id,snap.name,snap.root_tree));
            }
            SnapshotCommand::List { repo } => {
                let r=Repository::open(&repo)?; let list=r.list_snapshots()?;
                let structured: Vec<_>=list.iter().map(|s|json!({"id":s.id,"name":s.name,"root_tree":s.root_tree.to_string(),"created_at":s.created_at})).collect();
                let human=list.iter().map(|s|format!("{} {} {}",s.id,s.name,s.root_tree)).collect::<Vec<_>>().join("\n");
                emit(use_json,json!({"snapshots":structured}),human);
            }
            SnapshotCommand::Show { name, repo } => {
                let r=Repository::open(&repo)?; let s=r.get_snapshot(&name)?;
                emit(use_json,json!({"id":s.id,"name":s.name,"root_tree":s.root_tree.to_string(),"created_at":s.created_at,"source":s.source_description}),
                    format!("snapshot={}\nname={}\nroot={}\ncreated_at={}",s.id,s.name,s.root_tree,s.created_at));
            }
            SnapshotCommand::Export { name, to, repo } => {
                let r=Repository::open(&repo)?; let s=r.get_snapshot(&name)?;
                r.export_snapshot(&s,&to)?;
                emit(use_json,json!({"snapshot_id":s.id,"exported_to":to}),format!("Exported {} to {}",s.name,to.display()));
            }
        },
        Command::Doctor { repo } => {
            let r=Repository::open(&repo)?;
            let results=r.fsck(false)?;
            emit(use_json,json!({"healthy":true,"snapshots":results.snapshots,"trees":results.trees,"blobs":results.blobs}),
                format!("Repository OK: {} snapshots, {} trees, {} blobs",results.snapshots,results.trees,results.blobs));
        }
        Command::Fsck { repo, all } => {
            let r=Repository::open(&repo)?; let v=r.fsck(all)?;
            emit(use_json,json!({"healthy":true,"snapshots":v.snapshots,"trees":v.trees,"blobs":v.blobs,"published_objects":v.published_objects}),
                format!("FSCK OK snapshots={} trees={} blobs={} published_objects={}",v.snapshots,v.trees,v.blobs,v.published_objects));
        }
    }
    Ok(())
}
fn parse_repo_path(path: &Path) -> Result<Vec<RepoName>> {
    let mut components=Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(n) => {
                #[cfg(unix)]
                { components.push(RepoName::new(n.as_bytes().to_vec())?); }
                #[cfg(not(unix))]
                { components.push(RepoName::new(n.to_string_lossy().as_bytes().to_vec())?); }
            }
            _ => bail!("only relative repository paths without traversal are supported"),
        }
    }
    if components.is_empty() { bail!("path must not be empty"); }
    Ok(components)
}
