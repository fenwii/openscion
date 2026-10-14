//! OpenScion V0.4: Git-authoritative integration, immutable candidate evidence,
//! protected-ref CAS, and conservative Git/SQLite reconciliation.
//!
//! SECURITY: trusted registered Git repositories only. `git-tree-valid` verifies
//! object integrity, NOT application correctness; production CI/sandbox runners
//! must be added before enforcing application tests.
use anyhow::{anyhow,bail,Context,Result};
use fs2::FileExt;
use openscion_change::{db,now,ChangeRequest,ChangeService};
use openscion_git::GitRepository;
use openscion_policy::{valid_target,Policy};
use openscion_registry::{Principal,Registry,RepoRecord};
use openscion_validation::verified_tree;
use rusqlite::{params,OptionalExtension};
use serde::{Deserialize,Serialize};
use sha2::{Digest,Sha256};
use std::{fs::{self,OpenOptions},io::{Read,Write},path::Path,process::{Command,Stdio},thread,time::Duration};
use uuid::Uuid;
use wait_timeout::ChildExt;

const TIMEOUT:Duration=Duration::from_secs(60);
const OUTPUT_LIMIT:usize=1_048_576;
fn oid_plain(s:&str)->&str{s.rsplit(':').next().unwrap_or(s)}
fn valid_oid(oid:&str,len:usize)->bool{oid.len()==len && oid.bytes().all(|x|x.is_ascii_hexdigit())}
fn fixed_git(repo:&Path,args:&[String],stdin:Option<&[u8]>,allow_conflict:bool)->Result<(i32,Vec<u8>)>{
 let mut cmd=Command::new("git");
 cmd.env_clear().env("PATH",std::env::var("PATH").unwrap_or_else(|_|"/usr/bin:/bin".into()))
   .env("GIT_CONFIG_NOSYSTEM","1").env("GIT_CONFIG_GLOBAL","/dev/null")
   .env("GIT_NO_REPLACE_OBJECTS","1").env("GIT_NO_LAZY_FETCH","1")
   .env("GIT_TERMINAL_PROMPT","0").env("GIT_LFS_SKIP_SMUDGE","1")
   .env("GIT_AUTHOR_NAME","OpenScion Integration")
   .env("GIT_AUTHOR_EMAIL","integration@openscion.invalid")
   .env("GIT_COMMITTER_NAME","OpenScion Integration")
   .env("GIT_COMMITTER_EMAIL","integration@openscion.invalid")
   .arg("--no-pager").arg("-C").arg(repo)
   .arg("-c").arg("core.hooksPath=/dev/null")
   .arg("-c").arg("commit.gpgsign=false")
   .arg("-c").arg("core.fsmonitor=false")
   .args(args)
   .stdin(if stdin.is_some(){Stdio::piped()}else{Stdio::null()})
   .stdout(Stdio::piped()).stderr(Stdio::piped());
 let mut child=cmd.spawn().context("GIT_SPAWN_FAILED")?;
 if let Some(bytes)=stdin {child.stdin.take().ok_or_else(||anyhow!("GIT_STDIN"))?.write_all(bytes)?;}
 let mut out=child.stdout.take().ok_or_else(||anyhow!("GIT_STDOUT"))?;
 let mut err=child.stderr.take().ok_or_else(||anyhow!("GIT_STDERR"))?;
 let a=thread::spawn(move||{let mut b=Vec::new();let r=out.take((OUTPUT_LIMIT+1) as u64).read_to_end(&mut b);(r,b)});
 let b=thread::spawn(move||{let mut x=Vec::new();let r=err.take(65537).read_to_end(&mut x);(r,x)});
 let status=match child.wait_timeout(TIMEOUT)?{Some(s)=>s,None=>{let _=child.kill();let _=child.wait();bail!("GIT_TIMEOUT");}};
 let (ra,output)=a.join().map_err(|_|anyhow!("GIT_STDOUT_JOIN"))?;
 let (rb,errors)=b.join().map_err(|_|anyhow!("GIT_STDERR_JOIN"))?;
 ra?;rb?;
 if output.len()>OUTPUT_LIMIT || errors.len()>65536 {bail!("GIT_OUTPUT_LIMIT");}
 let status_code=status.code().unwrap_or(-1);
 if status_code!=0 && !(allow_conflict && status_code==1) {bail!("GIT_OPERATION_FAILED:{status_code}");}
 Ok((status_code,output))
}
fn git_text(repo:&Path,args:&[String])->Result<String>{
 let (_,bytes)=fixed_git(repo,args,None,false)?;
 Ok(String::from_utf8(bytes)?.trim().to_string())
}
fn ref_value(repo:&Path,reference:&str)->Result<String>{
 let value=git_text(repo,&["rev-parse".into(),"--verify".into(),format!("{reference}^{{commit}}")])?;
 Ok(value)
}
fn verified_object(repo:&Path,oid:&str,object_kind:&str)->Result<()> {
 let resolved=git_text(repo,&["cat-file".into(),"-t".into(),oid.into()])?;
 if resolved!=object_kind {bail!("GIT_OBJECT_KIND_MISMATCH");}
 Ok(())
}
fn has_tree(repo:&Path,oid:&str)->Result<String>{git_text(repo,&["show".into(),"-s".into(),"--format=%T".into(),oid.into()])}
fn first_parent(repo:&Path,oid:&str)->Result<String>{git_text(repo,&["rev-parse".into(),format!("{oid}^1")])}
fn lock_repo(reg:&Registry,id:&str)->Result<std::fs::File>{
 if id.len()>128 || !id.bytes().all(|x|x.is_ascii_hexdigit()||x==b'-'){bail!("INVALID_REPOSITORY_ID");}
 let dir=reg.db_path().parent().ok_or_else(||anyhow!("REGISTRY_PATH"))?.join("locks");fs::create_dir_all(&dir)?;
 let file=OpenOptions::new().create(true).read(true).write(true).open(dir.join(format!("land-{id}.lock")))?;
 file.lock_exclusive()?;Ok(file)
}
fn fingerprint<T:Serialize>(v:&T)->Result<String>{let mut h=Sha256::new();h.update(b"openscion/land-request/v1\0");h.update(serde_json::to_vec(v)?);Ok(hex::encode(h.finalize()))}
fn event(reg:&Registry,kind:&str,id:&str,payload:&str)->Result<()> {
 let c=db(reg)?;
 c.execute("INSERT INTO change_outbox(id,event_type,resource_id,payload,created_at) VALUES (?1,?2,?3,?4,?5)",params![Uuid::new_v4().to_string(),kind,id,payload,now()])?;
 Ok(())
}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct Operation {
 pub id:String,pub change_id:String,pub actor:String,pub state:String,
 pub attempt:i64,pub result_oid:Option<String>,pub error_code:Option<String>,
 pub observed_target:Option<String>,pub candidate_tree:Option<String>,pub candidate_oid:Option<String>,pub policy_digest:Option<String>,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct PreviewRequest {pub change_id:String,pub idempotency_key:String}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct ReviewRequest {pub decision:String}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct TrustedAttestation {
 pub check_name:String,pub candidate_oid:String,pub tree_oid:String,
 pub suite_digest:String,pub runner_digest:String,pub passed:bool,
}
#[derive(Clone)]pub struct LandService {pub registry:Registry}
impl LandService {
 pub fn new(registry:Registry)->Self{Self{registry}}
 fn change(&self,who:&Principal,change_id:&str,permission:&str)->Result<(ChangeRequest,RepoRecord,Policy,String)> {
   let ch=ChangeService::new(self.registry.clone()).get(who,change_id)?;
   if !self.registry.change_allowed(who,&ch.repo_id,permission)?{bail!("PERMISSION_DENIED");}
   let repo=self.registry.get_authorized(&ch.repo_id,who)?.ok_or_else(||anyhow!("REPOSITORY_NOT_FOUND"))?;
   let (pol,digest)=self.policy_for(&ch.repo_id,&ch.target_ref)?;
   Ok((ch,repo,pol,digest))
 }
 pub fn install_policy(&self,repo_name:&str,policy:Policy)->Result<String>{
   let digest=policy.digest()?;
   let c=db(&self.registry)?;
   let id:String=c.query_row("SELECT id FROM git_repositories WHERE name=?1",[repo_name],|r|r.get(0))?;
   c.execute("INSERT INTO change_policies(repo_id,target_ref,policy_json,policy_digest,updated_at) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(repo_id,target_ref) DO UPDATE SET policy_json=excluded.policy_json,policy_digest=excluded.policy_digest,updated_at=excluded.updated_at",params![id,policy.target_ref,serde_json::to_string(&policy)?,digest,now()])?;
   Ok(digest)
 }
 fn policy_for(&self,repo_id:&str,target:&str)->Result<(Policy,String)> {
   if !valid_target(target){bail!("INVALID_TARGET_REF");}
   let c=db(&self.registry)?;
   let entry:Option<(String,String)>=c.query_row("SELECT policy_json,policy_digest FROM change_policies WHERE repo_id=?1 AND target_ref=?2",params![repo_id,target],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
   let (encoded,digest)=entry.ok_or_else(||anyhow!("POLICY_NOT_CONFIGURED"))?;
   let p:Policy=serde_json::from_str(&encoded)?;
   if p.target_ref!=target || p.digest()?!=digest {bail!("POLICY_INTEGRITY_FAILURE");}
   Ok((p,digest))
 }
 fn verify_source(&self,ch:&ChangeRequest,repo:&RepoRecord)->Result<()> {
   let g=GitRepository::open(&repo.path)?;
   let oid=g.validate_oid(&ch.source_oid)?;
   verified_object(&repo.path,&oid,"commit")?;
   let retained=ref_value(&repo.path,&format!("refs/openscion/workspaces/{}",ch.workspace_id))?;
   if retained!=oid {bail!("CHANGE_SOURCE_UNRETAINED");}
   Ok(())
 }
 pub fn get(&self,who:&Principal,id:&str)->Result<Operation>{
   let c=db(&self.registry)?;
   let base:(String,String,String,String,i64,Option<String>,Option<String>)=c.query_row("SELECT id,change_id,actor,state,attempt,result_oid,error_code FROM land_operations WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?.ok_or_else(||anyhow!("LAND_NOT_FOUND"))?;
   let _=ChangeService::new(self.registry.clone()).get(who,&base.1)?;
   let a:Option<(String,Option<String>,Option<String>,String)>=c.query_row("SELECT observed_target,candidate_tree,candidate_commit,policy_digest FROM land_attempts WHERE operation_id=?1 AND attempt=?2",params![id,base.4],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
   let (observed,tree,candidate,policy)=match a{Some((x,y,z,p))=>(Some(x),y,z,Some(p)),None=>(None,None,None,None)};
   Ok(Operation{id:base.0,change_id:base.1,actor:base.2,state:base.3,attempt:base.4,result_oid:base.5,error_code:base.6,observed_target:observed,candidate_tree:tree,candidate_oid:candidate,policy_digest:policy})
 }
 pub fn start(&self,who:&Principal,request:PreviewRequest)->Result<Operation>{
   let (ch,repo,_,_)=self.change(who,&request.change_id,"preview")?;
   if request.idempotency_key.is_empty()||request.idempotency_key.len()>128{bail!("INVALID_IDEMPOTENCY_KEY");}
   self.verify_source(&ch,&repo)?;
   let fp=fingerprint(&request)?;let c=db(&self.registry)?;
   let previous:Option<(String,String)>=c.query_row("SELECT id,request_fingerprint FROM land_operations WHERE change_id=?1 AND actor=?2 AND idem_key=?3",params![ch.id,who.id,request.idempotency_key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
   if let Some((id,old))=previous {if old!=fp{bail!("LAND_IDEMPOTENCY_CONFLICT");}return self.get(who,&id);}
   let id=Uuid::new_v4().to_string();
   c.execute("INSERT INTO land_operations(id,change_id,actor,idem_key,request_fingerprint,state,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,'RECORDED',?6,?6)",params![id,ch.id,who.id,request.idempotency_key,fp,now()])?;
   drop(c);
   self.build_attempt(who,&id)
 }
 fn build_attempt(&self,who:&Principal,operation_id:&str)->Result<Operation>{
   let operation=self.get(who,operation_id)?;
   if operation.state=="MERGED" {return Ok(operation);}
   let (ch,repo,policy,digest)=self.change(who,&operation.change_id,"preview")?;
   self.verify_source(&ch,&repo)?;
   let _guard=lock_repo(&self.registry,&repo.id)?;
   // V0.6: mode check happens under the same OS lock as operator freeze.
   self.registry.require_git_authority(&repo.id)?;
   let observed=ref_value(&repo.path,&ch.target_ref)?;
   let source=oid_plain(&ch.source_oid);
   let c=db(&self.registry)?;
   let attempt=operation.attempt+1;
   if attempt>i64::from(policy.max_attempts){bail!("LAND_RETRY_EXHAUSTED");}
   c.execute("INSERT INTO land_attempts(operation_id,attempt,observed_target,source_oid,policy_digest,state) VALUES (?1,?2,?3,?4,?5,'PREVIEWING')",params![operation_id,attempt,observed,source,digest])?;
   c.execute("UPDATE land_operations SET state='PREVIEWING',attempt=?2,updated_at=?3,error_code=NULL WHERE id=?1",params![operation_id,attempt,now()])?;
   drop(c);
   let (status,output)=fixed_git(&repo.path,&["merge-tree".into(),"--write-tree".into(),"-z".into(),observed.clone(),source.into()],None,true)?;
   if status==1 {
     let c=db(&self.registry)?;
     c.execute("UPDATE land_attempts SET state='CONFLICT' WHERE operation_id=?1 AND attempt=?2",params![operation_id,attempt])?;
     c.execute("UPDATE land_operations SET state='CONFLICT',error_code='CHANGE_MERGE_CONFLICT',updated_at=?2 WHERE id=?1",params![operation_id,now()])?;
     return self.get(who,operation_id);
   }
   let first=output.split(|x|*x==0).next().ok_or_else(||anyhow!("INVALID_MERGE_OUTPUT"))?;
   let tree=std::str::from_utf8(first)?.trim().to_string();
   if !valid_oid(&tree,observed.len()){bail!("INVALID_MERGE_TREE");}
   verified_object(&repo.path,&tree,"tree")?;
   let previous_tree=has_tree(&repo.path,&observed)?;
   if tree==previous_tree {
     let c=db(&self.registry)?;
     c.execute("UPDATE land_attempts SET state='NO_EFFECT',candidate_tree=?3 WHERE operation_id=?1 AND attempt=?2",params![operation_id,attempt,tree])?;
     c.execute("UPDATE land_operations SET state='NO_EFFECT',error_code='CHANGE_NO_EFFECT',updated_at=?2 WHERE id=?1",params![operation_id,now()])?;
     return self.get(who,operation_id);
   }
   let message=format!("Integrate ChangeRequest {}\n\nSource-Commit: {}\n",ch.id,source);
   let candidate=git_text_with_input(&repo.path,&["commit-tree".into(),tree.clone(),"-p".into(),observed.clone()],message.as_bytes())?;
   if !valid_oid(&candidate,observed.len()) || first_parent(&repo.path,&candidate)?!=observed || has_tree(&repo.path,&candidate)?!=tree {bail!("CANDIDATE_INVARIANT_FAILED");}
   let reference=format!("refs/openscion/land/{operation_id}-{attempt}");
   let zeros="0".repeat(candidate.len());
   fixed_git(&repo.path,&["update-ref".into(),reference.clone(),candidate.clone(),zeros],None,false)?;
   let c=db(&self.registry)?;
   c.execute("UPDATE land_attempts SET candidate_tree=?3,candidate_commit=?4,candidate_ref=?5,state='CANDIDATE_READY' WHERE operation_id=?1 AND attempt=?2",params![operation_id,attempt,tree,candidate,reference])?;
   c.execute("UPDATE land_operations SET state='CANDIDATE_READY',updated_at=?2 WHERE id=?1",params![operation_id,now()])?;
   let _=event(&self.registry,"candidate.created",operation_id,"{}");
   self.get(who,operation_id)
 }
 pub fn validate(&self,who:&Principal,id:&str)->Result<Operation>{
   let op=self.get(who,id)?;let (ch,repo,policy,digest)=self.change(who,&op.change_id,"validate")?;
   if op.policy_digest.as_deref()!=Some(digest.as_str()) {bail!("POLICY_CHANGED_REPREVIEW_REQUIRED");}
   if op.state=="VALIDATED" || op.state=="APPROVED" {return Ok(op);}
   if op.state!="CANDIDATE_READY" && op.state!="VALIDATING" {bail!("CHANGE_INVALID_STATE");}
   let candidate=op.candidate_oid.clone().ok_or_else(||anyhow!("CANDIDATE_MISSING"))?;
   let tree=op.candidate_tree.clone().ok_or_else(||anyhow!("CANDIDATE_TREE_MISSING"))?;
   verified_object(&repo.path,&candidate,"commit")?;
   verified_object(&repo.path,&tree,"tree")?;
   if has_tree(&repo.path,&candidate)?!=tree || first_parent(&repo.path,&candidate)?!=op.observed_target.clone().unwrap_or_default() {bail!("CANDIDATE_INVARIANT_FAILED");}
   let c=db(&self.registry)?;
   for check in &policy.required_checks {
      if check=="git-tree-valid" {
         let proof=verified_tree(&candidate,&tree,&digest)?;
         c.execute("INSERT OR IGNORE INTO validation_runs(id,operation_id,attempt,candidate_oid,tree_oid,check_name,suite_digest,runner_digest,policy_digest,principal_id,state,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,NULL,'PASSED',?10)",params![Uuid::new_v4().to_string(),id,op.attempt,candidate,tree,proof.check_name,proof.suite_digest,proof.runner_digest,digest,now()])?;
      }
   }
   drop(c);
   // Other checks require evidence submitted by a separate authorized trusted validator.
   // Do not mark the operation fully validated until every required check is PASSED.
   let c=db(&self.registry)?;
   let mut complete=true;
   for check in &policy.required_checks {
      let n:i64=c.query_row("SELECT COUNT(*) FROM validation_runs WHERE operation_id=?1 AND attempt=?2 AND check_name=?3 AND candidate_oid=?4 AND tree_oid=?5 AND policy_digest=?6 AND state='PASSED'",params![id,op.attempt,check,candidate,tree,digest],|r|r.get(0))?;
      if n==0 {complete=false;break;}
   }
   c.execute("UPDATE land_operations SET state=?2,updated_at=?3 WHERE id=?1",params![id,if complete{"VALIDATED"}else{"VALIDATING"},now()])?;
   let _=event(&self.registry,"validation.completed",id,"{}");
   let _=ch;
   self.get(who,id)
 }
 /// Accepts *claims* from an authenticated trusted external runner. It does NOT execute tests.
 /// Admins or principals explicitly granted `attest` may submit candidate-bound evidence.
 pub fn attest(&self,who:&Principal,id:&str,proof:TrustedAttestation)->Result<Operation>{
   let op=self.get(who,id)?;
   let (_,_,policy,digest)=self.change(who,&op.change_id,"attest")?;
   if who.role!="validator" && who.role!="admin" {bail!("RUNNER_UNTRUSTED");}
   if !["CANDIDATE_READY","VALIDATING","VALIDATED"].contains(&op.state.as_str()) {bail!("CHANGE_INVALID_STATE");}
   if proof.check_name=="git-tree-valid" || !policy.required_checks.contains(&proof.check_name) {bail!("INVALID_VALIDATION_CHECK");}
   if proof.candidate_oid!=op.candidate_oid.clone().unwrap_or_default() || proof.tree_oid!=op.candidate_tree.clone().unwrap_or_default() || op.policy_digest.as_deref()!=Some(digest.as_str()) {bail!("VALIDATION_BINDING_MISMATCH");}
   if ![&proof.suite_digest,&proof.runner_digest].iter().all(|s|s.len()==64 && s.bytes().all(|c|c.is_ascii_hexdigit())) {bail!("INVALID_EVIDENCE_DIGEST");}
   let c=db(&self.registry)?;
   c.execute("INSERT INTO validation_runs(id,operation_id,attempt,candidate_oid,tree_oid,check_name,suite_digest,runner_digest,policy_digest,principal_id,state,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![Uuid::new_v4().to_string(),id,op.attempt,proof.candidate_oid,proof.tree_oid,proof.check_name,proof.suite_digest,proof.runner_digest,digest,who.id,if proof.passed{"PASSED"}else{"FAILED"},now()])?;
   drop(c);
   let _=event(&self.registry,"validation.attested",id,"{}");
   // Recompute aggregate validation state including this fresh evidence.
   self.validate_after_attestation(who,id)
 }
 fn validate_after_attestation(&self,who:&Principal,id:&str)->Result<Operation>{
   let op=self.get(who,id)?;
   let (_,_,policy,digest)=self.change(who,&op.change_id,"attest")?;
   let c=db(&self.registry)?;let mut all=true;
   for check in &policy.required_checks {
      let n:i64=c.query_row("SELECT COUNT(*) FROM validation_runs WHERE operation_id=?1 AND attempt=?2 AND candidate_oid=?3 AND tree_oid=?4 AND policy_digest=?5 AND check_name=?6 AND state='PASSED'",params![id,op.attempt,op.candidate_oid,op.candidate_tree,digest,check],|r|r.get(0))?;
      if n<1 {all=false;break;}
   }
   c.execute("UPDATE land_operations SET state=?2,updated_at=?3 WHERE id=?1",params![id,if all{"VALIDATED"}else{"VALIDATING"},now()])?;
   self.get(who,id)
 }
 pub fn review(&self,who:&Principal,id:&str,decision:&str)->Result<Operation>{
   let op=self.get(who,id)?;
   let (ch,_,policy,digest)=self.change(who,&op.change_id,"review")?;
   if op.policy_digest.as_deref()!=Some(digest.as_str()) {bail!("POLICY_CHANGED_REPREVIEW_REQUIRED");}
   if op.state!="VALIDATED" && op.state!="APPROVED" {bail!("CHANGE_INVALID_STATE");}
   if !["APPROVE","REJECT"].contains(&decision){bail!("INVALID_REVIEW_DECISION");}
   if policy.independent_review && who.id==ch.author {bail!("INDEPENDENT_REVIEW_REQUIRED");}
   let candidate=op.candidate_oid.clone().ok_or_else(||anyhow!("CANDIDATE_MISSING"))?;
   let tree=op.candidate_tree.clone().ok_or_else(||anyhow!("TREE_MISSING"))?;
   let c=db(&self.registry)?;
   c.execute("INSERT INTO review_decisions(id,change_id,operation_id,attempt,candidate_oid,tree_oid,policy_digest,reviewer,decision,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(operation_id,attempt,reviewer) DO UPDATE SET decision=excluded.decision,created_at=excluded.created_at",params![Uuid::new_v4().to_string(),ch.id,id,op.attempt,candidate,tree,digest,who.id,decision,now()])?;
   if decision=="REJECT" {c.execute("UPDATE land_operations SET state='REJECTED',updated_at=?2 WHERE id=?1",params![id,now()])?;}
   let _=event(&self.registry,"review.recorded",id,"{}");
   self.get(who,id)
 }
 fn evidence_ok(&self,op:&Operation,policy:&Policy,digest:&str)->Result<()> {
   let c=db(&self.registry)?;
   let candidate=op.candidate_oid.clone().ok_or_else(||anyhow!("CANDIDATE_MISSING"))?;
   let tree=op.candidate_tree.clone().ok_or_else(||anyhow!("TREE_MISSING"))?;
   for check in &policy.required_checks {
     let count:i64=c.query_row("SELECT COUNT(*) FROM validation_runs WHERE operation_id=?1 AND attempt=?2 AND candidate_oid=?3 AND tree_oid=?4 AND check_name=?5 AND policy_digest=?6 AND state='PASSED'",params![op.id,op.attempt,candidate,tree,check,digest],|r|r.get(0))?;
     if count<1 {bail!("CHANGE_VALIDATION_REQUIRED");}
   }
   let approved:i64=c.query_row("SELECT COUNT(*) FROM review_decisions WHERE operation_id=?1 AND attempt=?2 AND candidate_oid=?3 AND tree_oid=?4 AND policy_digest=?5 AND decision='APPROVE'",params![op.id,op.attempt,candidate,tree,digest],|r|r.get(0))?;
   if approved<i64::from(policy.required_approvals){bail!("CHANGE_APPROVAL_REQUIRED");}
   let rejected:i64=c.query_row("SELECT COUNT(*) FROM review_decisions WHERE operation_id=?1 AND attempt=?2 AND decision='REJECT'",params![op.id,op.attempt],|r|r.get(0))?;
   if rejected>0{bail!("CHANGE_REJECTED");}
   Ok(())
 }
 pub fn land(&self,who:&Principal,id:&str)->Result<Operation>{
   let op=self.get(who,id)?;
   let (ch,repo,policy,digest)=self.change(who,&op.change_id,"land")?;
   if op.state=="MERGED" {return Ok(op);}
   if op.state=="CAS_IN_FLIGHT" || op.state=="RECOVERY_REQUIRED" {return self.reconcile(who,id);}
   if op.policy_digest.as_deref()!=Some(digest.as_str()) {bail!("POLICY_CHANGED_REPREVIEW_REQUIRED");}
   if op.state!="VALIDATED" && op.state!="APPROVED" {bail!("CHANGE_VALIDATION_REQUIRED");}
   self.evidence_ok(&op,&policy,&digest)?;
   let _guard=lock_repo(&self.registry,&repo.id)?;
   // V0.6: mode check happens under the same OS lock as operator freeze.
   self.registry.require_git_authority(&repo.id)?;
   let current=ref_value(&repo.path,&ch.target_ref)?;
   let previous=op.observed_target.clone().ok_or_else(||anyhow!("TARGET_MISSING"))?;
   if current!=previous {
     let c=db(&self.registry)?;
     c.execute("UPDATE land_operations SET state='STALE',error_code='CHANGE_TARGET_CHANGED',updated_at=?2 WHERE id=?1",params![id,now()])?;
     drop(c);drop(_guard);
     return self.build_attempt(who,id);
   }
   let candidate=op.candidate_oid.clone().ok_or_else(||anyhow!("CANDIDATE_MISSING"))?;
   self.verify_source(&ch,&repo)?;
   let expected_pin=format!("refs/openscion/land/{id}-{}",op.attempt);
   if ref_value(&repo.path,&expected_pin)?!=candidate {bail!("CANDIDATE_RETENTION_FAILED");}
   if first_parent(&repo.path,&candidate)?!=current || has_tree(&repo.path,&candidate)?!=op.candidate_tree.clone().unwrap_or_default() {bail!("CANDIDATE_INVARIANT_FAILED");}
   let c=db(&self.registry)?;
   c.execute("UPDATE land_operations SET state='CAS_IN_FLIGHT',updated_at=?2 WHERE id=?1",params![id,now()])?;
   c.execute("UPDATE land_attempts SET state='CAS_IN_FLIGHT' WHERE operation_id=?1 AND attempt=?2",params![id,op.attempt])?;
   drop(c);
   let cas=fixed_git(&repo.path,&["update-ref".into(),"--no-deref".into(),ch.target_ref.clone(),candidate.clone(),current],None,false);
   let actual=ref_value(&repo.path,&ch.target_ref)?;
   if actual==candidate {
     let c=db(&self.registry)?;
     let tx=c.unchecked_transaction()?;
     tx.execute("UPDATE land_operations SET state='MERGED',result_oid=?2,error_code=NULL,updated_at=?3 WHERE id=?1",params![id,candidate,now()])?;
     tx.execute("UPDATE land_attempts SET state='MERGED' WHERE operation_id=?1 AND attempt=?2",params![id,op.attempt])?;
     tx.execute("UPDATE change_requests SET state='MERGED',generation=generation+1,updated_at=?2 WHERE id=?1",params![ch.id,now()])?;
     tx.execute("INSERT INTO change_outbox(id,event_type,resource_id,payload,created_at) VALUES (?1,'landing.succeeded',?2,?3,?4)",params![Uuid::new_v4().to_string(),id,format!("{{\"commit\":\"{candidate}\"}}"),now()])?;
     tx.commit()?;
     return self.get(who,id);
   }
   let c=db(&self.registry)?;
   c.execute("UPDATE land_operations SET state='RECOVERY_REQUIRED',error_code='LAND_CAS_CONFLICT',updated_at=?2 WHERE id=?1",params![id,now()])?;
   let _=cas;
   bail!("LAND_CAS_CONFLICT")
 }
 pub fn reconcile(&self,who:&Principal,id:&str)->Result<Operation>{
   let op=self.get(who,id)?;
   let (ch,repo,_,_)=self.change(who,&op.change_id,"reconcile")?;
   if op.state=="MERGED" {return Ok(op);}
   let _guard=lock_repo(&self.registry,&repo.id)?;
   // V0.6: mode check happens under the same OS lock as operator freeze.
   self.registry.require_git_authority(&repo.id)?;
   let current=ref_value(&repo.path,&ch.target_ref)?;
   let candidate=op.candidate_oid.clone().ok_or_else(||anyhow!("CANDIDATE_MISSING"))?;
   if current==candidate {
     let c=db(&self.registry)?;let tx=c.unchecked_transaction()?;
     tx.execute("UPDATE land_operations SET state='MERGED',result_oid=?2,error_code=NULL,updated_at=?3 WHERE id=?1",params![id,candidate,now()])?;
     tx.execute("UPDATE land_attempts SET state='MERGED' WHERE operation_id=?1 AND attempt=?2",params![id,op.attempt])?;
     tx.execute("UPDATE change_requests SET state='MERGED',generation=generation+1,updated_at=?2 WHERE id=?1",params![ch.id,now()])?;
     tx.commit()?;return self.get(who,id);
   }
   // Could have succeeded and then been advanced by another legitimate landing.
   // Do not infer success from ancestry alone; a durable witness would be required.
   let c=db(&self.registry)?;
   c.execute("UPDATE land_operations SET state='RECOVERY_REQUIRED',error_code='LAND_AMBIGUOUS_RESULT',updated_at=?2 WHERE id=?1",params![id,now()])?;
   bail!("LAND_RECONCILIATION_REQUIRED")
 }
 pub fn repreview(&self,who:&Principal,id:&str)->Result<Operation>{
   let op=self.get(who,id)?;
   if !["CONFLICT","STALE","PREVIEWING","VALIDATING","VALIDATED","REJECTED","CANDIDATE_READY"].contains(&op.state.as_str()) {bail!("CHANGE_INVALID_STATE");}
   self.build_attempt(who,id)
 }
}
fn git_text_with_input(repo:&Path,args:&[String],data:&[u8])->Result<String>{
 let (_,bytes)=fixed_git(repo,args,Some(data),false)?;
 Ok(String::from_utf8(bytes)?.trim().to_string())
}
#[cfg(test)]mod tests{use super::*;
 #[test]fn oid_validation(){assert!(valid_oid(&"a".repeat(40),40));assert!(!valid_oid("main",40));}
 #[test]fn fingerprints(){let a=fingerprint(&"same").unwrap();assert_eq!(a,fingerprint(&"same").unwrap());}
}
