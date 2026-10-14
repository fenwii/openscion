//! Offline backup and integrity manifest. Service, Git writers and shadow
//! workers MUST be stopped externally. This module does not pretend that
//! copying a live Git repository + SQLite is an atomic distributed backup.
use anyhow::{anyhow,bail,Context,Result};
use fs2::FileExt;
use openscion_registry::{Principal,Registry};
use rusqlite::{params,Connection};
use serde::{Deserialize,Serialize};
use sha2::{Digest,Sha256};
use std::{fs::{self,File,OpenOptions},io::{Read,Write},path::{Path,PathBuf},process::{Command,Stdio},time::{SystemTime,UNIX_EPOCH}};
use uuid::Uuid;
fn now()->i64{SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct FileDigest {pub path:String,pub bytes:u64,pub sha256:String}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct BackupManifest {pub schema_version:u32,pub backup_id:String,pub created_at:i64,pub files:Vec<FileDigest>,pub git_repository_ids:Vec<String>,pub requires_offline_writers:bool}
fn digest(path:&Path)->Result<FileDigest>{
 let mut file=File::open(path)?;let mut hash=Sha256::new();let mut length=0u64;let mut buf=[0u8;65536];
 loop{let n=file.read(&mut buf)?;if n==0{break;}hash.update(&buf[..n]);length+=n as u64;}
 Ok(FileDigest{path:String::new(),bytes:length,sha256:hex::encode(hash.finalize())})
}
fn copy_safe(src:&Path,dst:&Path,root:&Path,all:&mut Vec<FileDigest>)->Result<()> {
 let md=fs::symlink_metadata(src)?;
 if md.file_type().is_symlink(){bail!("BACKUP_SYMLINK_UNSUPPORTED");}
 if md.is_dir(){fs::create_dir_all(dst)?;
  for entry in fs::read_dir(src)? {let entry=entry?;copy_safe(&entry.path(),&dst.join(entry.file_name()),root,all)?;} 
 } else if md.is_file(){
  fs::copy(src,dst)?;
  let mut d=digest(dst)?;
  let relative=dst.strip_prefix(root)?;
  d.path=relative.to_string_lossy().replace('\\',"/");all.push(d);
 } else {bail!("BACKUP_UNSUPPORTED_FILE_TYPE");}
 Ok(())
}
fn safe_relative(p:&str)->bool{
 !p.is_empty() && !p.starts_with('/') && !p.split('/').any(|x|x.is_empty()||x=="."||x=="..") && !p.contains('\\')
}
#[derive(Clone)]pub struct BackupService {pub registry:Registry}
impl BackupService {
 pub fn new(registry:Registry)->Self{Self{registry}}
 /// Requires stopped service and an independently protected output directory.
 pub fn create_offline(&self,who:&Principal,destination:&Path,ack_stopped:bool)->Result<BackupManifest>{
  Registry::require_admin(who)?;
  if !ack_stopped{bail!("BACKUP_MAINTENANCE_ACK_REQUIRED");}
  let source=self.registry.db_path().parent().ok_or_else(||anyhow!("INVALID_DATA_DIR"))?.canonicalize()?;
  fs::create_dir_all(destination)?;
  let dest=destination.canonicalize()?;
  if dest.starts_with(&source) || source.starts_with(&dest){bail!("BACKUP_LOCATION_UNSAFE");}
  let lockpath=source.join(".backup-v06.lock");
  let lock=OpenOptions::new().create(true).write(true).open(&lockpath)?;
  lock.try_lock_exclusive().context("BACKUP_LOCK_BUSY")?;
  let backup_id=Uuid::new_v4().to_string();
  let stage=dest.join(format!(".backup-{backup_id}.incomplete"));
  fs::create_dir(&stage)?;
  let mut files=Vec::new();let data=stage.join("data");fs::create_dir(&data)?;
  // SQLite VACUUM INTO produces a transactionally consistent DB copy.
  let sqlite_src=Connection::open(self.registry.db_path())?;
  let sqlite_dst=data.join("service.sqlite");
  sqlite_src.execute("VACUUM INTO ?1",[sqlite_dst.to_string_lossy().to_string()])?;
  let mut fd=digest(&sqlite_dst)?;fd.path="data/service.sqlite".into();files.push(fd);
  for entry in fs::read_dir(&source)? {
   let entry=entry?;let name=entry.file_name();
   let display=name.to_string_lossy();
   if matches!(display.as_ref(),"service.sqlite"|"service.sqlite-wal"|"service.sqlite-shm"|".backup-v06.lock"){continue;}
   copy_safe(&entry.path(),&data.join(&name),&stage,&mut files)?;
  }
  let admin=Principal{id:"admin".into(),role:"admin".into()};
  let mut repo_ids=Vec::new();fs::create_dir_all(stage.join("git"))?;
  for repo in self.registry.list(&admin)? {
   let target=stage.join("git").join(format!("{}.bundle",repo.id));
   let out=Command::new("git").env_clear().env("PATH",std::env::var("PATH").unwrap_or_else(|_|"/usr/bin:/bin".into()))
     .env("GIT_CONFIG_NOSYSTEM","1").env("GIT_CONFIG_GLOBAL","/dev/null")
     .env("GIT_NO_LAZY_FETCH","1").arg("-C").arg(&repo.path)
     .arg("bundle").arg("create").arg(&target).arg("--all")
     .stdin(Stdio::null()).output()?;
   if !out.status.success(){bail!("BACKUP_GIT_BUNDLE_FAILED: {}",repo.name);}
   let mut d=digest(&target)?;d.path=format!("git/{}.bundle",repo.id);files.push(d);repo_ids.push(repo.id);
  }
  files.sort_by(|a,b|a.path.cmp(&b.path));
  let manifest=BackupManifest{schema_version:1,backup_id:backup_id.clone(),created_at:now(),files,git_repository_ids:repo_ids,requires_offline_writers:true};
  let body=serde_json::to_vec_pretty(&manifest)?;
  File::create(stage.join("manifest.json"))?.write_all(&body)?;
  File::open(stage.join("manifest.json"))?.sync_all()?;
  Self::verify(&stage)?;
  let final_dir=dest.join(format!("backup-{backup_id}"));fs::rename(&stage,&final_dir)?;
  File::open(&dest)?.sync_all()?;
  // The source DB catalog is best effort and not included in the snapshot.
  let c=Connection::open(self.registry.db_path())?;
  let _=c.execute("INSERT INTO backup_catalog_v6(id,root_path,manifest_sha256,status,created_at,verified_at) VALUES (?1,?2,?3,'VERIFIED',?4,?4)",params![backup_id,final_dir.to_string_lossy().to_string(),hex::encode(Sha256::digest(&body)),now()]);
  drop(lock);
  Ok(manifest)
 }
 pub fn verify(root:&Path)->Result<BackupManifest>{
  let bytes=fs::read(root.join("manifest.json"))?;
  if bytes.len()>16*1024*1024{bail!("BACKUP_MANIFEST_TOO_LARGE");}
  let m:BackupManifest=serde_json::from_slice(&bytes)?;
  if m.schema_version!=1 || !m.requires_offline_writers{bail!("BACKUP_MANIFEST_UNSUPPORTED");}
  let mut previous="";
  for f in &m.files {
   if !safe_relative(&f.path)||f.path.as_str()<=previous{bail!("BACKUP_MANIFEST_INVALID_PATH");}
   previous=&f.path;
   let full=root.join(&f.path);
   if fs::symlink_metadata(&full)?.file_type().is_symlink(){bail!("BACKUP_SYMLINK_INVALID");}
   let check=digest(&full)?;
   if check.bytes!=f.bytes||check.sha256!=f.sha256{bail!("BACKUP_HASH_MISMATCH");}
  }
  Ok(m)
 }
 /// Only validates a restore input. Never overwrites live Git or service data.
 pub fn restore_plan(root:&Path)->Result<serde_json::Value>{
  let m=Self::verify(root)?;
  Ok(serde_json::json!({"backup_id":m.backup_id,"verified_files":m.files.len(),"git_repositories":m.git_repository_ids.len(),"mode":"manual_offline_restore_required","will_modify_live_data":false}))
 }
}
