//! Transactional native reference CAS. V0.6 preview supports only reverified
//! imported Changesets; native authoring/export is NOT declared production-ready.
//! All authority and ref guards are checked within the SAME SQLite transaction.
use anyhow::{anyhow,bail,Result};
use openscion_authority::{AuthorityMode,AuthorityService};
use openscion_cas::FsObjectStore;
use openscion_derived::manifest_from_tree;
use openscion_native::{ChangesetId,ChangesetStore};
use openscion_registry::{Principal,Registry};
use openscion_types::ObjectId;
use rusqlite::{params,Connection,OptionalExtension,TransactionBehavior};
use serde::{Deserialize,Serialize};
use sha2::{Digest,Sha256};
use std::{collections::HashSet,str::FromStr,time::{SystemTime,UNIX_EPOCH}};
use uuid::Uuid;
fn now()->i64{SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64}
fn db(reg:&Registry)->Result<Connection>{let c=Connection::open(reg.db_path())?;c.execute_batch("PRAGMA foreign_keys=ON;PRAGMA busy_timeout=5000;PRAGMA synchronous=FULL;")?;Ok(c)}
fn valid_ref(s:&str)->bool{
 s.starts_with("refs/heads/") && s.len()<=256 && !s.contains("..") && !s.contains("//") && !s.ends_with(".lock") && !s.ends_with('.') && !s.ends_with('/')
 && s.bytes().all(|b|b.is_ascii_alphanumeric()||b"/_.-".contains(&b))
}
#[derive(Clone,Debug,Serialize,Deserialize,PartialEq,Eq)]
pub struct NativeRefSnapshot {pub repo_id:String,pub ref_name:String,pub target_changeset_id:String,pub generation:i64,pub authority_epoch:i64}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct CasRequest {
 pub repo_id:String,pub ref_name:String,pub expected_target:String,pub expected_generation:i64,
 pub authority_epoch:i64,pub new_target:String,pub idempotency_key:String,pub policy_evidence:String,
}
#[derive(Clone)]pub struct NativeRefService{pub registry:Registry}
impl NativeRefService {
 pub fn new(registry:Registry)->Self{Self{registry}}
 fn native_stores(&self,repo_id:&str)->Result<(FsObjectStore,ChangesetStore)>{
  if repo_id.len()>128 || !repo_id.bytes().all(|c|c.is_ascii_hexdigit()||c==b'-'){bail!("INVALID_REPOSITORY_ID");}
  let root=self.registry.db_path().parent().ok_or_else(||anyhow!("SERVICE_PATH"))?.join("shadow-native").join(repo_id);
  Ok((FsObjectStore::new(root.join("cas")),ChangesetStore::new(root.join("changesets"))))
 }
 /// Re-verify all reachable parents and file Blobs. This is deliberately
 /// bounded and independent of a mutable SQLite verification flag.
 fn verify_closure(&self,repo_id:&str,target:&str)->Result<()> {
  let (cas,store)=self.native_stores(repo_id)?;
  let target_id=ChangesetId::from_str(target)?;
  let mut seen=HashSet::new();let mut pending=vec![target_id];
  while let Some(id)=pending.pop(){
    if !seen.insert(id){continue;}
    if seen.len()>10_000{bail!("NATIVE_OBJECT_CLOSURE_LIMIT");}
    let cs=store.get(id).map_err(|_|anyhow!("NATIVE_OBJECT_CLOSURE_INCOMPLETE"))?;
    for change in &cs.changes {
      if let openscion_native::FileChange::Put{blob,..}=change{use openscion_cas::ObjectReader;cas.verify(*blob)?;}
    }
    pending.extend(cs.parents);
  }
  let c=db(&self.registry)?;
  let root:Option<String>=c.query_row("SELECT root_tree_id FROM shadow_mappings WHERE repo_id=?1 AND native_id=?2 AND state='VERIFIED'",params![repo_id,target],|r|r.get(0)).optional()?;
  let root=root.ok_or_else(||anyhow!("NATIVE_TARGET_NOT_VERIFIED"))?;
  manifest_from_tree(&cas,ObjectId::from_str(&root)?)?;
  Ok(())
 }
 /// Never allow an implicit rewind or unrelated-branch movement in preview.
 fn require_fast_forward(&self,repo_id:&str,old:&str,new:&str)->Result<()> {
  if old==new {bail!("NATIVE_REF_NO_EFFECT");}
  let (_,store)=self.native_stores(repo_id)?;
  let wanted=ChangesetId::from_str(old)?;
  let mut seen=HashSet::new();let mut stack=vec![ChangesetId::from_str(new)?];
  while let Some(id)=stack.pop(){
   if id==wanted{return Ok(());}
   if !seen.insert(id){continue;}
   if seen.len()>10_000{bail!("NATIVE_ANCESTRY_BUDGET");}
   stack.extend(store.get(id)?.parents);
  }
  bail!("NATIVE_NON_FAST_FORWARD");
 }
 pub fn read(&self,who:&Principal,repo_id:&str,ref_name:&str)->Result<NativeRefSnapshot>{
  self.registry.get_authorized(repo_id,who)?.ok_or_else(||anyhow!("REPOSITORY_NOT_FOUND"))?;
  if !valid_ref(ref_name){bail!("INVALID_NATIVE_REF");}
  let c=db(&self.registry)?;
  c.query_row("SELECT target_changeset_id,generation,authority_epoch FROM native_refs_v6 WHERE repo_id=?1 AND ref_name=?2",params![repo_id,ref_name],|r|Ok(NativeRefSnapshot{repo_id:repo_id.into(),ref_name:ref_name.into(),target_changeset_id:r.get(0)?,generation:r.get(1)?,authority_epoch:r.get(2)?})).optional()?.ok_or_else(||anyhow!("NATIVE_REF_NOT_FOUND"))
 }
 /// Bootstrap is legal only while CUTOVER_FROZEN, and only at the recorded
 /// reverified native checkpoint. This does not change any Git reference.
 pub fn bootstrap(&self,who:&Principal,repo_id:&str,ref_name:&str,epoch:i64)->Result<NativeRefSnapshot>{
  Registry::require_admin(who)?;if !valid_ref(ref_name){bail!("INVALID_NATIVE_REF");}
  let a=AuthorityService::new(self.registry.clone()).read(who,repo_id)?;
  if a.mode!=AuthorityMode::CutoverFrozen || a.epoch!=epoch {bail!("AUTHORITY_FENCED");}
  let target=a.checkpoint_native_id.ok_or_else(||anyhow!("CHECKPOINT_MISSING"))?;
  self.verify_closure(repo_id,&target)?;
  let mut c=db(&self.registry)?;let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
  let matches:i64=tx.query_row("SELECT COUNT(*) FROM repository_authority_v6 WHERE repo_id=?1 AND mode='CUTOVER_FROZEN' AND epoch=?2 AND checkpoint_native_id=?3",params![repo_id,epoch,target],|r|r.get(0))?;
  if matches!=1{bail!("AUTHORITY_FENCED");}
  tx.execute("INSERT INTO native_refs_v6(repo_id,ref_name,target_changeset_id,generation,authority_epoch,updated_at) VALUES (?1,?2,?3,0,?4,?5) ON CONFLICT(repo_id,ref_name) DO NOTHING",params![repo_id,ref_name,target,epoch,now()])?;
  let stored:String=tx.query_row("SELECT target_changeset_id FROM native_refs_v6 WHERE repo_id=?1 AND ref_name=?2",params![repo_id,ref_name],|r|r.get(0))?;
  if stored!=target{bail!("NATIVE_BOOTSTRAP_CONFLICT");}
  tx.commit()?;self.read(who,repo_id,ref_name)
 }
 /// Idempotent Ref CAS; generation detects the ABA case. The verified native
 /// target must already exist durably before the SQLite transaction begins.
 pub fn cas(&self,who:&Principal,req:CasRequest)->Result<NativeRefSnapshot>{
  if who.role!="admin" {bail!("PERMISSION_DENIED");} // preview admin-only
  if !valid_ref(&req.ref_name)||req.idempotency_key.len()>128||req.idempotency_key.is_empty(){bail!("INVALID_NATIVE_CAS_REQUEST");}
  if req.policy_evidence.len()<8||req.policy_evidence.len()>512 {bail!("POLICY_EVIDENCE_REQUIRED");}
  self.registry.get_authorized(&req.repo_id,who)?.ok_or_else(||anyhow!("REPOSITORY_NOT_FOUND"))?;
  let _=ChangesetId::from_str(&req.expected_target)?;
  self.verify_closure(&req.repo_id,&req.new_target)?;
  self.require_fast_forward(&req.repo_id,&req.expected_target,&req.new_target)?;
  let raw=serde_json::to_vec(&req)?;
  let fp=hex::encode(Sha256::digest(&raw));
  let mut c=db(&self.registry)?;let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
  let existing:Option<(String,i64,String)>=tx.query_row("SELECT request_fingerprint,result_generation,target_changeset_id FROM native_ref_updates_v6 WHERE repo_id=?1 AND ref_name=?2 AND id=?3",params![req.repo_id,req.ref_name,req.idempotency_key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
  if let Some((old_fp,generation,target))=existing {
    if old_fp!=fp {bail!("IDEMPOTENCY_CONFLICT");}
    tx.commit()?;
    return Ok(NativeRefSnapshot{repo_id:req.repo_id,ref_name:req.ref_name,target_changeset_id:target,generation,authority_epoch:req.authority_epoch});
  }
  let mode_epoch:Option<(String,i64)>=tx.query_row("SELECT mode,epoch FROM repository_authority_v6 WHERE repo_id=?1",[&req.repo_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
  if mode_epoch!=Some(("NATIVE_AUTHORITY_PREVIEW".into(),req.authority_epoch)){bail!("AUTHORITY_FENCED");}
  let enabled:String=tx.query_row("SELECT value FROM release_capabilities_v6 WHERE key='native_authority_preview'",[],|r|r.get(0))?;
  if enabled!="enabled" {bail!("FEATURE_NOT_QUALIFIED");}
  let n=tx.execute("UPDATE native_refs_v6 SET target_changeset_id=?1,generation=generation+1,updated_at=?2 WHERE repo_id=?3 AND ref_name=?4 AND target_changeset_id=?5 AND generation=?6 AND authority_epoch=?7",params![req.new_target,now(),req.repo_id,req.ref_name,req.expected_target,req.expected_generation,req.authority_epoch])?;
  if n!=1{bail!("NATIVE_REF_CAS_CONFLICT");}
  let result_gen=req.expected_generation.checked_add(1).ok_or_else(||anyhow!("GENERATION_OVERFLOW"))?;
  tx.execute("INSERT INTO native_ref_updates_v6(id,repo_id,ref_name,actor,request_fingerprint,expected_target,expected_generation,target_changeset_id,authority_epoch,result_generation,policy_evidence,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![req.idempotency_key,req.repo_id,req.ref_name,who.id,fp,req.expected_target,req.expected_generation,req.new_target,req.authority_epoch,result_gen,req.policy_evidence,now()])?;
  let payload=serde_json::json!({"repo_id":req.repo_id,"ref_name":req.ref_name,"native_id":req.new_target,"generation":result_gen,"epoch":req.authority_epoch}).to_string();
  tx.execute("INSERT INTO change_outbox(id,event_type,resource_id,payload,created_at) VALUES (?1,'native_ref.updated',?2,?3,?4)",params![Uuid::new_v4().to_string(),req.idempotency_key,payload,now()])?;
  tx.commit()?;
  Ok(NativeRefSnapshot{repo_id:req.repo_id,ref_name:req.ref_name,target_changeset_id:req.new_target,generation:result_gen,authority_epoch:req.authority_epoch})
 }
}
