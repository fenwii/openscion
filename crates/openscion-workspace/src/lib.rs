//! Git-backed isolated workspaces, recoverable lifecycle and retained commits.
//! Trust boundary: a worktree is NOT a process sandbox. Only register trusted Git repositories.
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use fs2::FileExt;
use openscion_git::GitRepository;
use openscion_registry::{Principal, Registry, RepoRecord};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::{self, OpenOptions}, io::Read, path::{Path, PathBuf}, process::{Command, Stdio}, thread, time::{Duration, SystemTime, UNIX_EPOCH}};
use uuid::Uuid;
use wait_timeout::ChildExt;

const GIT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_GIT_OUTPUT: usize = 4 * 1024 * 1024;
const MAX_GIT_ERR: u64 = 32 * 1024;
const MAX_WORKSPACES: i64 = 100;
fn now() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64 }
fn valid_key(s:&str)->bool { !s.is_empty() && s.len()<=128 && s.bytes().all(|c|c.is_ascii_alphanumeric() || matches!(c,b'-'|b'_'|b'.')) }
fn valid_path(s:&str)->bool { !s.is_empty() && s.len()<=4096 && !s.starts_with('/') && !s.contains('\\') && !s.contains('\0') && s.split('/').all(|p|!p.is_empty() && p!="." && p!="..") }
fn repo_ref(id:&str)->String{ format!("refs/openscion/workspaces/{id}") }
fn normalized_oid(value:&str)->&str { value.rsplit(':').next().unwrap_or(value) }

