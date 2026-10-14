//! V0.6 Git compatibility gateway: mirror-only and verified-import-only.
//! No arbitrary native Changeset -> Git Commit conversion is claimed.
//! Git refs under refs/heads/* remain untouched in this subsystem.
use anyhow::{anyhow,bail,Result};
use fs2::FileExt;
use std::{fs::{self,OpenOptions}};
use uuid::Uuid;
use rusqlite::TransactionBehavior;
use openscion_authority::{AuthorityMode,AuthorityService};
use openscion_git::GitRepository;
use openscion_native_ref::NativeRefService;
use openscion_registry::{Principal,Registry};
use openscion_shadow::ShadowService;
use rusqlite::{params,Connection,OptionalExtension};
use serde::Serialize;
use std::{process::{Command,Stdio},time::{SystemTime,UNIX_EPOCH,Duration},io::Read,thread};
use wait_timeout::ChildExt;
fn now()->i64{SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64}
fn db(reg:&Registry)->Result<Connection>{let c=Connection::open(reg.db_path())?;c.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000; PRAGMA synchronous=FULL;")?;Ok(c)}
fn run(repo:&std::path::Path,args:&[&str],allow_missing:bool)->Result<Option<String>>{
 let mut cmd=Command::new("git");cmd.env_clear().env("PATH",std::env::var("PATH").unwrap_or_else(|_|"/usr/bin:/bin".into()))
 .env("GIT_CONFIG_NOSYSTEM","1").env("GIT_CONFIG_GLOBAL","/dev/null")
 .env("GIT_NO_LAZY_FETCH","1").env("GIT_NO_REPLACE_OBJECTS","1")
 .env("GIT_TERMINAL_PROMPT","0").arg("--no-pager").arg("-C").arg(repo).args(args)
 .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
 let mut child=cmd.spawn()?;let mut out=child.stdout.take().ok_or_else(||anyhow!("GIT_STDOUT"))?;
 let reader=thread::spawn(move||{let mut data=Vec::new();out.take(8193).read_to_end(&mut data).map(|_|data)});
 let status=match child.wait_timeout(Duration::from_secs(20))?{Some(s)=>s,None=>{child.kill()?;let _=child.wait();bail!("GIT_MIRROR_TIMEOUT");}};
 let data=reader.join().map_err(|_|anyhow!("GIT_IO_PANIC"))??;
 if data.len()>8192{bail!("GIT_MIRROR_OUTPUT_LIMIT");}
 if !status.success(){if allow_missing && status.code()==Some(1) {return Ok(None);}bail!("GIT_MIRROR_OPERATION_FAILED");}
 Ok(Some(String::from_utf8(data)?.trim().to_string()))
}
#[derive(Clone,Debug,Serialize)]
pub struct MirrorStatus {pub repository_id:String,pub source_ref:String,pub native_id:String,
 pub native_generation:i64,pub mirror_ref:String,pub git_oid:String,pub mirrored_generation:i64,pub mirror_lag:i64}
#[derive(Clone)]pub struct GatewayService {pub registry:Registry}
impl GatewayService {
 pub fn new(registry:Registry)->Self{Self{registry}}
 fn mirror_ref(source:&str)->Result<String>{
  let suffix=source.strip_prefix("refs/heads/").ok_or_else(||anyhow!("INVALID_NATIVE_REF"))?;
  if suffix.is_empty()||suffix.len()>128||suffix.contains("..")||suffix.contains("//")||!suffix.bytes().all(|x|x.is_ascii_alphanumeric()||b"/_.-".contains(&x)){bail!("INVALID_NATIVE_REF");}
  Ok(format!("refs/openscion/mirror/{suffix}"))
 }
 /// Reverify the mapping against Git, then advance ONLY the derived mirror ref.
 /// This is usable for imported Git history. Newly authored native Changesets
 /// are intentionally rejected pending a complete, qualified Git exporter.
 pub fn sync_verified(&self,who:&Principal,repo_id:&str,ref_name:&str)->Result<MirrorStatus>{
  Registry::require_admin(who)?;
  let authority=AuthorityService::new(self.registry.clone()).read(who,repo_id)?;
  if authority.mode!=AuthorityMode::NativeAuthorityPreview{bail!("NATIVE_AUTHORITY_NOT_ENABLED");}
  let snap=NativeRefService::new(self.registry.clone()).read(who,repo_id,ref_name)?;
  if authority.epoch!=snap.authority_epoch{bail!("AUTHORITY_FENCED");}
  let c=db(&self.registry)?;
  let mapping:Option<String>=c.query_row("SELECT git_oid FROM shadow_mappings WHERE repo_id=?1 AND native_id=?2 AND state='VERIFIED'",params![repo_id,snap.target_changeset_id],|r|r.get(0)).optional()?;
  let oid=mapping.ok_or_else(||anyhow!("GIT_EXPORT_INELIGIBLE"))?;
  let checked=ShadowService::new(self.registry.clone()).verify_mapping(who,repo_id,&oid)?;
  if checked.native_id!=snap.target_changeset_id{bail!("GIT_NATIVE_PARITY_MISMATCH");}
  let repo=self.registry.get_authorized(repo_id,who)?.ok_or_else(||anyhow!("REPOSITORY_NOT_FOUND"))?;
  let git=GitRepository::open(&repo.path)?;
  let candidate=git.read_commit(&oid)?.oid;
  let expected=git.validate_oid(&candidate)?;
  let mr=Self::mirror_ref(ref_name)?;
  let prior=run(&repo.path,&["show-ref","--verify","--hash",&mr],true)?;
  let zeros="0".repeat(git.algorithm.hex_len());
  let previous=prior.as_deref().unwrap_or(&zeros);
  if prior.as_deref()!=Some(expected.as_str()) {
   run(&repo.path,&["update-ref","--no-deref",&mr,&expected,previous],false)?;
  }
  // A failed SQLite publication is recoverable by retrying this exact mirror
  // operation and comparing the already written Git Ref.
  c.execute("INSERT INTO git_export_mappings_v6(repo_id,native_id,git_oid,exporter_version,state,verified_at) VALUES (?1,?2,?3,1,'VERIFIED',?4) ON CONFLICT(repo_id,native_id,exporter_version) DO UPDATE SET git_oid=excluded.git_oid,state='VERIFIED',verified_at=excluded.verified_at",params![repo_id,snap.target_changeset_id,candidate,now()])?;
  c.execute("INSERT INTO git_mirror_observations_v6(repo_id,ref_name,native_id,native_generation,git_oid,mirrored_generation,verified_at) VALUES (?1,?2,?3,?4,?5,?4,?6) ON CONFLICT(repo_id,ref_name) DO UPDATE SET native_id=excluded.native_id,native_generation=excluded.native_generation,git_oid=excluded.git_oid,mirrored_generation=excluded.mirrored_generation,verified_at=excluded.verified_at WHERE excluded.mirrored_generation>=git_mirror_observations_v6.mirrored_generation",params![repo_id,ref_name,snap.target_changeset_id,snap.generation,candidate,now()])?;
  self.status(who,repo_id,ref_name)
 }
 /// Recoverable native->Git fallback. Phase 1 closes native writes in a
 /// SQLite epoch transition. Phase 2 conditionally advances refs/heads/main.
 /// Phase 3 returns the authority to Git. Re-running after interruption is safe.
 pub fn fallback_to_git(&self,who:&Principal,repo_id:&str,expected_epoch:i64,external_writers_fenced:bool)->Result<openscion_authority::Authority>{
  Registry::require_admin(who)?;
  if !external_writers_fenced {bail!("EXTERNAL_WRITER_FENCING_REQUIRED");}
  if repo_id.len()>128 || !repo_id.bytes().all(|x|x.is_ascii_hexdigit()||x==b'-'){bail!("INVALID_REPOSITORY_ID");}
  let base=self.registry.db_path().parent().ok_or_else(||anyhow!("SERVICE_PATH"))?.join("locks");
  fs::create_dir_all(&base)?;
  let lock=OpenOptions::new().create(true).write(true).read(true).open(base.join(format!("land-{repo_id}.lock")))?;
  lock.lock_exclusive()?;
  let authority=AuthorityService::new(self.registry.clone());
  let observed=authority.read(who,repo_id)?;
  if observed.epoch!=expected_epoch {bail!("AUTHORITY_FENCED");}
  let checkpoint_git=if observed.mode==AuthorityMode::NativeAuthorityPreview {
    let snap=NativeRefService::new(self.registry.clone()).read(who,repo_id,"refs/heads/main")?;
    let mirror=self.status(who,repo_id,"refs/heads/main")?;
    if mirror.mirror_lag!=0 || mirror.native_id!=snap.target_changeset_id {bail!("ROLLBACK_BLOCKED_BY_UNEXPORTED_COMMITS");}
    let mapping=ShadowService::new(self.registry.clone()).verify_mapping(who,repo_id,&mirror.git_oid)?;
    if mapping.native_id!=snap.target_changeset_id {bail!("GIT_NATIVE_PARITY_MISMATCH");}
    let mut c=db(&self.registry)?;
    let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let n=tx.execute("UPDATE repository_authority_v6 SET mode='FALLBACK_FROZEN',epoch=epoch+1,checkpoint_git_oid=?3,checkpoint_native_id=?4,updated_at=?5 WHERE repo_id=?1 AND mode='NATIVE_AUTHORITY_PREVIEW' AND epoch=?2",params![repo_id,expected_epoch,mirror.git_oid,snap.target_changeset_id,now()])?;
    if n!=1 {bail!("AUTHORITY_FENCED");}
    tx.execute("INSERT INTO authority_transitions_v6(id,repo_id,actor,from_mode,to_mode,from_epoch,to_epoch,git_checkpoint,native_checkpoint,note,created_at) VALUES (?1,?2,?3,'NATIVE_AUTHORITY_PREVIEW','FALLBACK_FROZEN',?4,?5,?6,?7,'fallback phase 1: native fenced',?8)",params![Uuid::new_v4().to_string(),repo_id,who.id,expected_epoch,expected_epoch+1,mirror.git_oid,snap.target_changeset_id,now()])?;
    tx.commit()?;
    mirror.git_oid
  } else if observed.mode==AuthorityMode::FallbackFrozen {
    observed.checkpoint_git_oid.ok_or_else(||anyhow!("FALLBACK_CHECKPOINT_MISSING"))?
  } else {bail!("AUTHORITY_MODE_CONFLICT");};
  let repo=self.registry.get_authorized(repo_id,who)?.ok_or_else(||anyhow!("REPOSITORY_NOT_FOUND"))?;
  let git=GitRepository::open(&repo.path)?;
  let target=git.validate_oid(&checkpoint_git)?;
  // Do not rely solely on the previously persisted mirror observation.
  let actual_mirror=run(&repo.path,&["show-ref","--verify","--hash","refs/openscion/mirror/main"],false)?.ok_or_else(||anyhow!("GIT_MIRROR_MISSING"))?;
  if actual_mirror!=target {bail!("GIT_MIRROR_TAMPERED");}

  // The last successful cutover records the source Git checkpoint. The
  // protected Git branch must not have moved independently since cutover.
  let c=db(&self.registry)?;
  let source:String=c.query_row("SELECT git_checkpoint FROM authority_transitions_v6 WHERE repo_id=?1 AND to_mode='CUTOVER_FROZEN' ORDER BY created_at DESC,id DESC LIMIT 1",[repo_id],|r|r.get(0))?;
  let baseline=git.validate_oid(&source)?;
  if target!=baseline {
    let ancestor=run(&repo.path,&["merge-base","--is-ancestor",&baseline,&target],true);
    // `--is-ancestor` produces no stdout, code 1 means not an ancestor.
    // Unlike missing-ref checks, an exit status 1 is a hard safety failure.
    match ancestor {
      Ok(Some(_))=>{},
      _=>bail!("GIT_FALLBACK_NON_FAST_FORWARD"),
    }
  }

  let current=run(&repo.path,&["rev-parse","--verify","refs/heads/main"],false)?.ok_or_else(||anyhow!("MISSING_GIT_MAIN"))?;
  if current!=target {
    if current!=baseline {bail!("SPLIT_BRAIN_GIT_MAIN_MOVED");}
    run(&repo.path,&["update-ref","--no-deref","refs/heads/main",&target,&baseline],false)?;
  }
  let mut c=db(&self.registry)?;let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
  let (mode,epoch):(String,i64)=tx.query_row("SELECT mode,epoch FROM repository_authority_v6 WHERE repo_id=?1",[repo_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
  if mode!="FALLBACK_FROZEN" {bail!("AUTHORITY_FENCED");}
  let n=tx.execute("UPDATE repository_authority_v6 SET mode='GIT_AUTHORITY',epoch=epoch+1,updated_at=?3 WHERE repo_id=?1 AND mode='FALLBACK_FROZEN' AND epoch=?2 AND checkpoint_git_oid=?4",params![repo_id,epoch,now(),checkpoint_git])?;
  if n!=1 {bail!("AUTHORITY_FENCED");}
  tx.execute("INSERT INTO authority_transitions_v6(id,repo_id,actor,from_mode,to_mode,from_epoch,to_epoch,git_checkpoint,note,created_at) VALUES (?1,?2,?3,'FALLBACK_FROZEN','GIT_AUTHORITY',?4,?5,?6,'fallback phase 3: verified Git CAS',?7)",params![Uuid::new_v4().to_string(),repo_id,who.id,epoch,epoch+1,checkpoint_git,now()])?;
  tx.commit()?;authority.read(who,repo_id)
 }
 pub fn status(&self,who:&Principal,repo_id:&str,ref_name:&str)->Result<MirrorStatus>{
  self.registry.get_authorized(repo_id,who)?.ok_or_else(||anyhow!("REPOSITORY_NOT_FOUND"))?;
  let snap=NativeRefService::new(self.registry.clone()).read(who,repo_id,ref_name)?;
  let c=db(&self.registry)?;
  let row:Option<(String,i64,String,i64)>=c.query_row("SELECT native_id,native_generation,git_oid,mirrored_generation FROM git_mirror_observations_v6 WHERE repo_id=?1 AND ref_name=?2",params![repo_id,ref_name],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
  let (_,_,git_oid,mirrored)=row.ok_or_else(||anyhow!("GIT_MIRROR_LAGGING"))?;
  Ok(MirrorStatus{repository_id:repo_id.into(),source_ref:ref_name.into(),native_id:snap.target_changeset_id,native_generation:snap.generation,mirror_ref:Self::mirror_ref(ref_name)?,git_oid,mirrored_generation:mirrored,mirror_lag:snap.generation.saturating_sub(mirrored)})
 }
}
