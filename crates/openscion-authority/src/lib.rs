//! V0.6 repository authority manager. No implicit authority promotion.
//! Production default: Git-authoritative. Native preview requires explicit
//! feature flag, verified Git/native mapping, backup checkpoint and admin role.
use anyhow::{anyhow,bail,Result};
use fs2::FileExt;
use std::{fs::{self,OpenOptions},io};
use openscion_git::GitRepository;
use openscion_registry::{Principal,Registry};
use openscion_shadow::ShadowService;
use rusqlite::{params,Connection,OptionalExtension,TransactionBehavior};
use serde::Serialize;
use std::{str::FromStr,time::{SystemTime,UNIX_EPOCH}};
use uuid::Uuid;
fn now()->i64 {SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64}
fn connect(reg:&Registry)->Result<Connection> {let db=Connection::open(reg.db_path())?; db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000; PRAGMA synchronous=FULL;")?; Ok(db)}
#[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize)]
#[serde(rename_all="SCREAMING_SNAKE_CASE")]
pub enum AuthorityMode {GitAuthority,ShadowQualifying,CutoverPreparing,CutoverFrozen,NativeAuthorityPreview,FallbackPreparing,FallbackFrozen,ReadOnly,Quarantined}
impl AuthorityMode {
    pub fn as_str(self)->&'static str {match self {Self::GitAuthority=>"GIT_AUTHORITY",Self::ShadowQualifying=>"SHADOW_QUALIFYING",Self::CutoverPreparing=>"CUTOVER_PREPARING",Self::CutoverFrozen=>"CUTOVER_FROZEN",Self::NativeAuthorityPreview=>"NATIVE_AUTHORITY_PREVIEW",Self::FallbackPreparing=>"FALLBACK_PREPARING",Self::FallbackFrozen=>"FALLBACK_FROZEN",Self::ReadOnly=>"READ_ONLY",Self::Quarantined=>"QUARANTINED"}}
}
impl FromStr for AuthorityMode {type Err=anyhow::Error;fn from_str(s:&str)->Result<Self>{Ok(match s {"GIT_AUTHORITY"=>Self::GitAuthority,"SHADOW_QUALIFYING"=>Self::ShadowQualifying,"CUTOVER_PREPARING"=>Self::CutoverPreparing,"CUTOVER_FROZEN"=>Self::CutoverFrozen,"NATIVE_AUTHORITY_PREVIEW"=>Self::NativeAuthorityPreview,"FALLBACK_PREPARING"=>Self::FallbackPreparing,"FALLBACK_FROZEN"=>Self::FallbackFrozen,"READ_ONLY"=>Self::ReadOnly,"QUARANTINED"=>Self::Quarantined,_=>bail!("UNKNOWN_AUTHORITY_MODE")})}}
#[derive(Debug,Clone,Serialize)]
pub struct Authority {pub repo_id:String,pub mode:AuthorityMode,pub epoch:i64,pub checkpoint_git_oid:Option<String>,pub checkpoint_native_id:Option<String>,pub backup_id:Option<String>}
#[derive(Clone)]pub struct AuthorityService {pub registry:Registry}
impl AuthorityService {
 fn lock_landing(&self,repo_id:&str)->Result<std::fs::File>{
    if repo_id.len()>128 || !repo_id.bytes().all(|x|x.is_ascii_hexdigit()||x==b'-'){bail!("INVALID_REPOSITORY_ID");}
    let base=self.registry.db_path().parent().ok_or_else(||anyhow!("SERVICE_PATH"))?.join("locks");
    fs::create_dir_all(&base)?;
    let file=OpenOptions::new().create(true).read(true).write(true).open(base.join(format!("land-{repo_id}.lock")))?;
    file.lock_exclusive()?;Ok(file)
 }
 /// Preview-only parity qualification. Every mapping in this local repo is
 /// reverified; no silent sampling or acceptance of ineligible jobs.
 pub fn qualify_preview(&self,who:&Principal,repo_id:&str)->Result<String>{
    Registry::require_admin(who)?;
    let repo=self.registry.get_authorized(repo_id,who)?.ok_or_else(||anyhow!("REPOSITORY_NOT_FOUND"))?;
    let git=GitRepository::open(&repo.path)?;let git_tip=git.resolve_ref("refs/heads/main")?;
    let c=connect(&self.registry)?;
    let pending:i64=c.query_row("SELECT COUNT(*) FROM shadow_jobs WHERE repo_id=?1 AND state IN ('PENDING','RUNNING','FAILED','INELIGIBLE')",[repo_id],|r|r.get(0))?;
    if pending!=0{bail!("QUALIFICATION_JOBS_INCOMPLETE");}
    let mut stmt=c.prepare("SELECT git_oid FROM shadow_mappings WHERE repo_id=?1 ORDER BY git_oid")?;
    let items:Vec<String>=stmt.query_map([repo_id],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
    if items.is_empty() || items.len()>10_000 {bail!("QUALIFICATION_CORPUS_UNSUPPORTED");}
    drop(stmt);drop(c);
    let shadow=ShadowService::new(self.registry.clone());
    let mut exact_tip:Option<String>=None;
    for oid in &items {let m=shadow.verify_mapping(who,repo_id,oid)?;if oid==&git_tip {exact_tip=Some(m.native_id);}}
    let native_id=exact_tip.ok_or_else(||anyhow!("QUALIFICATION_TIP_NOT_MAPPED"))?;
    let report=serde_json::json!({"kind":"native-authority-preview-local","verified":items.len(),"git_oid":git_tip,"native_id":native_id,"scope":"supported Git subset; not GA certification"}).to_string();
    let db=connect(&self.registry)?;
    db.execute("INSERT INTO repository_qualifications_v6(id,repo_id,git_oid,native_id,native_root,checks_json,status,created_at) VALUES (?1,?2,?3,?4,'root-checked',?5,'PREVIEW_PASS',?6)",params![Uuid::new_v4().to_string(),repo_id,git_tip,native_id,report,now()])?;
    Ok(report)
 }
 pub fn new(registry:Registry)->Self {Self{registry}}
 pub fn read(&self,who:&Principal,repo_id:&str)->Result<Authority>{
    self.registry.get_authorized(repo_id,who)?.ok_or_else(||anyhow!("REPOSITORY_NOT_FOUND"))?;
    let db=connect(&self.registry)?;
    let v:Option<(String,i64,Option<String>,Option<String>,Option<String>)>=db.query_row("SELECT mode,epoch,checkpoint_git_oid,checkpoint_native_id,backup_id FROM repository_authority_v6 WHERE repo_id=?1",[repo_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let (mode,epoch,git,native,backup)=v.ok_or_else(||anyhow!("AUTHORITY_NOT_REGISTERED"))?;
    Ok(Authority{repo_id:repo_id.to_owned(),mode:mode.parse()?,epoch,checkpoint_git_oid:git,checkpoint_native_id:native,backup_id:backup})
 }
 pub fn assert_git_writer(&self,repo_id:&str)->Result<()> {
    let db=connect(&self.registry)?;
    let mode:Option<String>=db.query_row("SELECT mode FROM repository_authority_v6 WHERE repo_id=?1",[repo_id],|r|r.get(0)).optional()?;
    if mode.as_deref()!=Some("GIT_AUTHORITY") {bail!("AUTHORITY_FENCED");}
    Ok(())
 }
 pub fn preview_enabled(&self)->Result<bool>{let db=connect(&self.registry)?; let x:String=db.query_row("SELECT value FROM release_capabilities_v6 WHERE key='native_authority_preview'",[],|r|r.get(0))?; Ok(x=="enabled")}
 pub fn set_preview_flag(&self,who:&Principal,enabled:bool)->Result<()> {
    Registry::require_admin(who)?;let db=connect(&self.registry)?;
    db.execute("UPDATE release_capabilities_v6 SET value=?1,updated_at=?2 WHERE key='native_authority_preview'",params![if enabled{"enabled"}else{"disabled"},now()])?;Ok(())
 }
 /// Preflight verifies that the exact current tracked ref has a reverified
 /// shadow mapping. It does not freeze writers and is NOT itself cutover approval.
 pub fn preflight(&self,who:&Principal,repo_id:&str,git_ref:&str)->Result<(String,String)>{
    Registry::require_admin(who)?;
    if !git_ref.starts_with("refs/heads/") || git_ref.len()>512 || git_ref.contains("..") {bail!("INVALID_GIT_REF");}
    let repo=self.registry.get_authorized(repo_id,who)?.ok_or_else(||anyhow!("REPOSITORY_NOT_FOUND"))?;
    let git=GitRepository::open(&repo.path)?;
    let git_oid=git.resolve_ref(git_ref)?;
    let mapping=ShadowService::new(self.registry.clone()).verify_mapping(who,repo_id,&git_oid)?;
    if mapping.state!="VERIFIED" {bail!("AUTHORITY_QUALIFICATION_FAILED");}
    Ok((git_oid,mapping.native_id))
 }
 /// A single transaction freezes Git backed mutations and pins the qualified
 /// checkpoint. External Git writers MUST ALSO be fenced at the OS boundary.
 pub fn freeze(&self,who:&Principal,repo_id:&str,expected_epoch:i64,git_oid:&str,native_id:&str,backup_id:&str,os_fenced_ack:bool)->Result<Authority>{
    Registry::require_admin(who)?;
    if !os_fenced_ack {bail!("EXTERNAL_WRITER_FENCING_REQUIRED");}
    if backup_id.trim().is_empty(){bail!("BACKUP_REQUIRED");}
    let _land_guard=self.lock_landing(repo_id)?;
    let c=connect(&self.registry)?;
    let backup_count:i64=c.query_row("SELECT COUNT(*) FROM backup_catalog_v6 WHERE id=?1 AND status='VERIFIED'",[backup_id],|r|r.get(0))?;
    if backup_count!=1 {bail!("VERIFIED_BACKUP_REQUIRED");}

    let (qualified_git,qualified_native)=self.preflight(who,repo_id,"refs/heads/main")?;
    if qualified_git!=git_oid || qualified_native!=native_id {bail!("CHECKPOINT_CHANGED");}
    let qualification:i64=c.query_row("SELECT COUNT(*) FROM repository_qualifications_v6 WHERE repo_id=?1 AND git_oid=?2 AND native_id=?3 AND status='PREVIEW_PASS'",params![repo_id,git_oid,native_id],|r|r.get(0))?;
    if qualification==0 {bail!("AUTHORITY_QUALIFICATION_FAILED");}
    drop(c);

    let mut db=connect(&self.registry)?;
    let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let n=tx.execute("UPDATE repository_authority_v6 SET mode='CUTOVER_FROZEN',epoch=epoch+1,checkpoint_git_oid=?3,checkpoint_native_id=?4,backup_id=?5,updated_at=?6 WHERE repo_id=?1 AND mode='GIT_AUTHORITY' AND epoch=?2",params![repo_id,expected_epoch,git_oid,native_id,backup_id,now()])?;
    if n!=1 {bail!("AUTHORITY_FENCED");}
    tx.execute("INSERT INTO authority_transitions_v6(id,repo_id,actor,from_mode,to_mode,from_epoch,to_epoch,git_checkpoint,native_checkpoint,backup_id,note,created_at) VALUES (?1,?2,?3,'GIT_AUTHORITY','CUTOVER_FROZEN',?4,?5,?6,?7,?8,'operator freeze',?9)",params![Uuid::new_v4().to_string(),repo_id,who.id,expected_epoch,expected_epoch+1,git_oid,native_id,backup_id,now()])?;
    tx.commit()?;self.read(who,repo_id)
 }
 /// Native authority remains an opt-in, single-host PREVIEW. It MUST be
 /// externally fenced from direct Git pushes and protected Git writers.
 pub fn promote_preview(&self,who:&Principal,repo_id:&str,expected_epoch:i64,os_fenced_ack:bool)->Result<Authority>{
    Registry::require_admin(who)?;
    if !os_fenced_ack {bail!("EXTERNAL_WRITER_FENCING_REQUIRED");}
    if !self.preview_enabled()?{bail!("FEATURE_NOT_QUALIFIED");}
    let c=connect(&self.registry)?;
    let boot:i64=c.query_row("SELECT COUNT(*) FROM native_refs_v6 WHERE repo_id=?1",[repo_id],|r|r.get(0))?;
    if boot==0 {bail!("NATIVE_REFS_NOT_BOOTSTRAPPED");}
    drop(c);
    let _guard=self.lock_landing(repo_id)?;

    let mut db=connect(&self.registry)?;let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let n=tx.execute("UPDATE repository_authority_v6 SET mode='NATIVE_AUTHORITY_PREVIEW',epoch=epoch+1,updated_at=?3 WHERE repo_id=?1 AND mode='CUTOVER_FROZEN' AND epoch=?2 AND checkpoint_native_id IS NOT NULL AND backup_id IS NOT NULL",params![repo_id,expected_epoch,now()])?;
    if n!=1{bail!("AUTHORITY_FENCED");}
    tx.execute("UPDATE native_refs_v6 SET authority_epoch=?2 WHERE repo_id=?1",params![repo_id,expected_epoch+1])?;
    tx.execute("INSERT INTO authority_transitions_v6(id,repo_id,actor,from_mode,to_mode,from_epoch,to_epoch,note,created_at) VALUES (?1,?2,?3,'CUTOVER_FROZEN','NATIVE_AUTHORITY_PREVIEW',?4,?5,'explicit operator preview; OS write fencing required',?6)",params![Uuid::new_v4().to_string(),repo_id,who.id,expected_epoch,expected_epoch+1,now()])?;
    tx.commit()?;self.read(who,repo_id)
 }
 /// Stops writes until an operator has re-qualified fallback; not a Git rewind.
 pub fn quarantine(&self,who:&Principal,repo_id:&str,reason:&str)->Result<Authority>{
    Registry::require_admin(who)?;
    if reason.is_empty()||reason.len()>1024{bail!("INVALID_REASON");}
    let _land_guard=self.lock_landing(repo_id)?;
    let mut db=connect(&self.registry)?;let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("UPDATE repository_authority_v6 SET mode='QUARANTINED',epoch=epoch+1,updated_at=?2 WHERE repo_id=?1",params![repo_id,now()])?;
    tx.execute("INSERT INTO operational_incidents_v6(id,repo_id,code,description,state,created_at,updated_at) VALUES (?1,?2,'MANUAL_QUARANTINE',?3,'OPEN',?4,?4)",params![Uuid::new_v4().to_string(),repo_id,reason,now()])?;
    tx.commit()?;self.read(who,repo_id)
 }
}