/// Fixed-argument Git runner. This is *not* a sandbox: trusted repositories only.
fn git(repo:&Path,args:&[String],limit:usize,write:bool)->Result<Vec<u8>> {
    let mut cmd=Command::new("git");
    cmd.env_clear().env("PATH",std::env::var("PATH").unwrap_or_else(|_|"/usr/bin:/bin".into()))
        .env("GIT_CONFIG_NOSYSTEM","1").env("GIT_CONFIG_GLOBAL","/dev/null")
        .env("GIT_NO_REPLACE_OBJECTS","1").env("GIT_NO_LAZY_FETCH","1")
        .env("GIT_TERMINAL_PROMPT","0").env("GIT_LFS_SKIP_SMUDGE","1")
        .env("GIT_ATTR_NOSYSTEM","1").env("GIT_EDITOR","true")
        .arg("--no-pager").arg("-C").arg(repo)
        .arg("-c").arg("core.hooksPath=/dev/null")
        .arg("-c").arg("commit.gpgsign=false")
        .arg("-c").arg("core.fsmonitor=false");
    if !write {cmd.env("GIT_OPTIONAL_LOCKS","0");}
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child=cmd.spawn().context("failed to launch Git")?;
    let stdout=child.stdout.take().ok_or_else(||anyhow!("missing Git stdout"))?;
    let stderr=child.stderr.take().ok_or_else(||anyhow!("missing Git stderr"))?;
    let max=limit.min(MAX_GIT_OUTPUT) as u64;
    let out_reader=thread::spawn(move||{let mut b=Vec::new();let r=stdout.take(max+1).read_to_end(&mut b);(r,b)});
    let err_reader=thread::spawn(move||{let mut b=Vec::new();let r=stderr.take(MAX_GIT_ERR+1).read_to_end(&mut b);(r,b)});
    let result=match child.wait_timeout(GIT_TIMEOUT)? {Some(s)=>s,None=>{let _=child.kill();let _=child.wait();bail!("GIT_TIMEOUT");}};
    let (a,out)=out_reader.join().map_err(|_|anyhow!("Git stdout reader panicked"))?;
    let (b,err)=err_reader.join().map_err(|_|anyhow!("Git stderr reader panicked"))?;
    a?;b?;
    if out.len() as u64>max || err.len() as u64>MAX_GIT_ERR {bail!("GIT_OUTPUT_LIMIT");}
    if !result.success(){bail!("GIT_FAILED({})",result.code().unwrap_or(-1));}
    Ok(out)
}
fn git_s(repo:&Path,args:&[String],write:bool)->Result<String>{
    let v=git(repo,args,4096,write)?;
    Ok(String::from_utf8(v)?.trim().to_string())
}
fn git_oid(repo:&Path)->Result<String>{git_s(repo,&["rev-parse".into(),"--verify".into(),"HEAD^{commit}".into()],false)}
fn git_ref(repo:&Path,reference:&str)->Result<Option<String>> {
    // Missing ref should not throw; distinguish from a corrupt object via rev-parse --verify.
    let out=git(repo,&["for-each-ref".into(),"--format=%(objectname)".into(),reference.into()],4096,false)?;
    let value=String::from_utf8(out)?.trim().to_string();
    if value.is_empty(){Ok(None)}else{Ok(Some(value))}
}
fn update_ref(repo:&Path,reference:&str,new:&str,old:&str)->Result<()> {
    git(repo,&["update-ref".into(),reference.into(),new.into(),old.into()],1024,true)?;Ok(())
}
fn lock(reg:&Registry,key:&str)->Result<std::fs::File>{
    if !valid_key(key){bail!("invalid lock key");}
    let dir=reg.db_path().parent().ok_or_else(||anyhow!("invalid data directory"))?.join("locks");
    fs::create_dir_all(&dir)?;
    let file=OpenOptions::new().read(true).write(true).create(true).open(dir.join(format!("{key}.lock")))?;
    file.lock_exclusive()?;Ok(file)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub id:String, pub repo_id:String, pub owner:String, pub task_id:Option<String>,
    pub base_oid:String, pub head_oid:String,
    #[serde(skip_serializing)] pub path:PathBuf,
    pub state:String, pub generation:i64, pub lease_expires:i64,
    pub created_at:i64, pub updated_at:i64, pub error:Option<String>,
}
#[derive(Debug,Clone,Deserialize,Serialize)]
pub struct CreateRequest {
    pub repository_id:String, pub base_revision:String, pub task_id:Option<String>,
    pub lease_seconds:Option<u64>, pub idempotency_key:String,
}
#[derive(Debug,Clone,Deserialize,Serialize)]
pub struct CommitRequest {
    pub expected_head:String, pub message:String, pub selected_paths:Vec<String>,
    pub idempotency_key:String,
}
#[derive(Debug,Clone,Serialize)]
pub struct CommitResult {pub workspace_id:String, pub commit_oid:String, pub parent_oid:String, pub retained:bool, pub merged:bool}
#[derive(Debug,Clone,Serialize)]
pub struct StatusEntry {pub path_b64:String,pub index_status:String,pub worktree_status:String,pub original_path_b64:Option<String>}
#[derive(Debug,Clone,Serialize)]
pub struct WorkspaceStatus {pub workspace_id:String,pub base_oid:String,pub head_oid:String,pub entries:Vec<StatusEntry>,pub dirty:bool}
#[derive(Clone)]
pub struct WorkspaceManager {registry:Registry}
impl WorkspaceManager {
    pub fn new(registry:Registry)->Self{Self{registry}}
    fn conn(&self)->Result<Connection>{
        let db=Connection::open(self.registry.db_path())?;
        db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;Ok(db)
    }
    fn repo(&self,principal:&Principal,id:&str,permission:&str)->Result<RepoRecord>{
        let record=self.registry.get_authorized(id,principal)?.ok_or_else(||anyhow!("REPOSITORY_NOT_FOUND"))?;
        if !self.registry.workspace_allowed(principal,id,permission)?{bail!("PERMISSION_DENIED");}
        Ok(record)
    }
    fn require_owner(&self,principal:&Principal,ws:&Workspace,permission:&str)->Result<RepoRecord>{
        if principal.role!="admin" && ws.owner!=principal.id {bail!("WORKSPACE_NOT_FOUND");}
        self.repo(principal,&ws.repo_id,permission)
    }
    pub fn get(&self,principal:&Principal,id:&str)->Result<Workspace>{
        let ws=self.raw_get(id)?;
        if principal.role!="admin" && (principal.id!=ws.owner || !self.registry.permitted(principal,&ws.repo_id)?) {bail!("WORKSPACE_NOT_FOUND");}
        Ok(ws)
    }
    fn raw_get(&self,id:&str)->Result<Workspace>{
        let db=self.conn()?;
        db.query_row("SELECT id,repo_id,owner,task_id,base_oid,head_oid,path,state,generation,lease_expires,created_at,updated_at,error FROM workspaces WHERE id=?1",[id],|r|Ok(Workspace{
            id:r.get(0)?,repo_id:r.get(1)?,owner:r.get(2)?,task_id:r.get(3)?,base_oid:r.get(4)?,head_oid:r.get(5)?,path:PathBuf::from(r.get::<_,String>(6)?),state:r.get(7)?,generation:r.get(8)?,lease_expires:r.get(9)?,created_at:r.get(10)?,updated_at:r.get(11)?,error:r.get(12)?
        })).optional()?.ok_or_else(||anyhow!("WORKSPACE_NOT_FOUND"))
    }
    pub fn list(&self,principal:&Principal)->Result<Vec<Workspace>>{
        let db=self.conn()?;
        let mut stmt=db.prepare("SELECT id FROM workspaces ORDER BY created_at,id LIMIT 1000")?;
        let ids=stmt.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids.into_iter().filter_map(|id|self.get(principal,&id).ok()).collect())
    }
    fn advance(&self,id:&str,from:&str,to:&str,head:Option<&str>)->Result<()> {
        let db=self.conn()?;
        let updated=db.execute("UPDATE workspaces SET state=?1,head_oid=COALESCE(?2,head_oid),generation=generation+1,updated_at=?3,error=NULL WHERE id=?4 AND state=?5",params![to,head,now(),id,from])?;
        if updated!=1 {bail!("WORKSPACE_STATE_CONFLICT");}Ok(())
    }
    fn flag_recovery(&self,id:&str,reason:&str)->Result<()> {
        let db=self.conn()?;
        db.execute("UPDATE workspaces SET state='RECOVERY_REQUIRED',error=?2,generation=generation+1,updated_at=?3 WHERE id=?1 AND state!='DISPOSED'",params![id,reason,now()])?;Ok(())
    }
    pub fn create(&self,principal:&Principal,req:CreateRequest)->Result<Workspace>{
        let repo=self.repo(principal,&req.repository_id,"create")?;
        if !valid_key(&req.idempotency_key) {bail!("INVALID_IDEMPOTENCY_KEY");}
        if req.task_id.as_ref().is_some_and(|v|v.len()>256){bail!("INVALID_TASK_ID");}
        let lease=req.lease_seconds.unwrap_or(86400);
        if !(60..=604800).contains(&lease){bail!("INVALID_LEASE");}
        let g=GitRepository::open(&repo.path)?;
        let resolved=if req.base_revision=="HEAD" || req.base_revision.starts_with("refs/") {
            g.resolve_ref(&req.base_revision)?
        } else {let oid=g.validate_oid(&req.base_revision)?;format!("git:{}:{oid}",g.algorithm.label())};
        g.read_commit(&resolved).context("WORKSPACE_BASE_INVALID")?;
        let root=self.registry.workspace_root();fs::create_dir_all(&root)?;
        let fingerprint=hash_json(&req)?;
        // Serialize allocation and administrative Git worktree creation per repository.
        let _authority_guard=lock(&self.registry,&format!("land-{}",repo.id))?;
        self.registry.require_git_authority(&repo.id)?;
        let _admin_guard=lock(&self.registry,&format!("repo-{}",repo.id))?;
        let db=self.conn()?;
        let existing:Option<(String,String)>=db.query_row("SELECT w.id,o.fingerprint FROM workspace_ops o JOIN workspaces w ON w.id=o.workspace_id WHERE o.principal_id=?1 AND o.kind='CREATE' AND o.idem_key=?2",params![principal.id,req.idempotency_key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((id,fp))=existing {
            if fp!=fingerprint {bail!("WORKSPACE_IDEMPOTENCY_CONFLICT");}
            return self.get(principal,&id);
        }
        let count:i64=db.query_row("SELECT COUNT(*) FROM workspaces WHERE state NOT IN ('DISPOSED','FAILED')",[],|r|r.get(0))?;
        if count>=MAX_WORKSPACES {bail!("WORKSPACE_LIMIT_EXCEEDED");}
        let id=Uuid::new_v4().to_string();
        let path=root.join(&repo.id).join(&id);
        fs::create_dir_all(path.parent().ok_or_else(||anyhow!("invalid workspace path"))?)?;
        let secs=now();
        db.execute("INSERT INTO workspaces(id,repo_id,owner,task_id,base_oid,head_oid,path,state,lease_expires,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?5,?6,'PREPARING',?7,?8,?8)",params![id,repo.id,principal.id,req.task_id,resolved,path.to_string_lossy().to_string(),secs+lease as i64,secs])?;
        db.execute("INSERT INTO workspace_ops(id,workspace_id,principal_id,kind,idem_key,fingerprint,state,created_at) VALUES (?1,?2,?3,'CREATE',?4,?5,'PENDING',?6)",params![Uuid::new_v4().to_string(),id,principal.id,req.idempotency_key,fingerprint,secs])?;
        drop(db);
        let outcome=(||->Result<()> {
            git(&repo.path,&["worktree".into(),"add".into(),"--detach".into(),path.to_string_lossy().to_string(),normalized_oid(&resolved).into()],2048,true)?;
            let current=git_oid(&path)?;
            if current!=normalized_oid(&resolved){bail!("WORKSPACE_BASE_MISMATCH");}
            let zero="0".repeat(current.len());
            update_ref(&repo.path,&repo_ref(&id),&current,&zero)?;
            self.advance(&id,"PREPARING","READY",None)?;
            self.conn()?.execute("UPDATE workspace_ops SET state='DONE' WHERE workspace_id=?1 AND kind='CREATE'",[&id])?;
            Ok(())
        })();
        if let Err(err)=outcome {let _=self.flag_recovery(&id,"WORKSPACE_PROVISION_INCOMPLETE");return Err(err);}
        self.get(principal,&id)
    }
    pub fn status(&self,principal:&Principal,id:&str)->Result<WorkspaceStatus>{
        let ws=self.get(principal,id)?;
        if ws.state=="DISPOSED" {bail!("WORKSPACE_DISPOSED");}
        let head=git_oid(&ws.path)?;
        let raw=git(&ws.path,&["status".into(),"--porcelain=v1".into(),"-z".into(),"--untracked-files=all".into()],MAX_GIT_OUTPUT,false)?;
        let entries=parse_status(&raw)?;
        Ok(WorkspaceStatus{workspace_id:id.into(),base_oid:ws.base_oid,head_oid:head,dirty:!entries.is_empty(),entries})
    }
    pub fn diff(&self,principal:&Principal,id:&str)->Result<Vec<u8>> {
        let ws=self.get(principal,id)?;
        if ws.state=="DISPOSED" {bail!("WORKSPACE_DISPOSED");}
        git(&ws.path,&["diff".into(),"--binary".into(),"--no-ext-diff".into(),"HEAD".into(),"--".into()],1024*1024,false)
    }
    pub fn commit(&self,principal:&Principal,id:&str,req:CommitRequest)->Result<CommitResult>{
        let ws=self.get(principal,id)?;let repo=self.require_owner(principal,&ws,"commit")?;
        if now()>ws.lease_expires {bail!("WORKSPACE_EXPIRED");}
        if !valid_key(&req.idempotency_key) || req.message.is_empty() || req.message.len()>8192 || req.message.contains('\0') || req.selected_paths.is_empty() || req.selected_paths.len()>1000 || req.selected_paths.iter().any(|s|!valid_path(s)) {bail!("INVALID_COMMIT_REQUEST");}
        let _authority_guard=lock(&self.registry,&format!("land-{}",repo.id))?;
        let _guard=lock(&self.registry,&format!("ws-{id}"))?;
        self.registry.require_git_authority(&repo.id)?;
        let ws=self.raw_get(id)?;
        let fp=hash_json(&req)?;
        let db=self.conn()?;
        let old:Option<(String,String,String,Option<String>,String)>=db.query_row("SELECT id,fingerprint,state,result_oid,previous_head FROM workspace_ops WHERE principal_id=?1 AND workspace_id=?2 AND kind='COMMIT' AND idem_key=?3",params![principal.id,id,req.idempotency_key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        if let Some((op,old_fp,state,result,before))=old {
            if old_fp!=fp {bail!("WORKSPACE_IDEMPOTENCY_CONFLICT");}
            if state=="DONE" {let oid=result.ok_or_else(||anyhow!("MISSING_COMMIT_RESULT"))?;return Ok(CommitResult{workspace_id:id.into(),commit_oid:oid,parent_oid:before,retained:true,merged:false});}
            return self.recover_pending_commit(&ws,&repo,&op,&before);
        }
        if ws.state!="READY" && ws.state!="ACTIVE" && ws.state!="SUBMITTED" {bail!("WORKSPACE_INVALID_STATE");}
        let git_repo=GitRepository::open(&repo.path)?;
        let expected=git_repo.validate_oid(&req.expected_head)?;
        let current=git_oid(&ws.path)?;
        if current!=expected || normalized_oid(&ws.head_oid)!=expected {bail!("WORKSPACE_HEAD_CHANGED");}
        let op=Uuid::new_v4().to_string();
        db.execute("INSERT INTO workspace_ops(id,workspace_id,principal_id,kind,idem_key,fingerprint,previous_head,state,created_at) VALUES (?1,?2,?3,'COMMIT',?4,?5,?6,'PENDING',?7)",params![op,id,principal.id,req.idempotency_key,fp,current,now()])?;
        self.advance(id,&ws.state,"COMMITTING",None)?;
        drop(db);
        let result=(||->Result<CommitResult>{
            let mut add=vec!["add".into(),"-A".into(),"--".into()];add.extend(req.selected_paths.iter().cloned());
            git(&ws.path,&add,1024,true)?;
            let staged=git(&ws.path,&["diff".into(),"--cached".into(),"--name-only".into(),"-z".into()],4096,false)?;
            if staged.is_empty() {bail!("WORKSPACE_NO_CHANGES");}
            let author=principal.id.clone();
            git(&ws.path,&["-c".into(),format!("user.name={author}"),"-c".into(),format!("user.email={author}@agents.openscion.invalid"),"commit".into(),"--no-verify".into(),"--no-gpg-sign".into(),"-m".into(),req.message],4096,true)?;
            self.recover_pending_commit(&ws,&repo,&op,&current)
        })();
        if result.is_err() {let _=self.flag_recovery(id,"COMMIT_RECONCILIATION_REQUIRED");}
        result
    }
    fn recover_pending_commit(&self,ws:&Workspace,repo:&RepoRecord,op:&str,before:&str)->Result<CommitResult>{
        let head=git_oid(&ws.path)?;
        if head==before {bail!("WORKSPACE_COMMIT_NOT_CREATED");}
        let parent=git_s(&ws.path,&["rev-parse".into(),"HEAD^".into()],false)?;
        if parent!=before {bail!("WORKSPACE_HEAD_AMBIGUOUS");}
        let reference=repo_ref(&ws.id);
        match git_ref(&repo.path,&reference)? {
            Some(value) if value==head=>{},
            Some(value) if value==before=>update_ref(&repo.path,&reference,&head,before)?,
            _=>bail!("WORKSPACE_RETENTION_CONFLICT"),
        }
        let mut db=self.conn()?;
        let tx=db.transaction()?;
        tx.execute("INSERT OR IGNORE INTO workspace_commits(id,workspace_id,previous_oid,commit_oid,operation_id,created_at) VALUES (?1,?2,?3,?4,?5,?6)",params![Uuid::new_v4().to_string(),ws.id,before,head,op,now()])?;
        tx.execute("UPDATE workspace_ops SET state='DONE',result_oid=?2 WHERE id=?1",params![op,head])?;
        tx.execute("UPDATE workspaces SET state='SUBMITTED',head_oid=?2,generation=generation+1,updated_at=?3,error=NULL WHERE id=?1",params![ws.id,format!("git:{}:{head}",if head.len()==40{"sha1"}else{"sha256"}),now()])?;
        tx.commit()?;
        Ok(CommitResult{workspace_id:ws.id.clone(),commit_oid:head,parent_oid:before.into(),retained:true,merged:false})
    }
    pub fn renew(&self,principal:&Principal,id:&str,seconds:u64)->Result<Workspace>{
        let ws=self.get(principal,id)?;self.require_owner(principal,&ws,"renew")?;
        if !(60..=604800).contains(&seconds) || ws.state=="DISPOSED" {bail!("INVALID_LEASE");}
        let db=self.conn()?;
        db.execute("UPDATE workspaces SET lease_expires=?2,state=CASE WHEN state='EXPIRED' THEN 'READY' ELSE state END,generation=generation+1,updated_at=?3 WHERE id=?1",params![id,now()+seconds as i64,now()])?;
        self.get(principal,id)
    }
    pub fn dispose(&self,principal:&Principal,id:&str)->Result<Workspace>{
        let ws=self.get(principal,id)?;let repo=self.require_owner(principal,&ws,"dispose")?;
        let _authority_guard=lock(&self.registry,&format!("land-{}",repo.id))?;
        let _admin=lock(&self.registry,&format!("repo-{}",repo.id))?;
        let _guard=lock(&self.registry,&format!("ws-{id}"))?;
        self.registry.require_git_authority(&repo.id)?;
        let ws=self.raw_get(id)?;
        if ws.state=="DISPOSED" {return self.get(principal,id);}
        if ws.state=="COMMITTING" || ws.state=="PREPARING" {bail!("WORKSPACE_BUSY");}
        if ws.path.exists() {
            let status=git(&ws.path,&["status".into(),"--porcelain=v1".into(),"-z".into(),"--untracked-files=all".into()],4096,false)?;
            if !status.is_empty(){bail!("WORKSPACE_DIRTY");}
        }
        if git_ref(&repo.path,&repo_ref(id))?.as_deref()!=Some(normalized_oid(&ws.head_oid)){bail!("WORKSPACE_RETENTION_CONFLICT");}
        self.conn()?.execute("UPDATE workspaces SET state='DISPOSING',generation=generation+1,updated_at=?2 WHERE id=?1",params![id,now()])?;
        if ws.path.exists(){
            if let Err(err)=git(&repo.path,&["worktree".into(),"remove".into(),ws.path.to_string_lossy().to_string()],2048,true){let _=self.flag_recovery(id,"REMOVE_FAILED");return Err(err);}
        }
        self.advance(id,"DISPOSING","DISPOSED",None)?;
        self.get(principal,id)
    }
    /// Administrative lease sweep. Never destroys dirty or clean worktrees.
    pub fn expire_all(&self)->Result<usize>{
        let db=self.conn()?;
        let updated=db.execute("UPDATE workspaces SET state='EXPIRED',generation=generation+1,updated_at=?1 WHERE lease_expires<?1 AND state IN ('READY','ACTIVE','SUBMITTED')",params![now()])?;
        Ok(updated)
    }
    /// Conservative reconciliation: never delete dirty data or guess ambiguous commits.
    pub fn recover_all(&self)->Result<Vec<(String,String)>>{
        let db=self.conn()?;
        let mut stmt=db.prepare("SELECT id FROM workspaces WHERE state IN ('PREPARING','COMMITTING','DISPOSING','RECOVERY_REQUIRED')")?;
        let ids=stmt.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let admin=Principal{id:"admin".into(),role:"admin".into()};
        let mut out=Vec::new();
        for id in ids {
            let ws=self.raw_get(&id)?;
            let repo=self.repo(&admin,&ws.repo_id,"create")?;
            let _authority_guard=lock(&self.registry,&format!("land-{}",repo.id))?;
            if self.registry.require_git_authority(&repo.id).is_err() {
                out.push((id,"AUTHORITY_FENCED".into()));
                continue;
            }
            let _guard=lock(&self.registry,&format!("repo-{}",repo.id))?;
            let _ws_guard=lock(&self.registry,&format!("ws-{id}"))?;
            let action=(||->Result<String>{
                if ws.state=="DISPOSING" && !ws.path.exists() {
                    self.advance(&id,"DISPOSING","DISPOSED",None)?;
                    return Ok("DISPOSED".into());
                }
                let pending_create:Option<i64>=self.conn()?.query_row(
                    "SELECT 1 FROM workspace_ops WHERE workspace_id=?1 AND kind='CREATE' AND state='PENDING' LIMIT 1",
                    [&id],|r|r.get(0)).optional()?;
                if ws.path.exists() && git_oid(&ws.path).ok().as_deref()==Some(normalized_oid(&ws.base_oid))
                    && (ws.state=="PREPARING" || ws.state=="RECOVERY_REQUIRED") && pending_create.is_some() {
                    let original=normalized_oid(&ws.base_oid);
                    match git_ref(&repo.path,&repo_ref(&id))? {
                        Some(ref_oid) if ref_oid==original=>{},
                        None=>update_ref(&repo.path,&repo_ref(&id),original,&"0".repeat(original.len()))?,
                        _=>bail!("workspace retention mismatch"),
                    }
                    self.advance(&id,&ws.state,"READY",None)?;
                    self.conn()?.execute("UPDATE workspace_ops SET state='DONE' WHERE workspace_id=?1 AND kind='CREATE'",[&id])?;
                    return Ok("READY".into());
                }
                let db=self.conn()?;
                let pending:Option<(String,String)>=db.query_row(
                    "SELECT id,previous_head FROM workspace_ops WHERE workspace_id=?1 AND kind='COMMIT' AND state='PENDING' ORDER BY created_at DESC LIMIT 1",
                    [&id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                if let Some((op,before))=pending {
                    if self.recover_pending_commit(&ws,&repo,&op,&before).is_ok() {
                        return Ok("SUBMITTED".into());
                    }
                }
                bail!("manual reconciliation required")
            })();
            let outcome=match action {Ok(state)=>state,Err(_)=>{let _=self.flag_recovery(&id,"OPERATOR_REVIEW_REQUIRED");"RECOVERY_REQUIRED".into()}};
            out.push((id,outcome));
        }
        Ok(out)
    }

}
fn hash_json<T:Serialize>(value:&T)->Result<String>{let bytes=serde_json::to_vec(value)?;let mut h=Sha256::new();h.update(bytes);Ok(hex::encode(h.finalize()))}
fn parse_status(raw:&[u8])->Result<Vec<StatusEntry>> {
    let mut out=Vec::new();let mut parts=raw.split(|b|*b==0).filter(|v|!v.is_empty());
    while let Some(entry)=parts.next(){
        if entry.len()<4 || entry[2]!=b' ' {bail!("invalid Git porcelain record");}
        let i=entry[0] as char;let w=entry[1] as char;
        let original=if matches!(i,'R'|'C') || matches!(w,'R'|'C') {Some(URL_SAFE_NO_PAD.encode(parts.next().ok_or_else(||anyhow!("truncated rename record"))?))}else{None};
        out.push(StatusEntry{path_b64:URL_SAFE_NO_PAD.encode(&entry[3..]),index_status:i.to_string(),worktree_status:w.to_string(),original_path_b64:original});
        if out.len()>10000 {bail!("WORKSPACE_STATUS_LIMIT");}
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn path_checks(){assert!(valid_path("src/a.rs"));assert!(!valid_path("../secrets"));assert!(!valid_path("/root"));assert!(!valid_path("a//b"));}
    #[test] fn porcelain(){let got=parse_status(b" M src/a.rs\0?? new.txt\0").unwrap();assert_eq!(got.len(),2);assert!(got[0].path_b64.len()>3);}
}
