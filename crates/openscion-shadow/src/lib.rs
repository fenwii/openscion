//! OpenScion V0.5 shadow Git history import and native semantic parity.
//!
//! CRITICAL: This module NEVER modifies Git refs or commits, V0.4 landing,
//! or V0.3 workspace state. Git remains the only authoritative source.
use anyhow::{anyhow,bail,Context,Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD,Engine as _};
use openscion_cas::{FsObjectStore,ObjectWriter};
use openscion_derived::{Manifest,Leaf,difference,derive_root,manifest_from_tree,verify_parity};
use openscion_git::{GitRepository,CommitInfo};
use openscion_native::{Changeset,ChangesetId,ChangesetStore};
use openscion_registry::{Principal,Registry,RepoRecord};
use openscion_types::{FileMode,ObjectId,ObjectKind};
use rusqlite::{params,Connection,OptionalExtension,TransactionBehavior};
use serde::Serialize;
use sha2::{Digest,Sha256};
use std::{collections::{BTreeMap,HashMap,HashSet},sync::mpsc,thread,time::{Duration,SystemTime,UNIX_EPOCH}};
use uuid::Uuid;

const IMPORT_MAX_COMMITS:usize=10_000;
const IMPORT_MAX_FILES:usize=100_000;
const IMPORT_MAX_DIR_DEPTH:usize=128;
const BLOB_LIMIT:usize=32*1024*1024; // Git backend has a 32 MiB output ceiling
const MAX_IMPORT_TOTAL_BYTES:u64=512*1024*1024; // per commit, including parity scan
const MAX_COMMIT_METADATA_BYTES:usize=64*1024*1024; // graph discovery cache
const LEASE_SECS:i64=120;
fn now()->i64{SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64}
fn sha_hex(bytes:&[u8])->String{hex::encode(Sha256::digest(bytes))}
fn conn(reg:&Registry)->Result<Connection>{
    let db=Connection::open(reg.db_path())?;
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000; PRAGMA synchronous=FULL;")?;
    Ok(db)
}
fn require_admin(who:&Principal)->Result<()> {
    if who.role!="admin" {bail!("PERMISSION_DENIED");}
    Ok(())
}
fn get_repo(reg:&Registry,who:&Principal,id:&str)->Result<RepoRecord>{
    reg.get_authorized(id,who)?.ok_or_else(||anyhow!("REPOSITORY_NOT_FOUND"))
}
#[derive(Clone,Debug,Serialize)]
pub struct ShadowStatus {
    pub repo_id:String,pub enabled:bool,pub tracked_ref:String,
    pub state:String,pub last_observed_oid:Option<String>,
    pub verified_mappings:i64,pub pending_jobs:i64,pub failed_jobs:i64,
}
#[derive(Clone,Debug,Serialize)]
pub struct Mapping {
    pub repo_id:String,pub git_oid:String,pub native_id:String,
    pub root_tree_id:String,pub git_tree_oid:String,pub manifest_digest:String,
    pub state:String,
}
#[derive(Clone,Debug,Serialize)]
pub struct Job {
    pub id:String,pub repo_id:String,pub git_oid:String,
    pub state:String,pub generation:i64,pub attempts:i64,
    pub last_error:Option<String>,
}
#[derive(Clone,Debug,Serialize)]
pub struct WorkerResult {pub job_id:String,pub state:String,pub result:Option<String>,pub error:Option<String>}

#[derive(Clone)]
pub struct ShadowService{pub registry:Registry}
impl ShadowService {
    pub fn new(registry:Registry)->Self{Self{registry}}
    fn store_paths(&self,repo:&str)->Result<(FsObjectStore,ChangesetStore)> {
        // Native objects are globally content-addressed, but authorization is
        // always performed at repository boundary. A separate per-repo store
        // prevents accidental data accessibility across repositories.
        if repo.len()>128 || !repo.bytes().all(|c|c.is_ascii_hexdigit()||c==b'-') {bail!("invalid repository id");}
        let root=self.registry.db_path().parent().context("invalid service data directory")?
            .join("shadow-native").join(repo);
        Ok((FsObjectStore::new(root.join("cas")),ChangesetStore::new(root.join("changesets"))))
    }
    pub fn enable(&self,who:&Principal,repo_id:&str,tracked_ref:&str)->Result<ShadowStatus>{
        require_admin(who)?;
        let repo=get_repo(&self.registry,who,repo_id)?;
        if !tracked_ref.starts_with("refs/heads/") || tracked_ref.len()>512 || tracked_ref.contains("..")
            || tracked_ref.bytes().any(|c|!c.is_ascii_graphic()) {bail!("INVALID_SHADOW_REF");}
        let git=GitRepository::open(&repo.path)?;
        let oid=git.resolve_ref(tracked_ref)?; // resolves to a pinned Git commit OID
        git.read_commit(&oid)?;
        let db=conn(&self.registry)?;
        db.execute("INSERT INTO shadow_repositories(repo_id,enabled,tracked_ref,state,last_observed_oid,updated_at)
          VALUES (?1,1,?2,'ENABLED',?3,?4)
          ON CONFLICT(repo_id) DO UPDATE SET enabled=1,tracked_ref=excluded.tracked_ref,state='ENABLED',last_observed_oid=excluded.last_observed_oid,updated_at=excluded.updated_at",
          params![repo_id,tracked_ref,oid,now()])?;
        db.execute("INSERT INTO shadow_ref_observations(id,repo_id,ref_name,git_oid,observed_at) VALUES (?1,?2,?3,?4,?5)",
          params![Uuid::new_v4().to_string(),repo_id,tracked_ref,oid,now()])?;
        self.enqueue(who,repo_id,&oid)?;
        self.status(who,repo_id)
    }
    pub fn status(&self,who:&Principal,repo_id:&str)->Result<ShadowStatus>{
        get_repo(&self.registry,who,repo_id)?;
        let db=conn(&self.registry)?;
        let values:Option<(i64,String,String,Option<String>)>=db.query_row(
          "SELECT enabled,tracked_ref,state,last_observed_oid FROM shadow_repositories WHERE repo_id=?1",
          [repo_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        let (enabled,tracked_ref,state,last_observed_oid)=values.unwrap_or((0,"refs/heads/main".into(),"DISABLED".into(),None));
        let verified_mappings:i64=db.query_row("SELECT COUNT(*) FROM shadow_mappings WHERE repo_id=?1 AND state='VERIFIED'",[repo_id],|r|r.get(0))?;
        let pending_jobs:i64=db.query_row("SELECT COUNT(*) FROM shadow_jobs WHERE repo_id=?1 AND state IN ('PENDING','RUNNING')",[repo_id],|r|r.get(0))?;
        let failed_jobs:i64=db.query_row("SELECT COUNT(*) FROM shadow_jobs WHERE repo_id=?1 AND state IN ('FAILED','INELIGIBLE')",[repo_id],|r|r.get(0))?;
        Ok(ShadowStatus{repo_id:repo_id.into(),enabled:enabled!=0,tracked_ref,state,last_observed_oid,
            verified_mappings,pending_jobs,failed_jobs})
    }
    pub fn mapping(&self,who:&Principal,repo_id:&str,oid:&str)->Result<Mapping>{
        let repo=get_repo(&self.registry,who,repo_id)?;
        let g=GitRepository::open(&repo.path)?;
        let oid=g.validate_oid(oid)?;
        let git_oid=format!("git:{}:{oid}",g.algorithm.label());
        self.mapping_internal(repo_id,&git_oid)?.ok_or_else(||anyhow!("SHADOW_MAPPING_NOT_FOUND"))
    }
    fn mapping_internal(&self,repo_id:&str,oid:&str)->Result<Option<Mapping>>{
        let db=conn(&self.registry)?;
        db.query_row("SELECT repo_id,git_oid,native_id,root_tree_id,git_tree_oid,manifest_digest,state
           FROM shadow_mappings WHERE repo_id=?1 AND git_oid=?2",params![repo_id,oid],|r|Ok(Mapping{
              repo_id:r.get(0)?,git_oid:r.get(1)?,native_id:r.get(2)?,root_tree_id:r.get(3)?,
              git_tree_oid:r.get(4)?,manifest_digest:r.get(5)?,state:r.get(6)?
           })).optional().map_err(Into::into)
    }
    pub fn enqueue(&self,who:&Principal,repo_id:&str,oid:&str)->Result<Job>{
        require_admin(who)?;
        let repo=get_repo(&self.registry,who,repo_id)?;
        let git=GitRepository::open(&repo.path)?;
        let pinned=git.read_commit(oid)?.oid;
        let db=conn(&self.registry)?;
        let enabled:Option<i64>=db.query_row("SELECT enabled FROM shadow_repositories WHERE repo_id=?1",[repo_id],|r|r.get(0)).optional()?;
        if enabled!=Some(1){bail!("SHADOW_NOT_ENABLED");}
        let id=Uuid::new_v4().to_string();
        db.execute("INSERT OR IGNORE INTO shadow_jobs(id,repo_id,git_oid,job_kind,state,created_at,updated_at)
               VALUES (?1,?2,?3,'IMPORT','PENDING',?4,?4)",params![id,repo_id,pinned,now()])?;
        db.query_row("SELECT id,repo_id,git_oid,state,generation,attempts,last_error FROM shadow_jobs
               WHERE repo_id=?1 AND git_oid=?2 AND job_kind='IMPORT'",params![repo_id,pinned],|r|Ok(Job{
                   id:r.get(0)?,repo_id:r.get(1)?,git_oid:r.get(2)?,state:r.get(3)?,generation:r.get(4)?,
                   attempts:r.get(5)?,last_error:r.get(6)?
               })).map_err(Into::into)
    }
    pub fn observe(&self,who:&Principal,repo_id:&str)->Result<Job>{
        require_admin(who)?;
        let status=self.status(who,repo_id)?;
        if !status.enabled {bail!("SHADOW_NOT_ENABLED");}
        let repo=get_repo(&self.registry,who,repo_id)?;
        let git=GitRepository::open(&repo.path)?;
        let oid=git.resolve_ref(&status.tracked_ref)?;
        let db=conn(&self.registry)?;
        db.execute("UPDATE shadow_repositories SET last_observed_oid=?2,updated_at=?3 WHERE repo_id=?1",
          params![repo_id,oid,now()])?;
        db.execute("INSERT INTO shadow_ref_observations(id,repo_id,ref_name,git_oid,observed_at) VALUES (?1,?2,?3,?4,?5)",
          params![Uuid::new_v4().to_string(),repo_id,status.tracked_ref,oid,now()])?;
        self.enqueue(who,repo_id,&oid)
    }
    pub fn jobs(&self,who:&Principal,repo_id:&str)->Result<Vec<Job>>{
        get_repo(&self.registry,who,repo_id)?;
        let db=conn(&self.registry)?;
        let mut s=db.prepare("SELECT id,repo_id,git_oid,state,generation,attempts,last_error FROM shadow_jobs WHERE repo_id=?1 ORDER BY created_at DESC LIMIT 100")?;
        let rows=s.query_map([repo_id],|r|Ok(Job{id:r.get(0)?,repo_id:r.get(1)?,git_oid:r.get(2)?,state:r.get(3)?,generation:r.get(4)?,attempts:r.get(5)?,last_error:r.get(6)?}))?;
        rows.collect::<std::result::Result<Vec<_>,_>>().map_err(Into::into)
    }
    /// Claim a single job by an IMMEDIATE SQLite transaction and fenced generation.
    fn claim(&self,repo_id:&str,worker:&str)->Result<Option<(Job,i64)>>{
        let mut db=conn(&self.registry)?;
        let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let choice:Option<String>=tx.query_row("SELECT id FROM shadow_jobs
          WHERE repo_id=?1 AND EXISTS (SELECT 1 FROM shadow_repositories sr WHERE sr.repo_id=shadow_jobs.repo_id AND sr.enabled=1)
            AND (state='PENDING' OR (state='RUNNING' AND lease_until<?2))
          ORDER BY created_at,id LIMIT 1",params![repo_id,now()],|r|r.get(0)).optional()?;
        let Some(id)=choice else {tx.commit()?;return Ok(None)};
        tx.execute("UPDATE shadow_jobs SET state='RUNNING',generation=generation+1,attempts=attempts+1,
          lease_until=?2,worker=?3,updated_at=?4 WHERE id=?1",params![id,now()+LEASE_SECS,worker,now()])?;
        let job=tx.query_row("SELECT id,repo_id,git_oid,state,generation,attempts,last_error FROM shadow_jobs WHERE id=?1",[&id],|r|Ok(Job{
          id:r.get(0)?,repo_id:r.get(1)?,git_oid:r.get(2)?,state:r.get(3)?,generation:r.get(4)?,attempts:r.get(5)?,last_error:r.get(6)?
        }))?;
        let generation=job.generation;tx.commit()?;Ok(Some((job,generation)))
    }
    pub fn work(&self,who:&Principal,repo_id:&str,max_jobs:usize)->Result<Vec<WorkerResult>>{
        require_admin(who)?;
        get_repo(&self.registry,who,repo_id)?;
        if max_jobs==0||max_jobs>100 {bail!("worker batch count out of bounds");}
        let worker=Uuid::new_v4().to_string();let mut results=Vec::new();
        for _ in 0..max_jobs {
            let Some((job,gen))=self.claim(repo_id,&worker)? else {break;};
            // A short fixed lease alone is not enough for real Git histories.
            // Renew it periodically; SQLite generation fencing still protects
            // against a paused or dead worker being superseded.
            let (stop_tx,stop_rx)=mpsc::channel::<()>();
            let reg=self.registry.clone();let job_id=job.id.clone();let worker_id=worker.clone();
            let heartbeat=thread::spawn(move||{
                loop {
                    match stop_rx.recv_timeout(Duration::from_secs(20)) {
                        Ok(())|Err(mpsc::RecvTimeoutError::Disconnected)=>break,
                        Err(mpsc::RecvTimeoutError::Timeout)=>{
                            let db=match conn(&reg){Ok(x)=>x,Err(_)=>break};
                            let rows=db.execute("UPDATE shadow_jobs SET lease_until=?4 WHERE id=?1 AND generation=?2 AND worker=?3 AND state='RUNNING'",
                                params![job_id,gen,worker_id,now()+LEASE_SECS]);
                            if !matches!(rows,Ok(1)){break;}
                        }
                    }
                }
            });
            let outcome=self.import_history(who,&job.repo_id,&job.git_oid);
            let _=stop_tx.send(());
            heartbeat.join().map_err(|_|anyhow!("SHADOW_HEARTBEAT_FAILED"))?;
            let (state,result,detail)=match outcome {
                Ok(mapping)=>("VERIFIED",Some(mapping.native_id),None),
                Err(e)=>{
                    let message=e.to_string();
                    if message.contains("INELIGIBLE") || message.contains("BUDGET_EXCEEDED") {
                        ("INELIGIBLE",None,Some(message))
                    } else {("FAILED",None,Some(message))}
                }
            };
            let db=conn(&self.registry)?;
            let affected=db.execute("UPDATE shadow_jobs SET state=?3,last_error=?4,lease_until=0,
                 updated_at=?5 WHERE id=?1 AND generation=?2 AND worker=?6 AND state='RUNNING'",
                 params![job.id,gen,state,detail.as_deref(),now(),worker])?;
            if affected!=1 {bail!("SHADOW_JOB_FENCE_LOST");}
            results.push(WorkerResult{job_id:job.id,state:state.into(),result,error:detail});
        }
        Ok(results)
    }
    pub fn import_history(&self,who:&Principal,repo_id:&str,git_oid:&str)->Result<Mapping>{
        require_admin(who)?;
        let repo=get_repo(&self.registry,who,repo_id)?;
        let git=GitRepository::open(&repo.path)?;
        let tip=git.read_commit(git_oid)?.oid;
        if let Some(m)=self.mapping_internal(repo_id,&tip)? {if m.state=="VERIFIED" {return self.verify_mapping(who,repo_id,&tip);}}
        let (blobs,changesets)=self.store_paths(repo_id)?;
        // Iterative DFS: parents are processed before their children. Keeps
        // recursion off the stack; limits work so a malicious/deep history
        // cannot exhaust memory or indefinitely occupy a worker.
        let mut visiting=HashSet::<String>::new();
        let mut cache=HashMap::<String,CommitInfo>::new();
        let mut metadata_bytes=0usize;
        let mut completed=Vec::<String>::new();
        let mut stack=vec![(tip.clone(),false)];
        while let Some((oid,expanded))=stack.pop() {
            if self.mapping_internal(repo_id,&oid)?.is_some() {continue;}
            if expanded {completed.push(oid);continue;}
            if !visiting.insert(oid.clone()){continue;}
            if visiting.len()>IMPORT_MAX_COMMITS {bail!("SHADOW_IMPORT_BUDGET_EXCEEDED");}
            let commit=git.read_commit(&oid)?;
            metadata_bytes=metadata_bytes.checked_add(commit.raw_b64.len()).ok_or_else(||anyhow!("SHADOW_IMPORT_BUDGET_EXCEEDED"))?;
            if metadata_bytes>MAX_COMMIT_METADATA_BYTES {bail!("SHADOW_IMPORT_BUDGET_EXCEEDED");}
            stack.push((oid.clone(),true));
            for parent in commit.parents.iter().rev() {
                if self.mapping_internal(repo_id,parent)?.is_none(){stack.push((parent.clone(),false));}
            }
            cache.insert(oid,commit);
        }
        for oid in completed {
            let info=cache.remove(&oid).ok_or_else(||anyhow!("missing cached Git commit"))?;
            self.import_one(&git,repo_id,&info,&blobs,&changesets)?;
        }
        self.mapping_internal(repo_id,&tip)?.ok_or_else(||anyhow!("missing imported tip mapping"))
    }
    fn import_one(&self,git:&GitRepository,repo_id:&str,info:&CommitInfo,
          blobs:&FsObjectStore,changesets:&ChangesetStore)->Result<Mapping>{
        if let Some(m)=self.mapping_internal(repo_id,&info.oid)? {return Ok(m);}
        let mut parent_ids=Vec::with_capacity(info.parents.len());
        let mut base=Manifest::new();
        for (n,parent) in info.parents.iter().enumerate(){
            let mapping=self.mapping_internal(repo_id,parent)?.ok_or_else(||anyhow!("missing parent mapping"))?;
            if mapping.state!="VERIFIED"{bail!("SHADOW_PARENT_NOT_VERIFIED");}
            let id:ChangesetId=mapping.native_id.parse()?;
            changesets.get(id).context("SHADOW_PARENT_CHANGESET_MISSING_OR_CORRUPT")?;
            parent_ids.push(id);
            if n==0 {
                let root:ObjectId=mapping.root_tree_id.parse()?;
                base=manifest_from_tree(blobs,root)?;
            }
        }
        let (git_manifest,blob_mappings)=read_git_manifest(git,&info.oid,blobs)?;
        let changes=difference(&base,&git_manifest)?;
        if changes.len()>openscion_native::MAX_CHANGES {bail!("SHADOW_INELIGIBLE_DELTA_SIZE");}
        let raw=URL_SAFE_NO_PAD.decode(&info.raw_b64)?;
        let author=info.author_b64.as_deref().map(|b|URL_SAFE_NO_PAD.decode(b)).transpose()?.unwrap_or_default();
        let committer=info.committer_b64.as_deref().map(|b|URL_SAFE_NO_PAD.decode(b)).transpose()?.unwrap_or_default();
        let message=URL_SAFE_NO_PAD.decode(&info.message_b64)?;
        let mut raw_sha=[0u8;32];raw_sha.copy_from_slice(&Sha256::digest(&raw));
        let cset=Changeset{parents:parent_ids,git_origin:info.oid.clone(),git_raw_sha256:raw_sha,
            author,committer,message,changes};
        let native_id=changesets.put(&cset)?;
        let previous=if info.parents.is_empty(){None}else{
            Some(self.mapping_internal(repo_id,&info.parents[0])?.ok_or_else(||anyhow!("missing first parent"))?.root_tree_id.parse::<ObjectId>()?)
        };
        let root=derive_root(blobs,previous,&cset.changes)?;
        // Freshly traverse authoritative Git for a second independent
        // verification pass; compare every tracked path, mode and Blob hash.
        let (verified_git,_)=read_git_manifest(git,&info.oid,blobs)?;
        verify_parity(blobs,root,&verified_git)?;
        if verified_git!=git_manifest {bail!("SHADOW_GIT_SOURCE_CHANGED");}
        let fresh_info=git.read_commit(&info.oid)?;
        if fresh_info.tree_oid!=info.tree_oid || fresh_info.raw_b64!=info.raw_b64 {bail!("SHADOW_COMMIT_CHANGED");}
        let manifest_digest=digest_manifest(&git_manifest);
        let mapping=Mapping{repo_id:repo_id.into(),git_oid:info.oid.clone(),native_id:native_id.to_string(),
            root_tree_id:root.to_string(),git_tree_oid:info.tree_oid.clone(),manifest_digest,state:"VERIFIED".into()};
        let changed_paths:Vec<String>=cset.changes.iter().map(|c|URL_SAFE_NO_PAD.encode(c.path())).collect();
        let parents:Vec<String>=cset.parents.iter().map(ToString::to_string).collect();
        let now=now();
        let mut db=conn(&self.registry)?;
        let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT OR IGNORE INTO shadow_mappings(repo_id,git_oid,native_id,root_tree_id,git_tree_oid,manifest_digest,state,created_at)
          VALUES (?1,?2,?3,?4,?5,?6,'VERIFIED',?7)",
          params![repo_id,mapping.git_oid,mapping.native_id,mapping.root_tree_id,mapping.git_tree_oid,mapping.manifest_digest,now])?;
        // Any competing importer MUST agree on every immutable result.
        let existing:(String,String,String,String)=tx.query_row("SELECT native_id,root_tree_id,git_tree_oid,manifest_digest
          FROM shadow_mappings WHERE repo_id=?1 AND git_oid=?2",params![repo_id,mapping.git_oid],
          |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        if existing!=(mapping.native_id.clone(),mapping.root_tree_id.clone(),mapping.git_tree_oid.clone(),mapping.manifest_digest.clone()) {
            bail!("SHADOW_MAPPING_MISMATCH");
        }
        for (git_blob,native_blob) in &blob_mappings {
            tx.execute("INSERT OR IGNORE INTO shadow_git_blobs(repo_id,git_oid,native_blob_id,created_at) VALUES (?1,?2,?3,?4)",
                params![repo_id,git_blob,native_blob.to_string(),now])?;
            let previous:String=tx.query_row("SELECT native_blob_id FROM shadow_git_blobs WHERE repo_id=?1 AND git_oid=?2",
                params![repo_id,git_blob],|r|r.get(0))?;
            if previous!=native_blob.to_string() {bail!("SHADOW_GIT_BLOB_MAPPING_MISMATCH");}
        }
        let changed_json=serde_json::to_string(&changed_paths)?;
        let graph_json=serde_json::to_string(&parents)?;
        for (kind,value,result) in [
          ("root_tree/v1",format!("\"{}\"",root),root.to_string()),
          ("changed_paths/v1",changed_json.clone(),sha_hex(changed_json.as_bytes())),
          ("commit_graph/v1",graph_json.clone(),sha_hex(graph_json.as_bytes())),
          ("git_mapping/v1",format!("\"{}\"",mapping.git_oid),sha_hex(mapping.git_oid.as_bytes())),
        ] {
            tx.execute("INSERT OR IGNORE INTO shadow_derived(repo_id,native_id,kind,algorithm_version,result_id,value_json,created_at)
             VALUES (?1,?2,?3,1,?4,?5,?6)",params![repo_id,mapping.native_id,kind,result,value,now])?;
        }
        tx.execute("INSERT INTO shadow_verifications(id,repo_id,git_oid,native_id,algorithm_version,state,detail,created_at)
          VALUES (?1,?2,?3,?4,1,'VERIFIED',?5,?6)",params![Uuid::new_v4().to_string(),repo_id,info.oid,mapping.native_id,mapping.manifest_digest,now])?;
        tx.commit()?;
        Ok(mapping)
    }
    pub fn retry_failed(&self,who:&Principal,repo_id:&str,job_id:&str)->Result<Job>{
        require_admin(who)?;
        get_repo(&self.registry,who,repo_id)?;
        let db=conn(&self.registry)?;
        let changed=db.execute("UPDATE shadow_jobs SET state='PENDING',last_error=NULL,lease_until=0,worker=NULL,updated_at=?3
            WHERE id=?1 AND repo_id=?2 AND state='FAILED'",
            params![job_id,repo_id,now()])?;
        if changed!=1 {bail!("SHADOW_JOB_NOT_RETRYABLE");}
        db.query_row("SELECT id,repo_id,git_oid,state,generation,attempts,last_error FROM shadow_jobs WHERE id=?1",[job_id],|r|Ok(Job{
            id:r.get(0)?,repo_id:r.get(1)?,git_oid:r.get(2)?,state:r.get(3)?,generation:r.get(4)?,attempts:r.get(5)?,last_error:r.get(6)?
        })).map_err(Into::into)
    }
    /// Regenerate absent V1 derived indexes only after re-verifying Git and the
    /// immutable first-parent Changeset. Never overwrite conflicting old facts.
    pub fn rebuild_derived(&self,who:&Principal,repo_id:&str,git_oid:&str)->Result<Mapping>{
        require_admin(who)?;
        let verified=self.verify_mapping(who,repo_id,git_oid)?;
        let (_,store)=self.store_paths(repo_id)?;
        let id:ChangesetId=verified.native_id.parse()?;
        let cset=store.get(id)?;
        let paths:Vec<String>=cset.changes.iter().map(|c|URL_SAFE_NO_PAD.encode(c.path())).collect();
        let parents:Vec<String>=cset.parents.iter().map(ToString::to_string).collect();
        let changes_json=serde_json::to_string(&paths)?;
        let graph_json=serde_json::to_string(&parents)?;
        let facts=[
          ("root_tree/v1",format!("\"{}\"",verified.root_tree_id),verified.root_tree_id.clone()),
          ("changed_paths/v1",changes_json.clone(),sha_hex(changes_json.as_bytes())),
          ("commit_graph/v1",graph_json.clone(),sha_hex(graph_json.as_bytes())),
          ("git_mapping/v1",format!("\"{}\"",verified.git_oid),sha_hex(verified.git_oid.as_bytes())),
        ];
        let mut db=conn(&self.registry)?;
        let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (kind,value,result) in facts {
            tx.execute("INSERT OR IGNORE INTO shadow_derived(repo_id,native_id,kind,algorithm_version,result_id,value_json,created_at)
                VALUES (?1,?2,?3,1,?4,?5,?6)",
                params![repo_id,verified.native_id,kind,result,value,now()])?;
            let stored:(String,String)=tx.query_row("SELECT result_id,value_json FROM shadow_derived WHERE repo_id=?1 AND native_id=?2 AND kind=?3 AND algorithm_version=1",
                params![repo_id,verified.native_id,kind],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if stored!=(result.to_string(),value.to_string()) {bail!("SHADOW_DERIVATION_MISMATCH");}
        }
        tx.commit()?;Ok(verified)
    }
    /// Recompute parity against live authoritative Git, not the cached mapping.
    pub fn verify_mapping(&self,who:&Principal,repo_id:&str,git_oid:&str)->Result<Mapping>{
        let repo=get_repo(&self.registry,who,repo_id)?;
        let git=GitRepository::open(&repo.path)?;
        let info=git.read_commit(git_oid)?;
        let mapping=self.mapping_internal(repo_id,&info.oid)?.ok_or_else(||anyhow!("SHADOW_MAPPING_NOT_FOUND"))?;
        if mapping.state!="VERIFIED" || mapping.git_tree_oid!=info.tree_oid {bail!("SHADOW_MAPPING_MISMATCH");}
        let (blobs,changesets)=self.store_paths(repo_id)?;
        let cs_id:ChangesetId=mapping.native_id.parse()?;
        let cs=changesets.get(cs_id)?;
        if cs.git_origin!=info.oid || cs.id()?!=cs_id {bail!("SHADOW_CHANGESET_MISMATCH");}
        let raw=URL_SAFE_NO_PAD.decode(&info.raw_b64)?;
        if cs.git_raw_sha256.as_slice()!=Sha256::digest(&raw).as_slice() {bail!("SHADOW_RAW_COMMIT_MISMATCH");}
        if cs.parents.len()!=info.parents.len(){bail!("SHADOW_PARENT_COUNT_MISMATCH");}
        let mut parent_root=None;
        for (index,parent_git) in info.parents.iter().enumerate(){
            let parent=self.mapping_internal(repo_id,parent_git)?.ok_or_else(||anyhow!("SHADOW_PARENT_UNMAPPED"))?;
            if parent.native_id!=cs.parents[index].to_string(){bail!("SHADOW_PARENT_ID_MISMATCH");}
            if index==0 {parent_root=Some(parent.root_tree_id.parse::<ObjectId>()?);}
        }
        let (actual,_)=read_git_manifest(&git,&info.oid,&blobs)?;
        let expected_delta=difference(&if let Some(parent)=parent_root {manifest_from_tree(&blobs,parent)?} else {Manifest::new()},&actual)?;
        if cs.changes!=expected_delta {bail!("SHADOW_FILE_DELTA_MISMATCH");}
        let recomputed_root=derive_root(&blobs,parent_root,&cs.changes)?;
        let root:ObjectId=mapping.root_tree_id.parse()?;
        if recomputed_root!=root {bail!("SHADOW_DERIVED_ROOT_MISMATCH");}
        verify_parity(&blobs,root,&actual)?;
        if mapping.manifest_digest!=digest_manifest(&actual) {bail!("SHADOW_MANIFEST_DIGEST_MISMATCH");}
        Ok(mapping)
    }
    pub fn verify_sample(&self,who:&Principal,repo_id:&str,limit:usize)->Result<Vec<Mapping>>{
        if limit==0||limit>1000{bail!("verification sample limit exceeded");}
        get_repo(&self.registry,who,repo_id)?;
        let db=conn(&self.registry)?;
        let mut s=db.prepare("SELECT git_oid FROM shadow_mappings WHERE repo_id=?1 AND state='VERIFIED' ORDER BY git_oid LIMIT ?2")?;
        let oids:Vec<String>=s.query_map(params![repo_id,limit as i64],|r|r.get(0))?.collect::<std::result::Result<_,_>>()?;
        let mut verified=Vec::new();
        for oid in oids {verified.push(self.verify_mapping(who,repo_id,&oid)?);}
        Ok(verified)
    }
}

/// Walk Git Trees recursively without checkout; names and paths remain raw bytes.
/// An unsupported Gitlink or any unavailable Blob explicitly blocks conversion.
fn read_git_manifest(git:&GitRepository,commit_oid:&str,store:&FsObjectStore)->Result<(Manifest,BTreeMap<String,ObjectId>)>{
    let mut result=Manifest::new();
    let mut mappings=BTreeMap::new();
    let mut total_bytes=0u64;
    let mut dirs=vec![(Vec::<u8>::new(),0usize)];
    while let Some((prefix,depth))=dirs.pop() {
        if depth>IMPORT_MAX_DIR_DEPTH {bail!("SHADOW_INELIGIBLE_DEPTH");}
        let tree=git.tree_at(commit_oid,&prefix)?;
        for e in tree.entries {
            let name=URL_SAFE_NO_PAD.decode(&e.name_b64)?;
            let mut path=prefix.clone();
            if !path.is_empty(){path.push(b'/');}
            path.extend_from_slice(&name);
            openscion_native::validate_path(&path).map_err(|_|anyhow!("SHADOW_INELIGIBLE_PATH"))?;
            match (e.kind.as_str(),e.mode.as_str()) {
                ("tree","040000")=>dirs.push((path,depth+1)),
                ("blob","100644") | ("blob","100755") | ("blob","120000")=>{
                    if result.len()>=IMPORT_MAX_FILES {bail!("SHADOW_INELIGIBLE_FILE_COUNT");}
                    let mode=match e.mode.as_str(){"100644"=>FileMode::Regular,"100755"=>FileMode::Executable,_=>FileMode::Symlink};
                    let file=git.file_at(commit_oid,&path,BLOB_LIMIT).map_err(|err|anyhow!("SHADOW_INELIGIBLE_BLOB: {err}"))?;
                    total_bytes=total_bytes.checked_add(file.content.len() as u64).ok_or_else(||anyhow!("SHADOW_INELIGIBLE_BLOB_BUDGET"))?;
                    if total_bytes>MAX_IMPORT_TOTAL_BYTES {bail!("SHADOW_INELIGIBLE_BLOB_BUDGET");}
                    let native_id=store.put(ObjectKind::Blob,&file.content)?;
                    if let Some(previous)=mappings.insert(file.blob_oid,native_id) {
                        if previous!=native_id {bail!("SHADOW_GIT_BLOB_MAPPING_MISMATCH");}
                    }
                    if result.insert(path,Leaf{mode,blob:native_id}).is_some(){bail!("duplicate Git path");}
                }
                ("commit","160000")=>bail!("SHADOW_INELIGIBLE_GITLINK"),
                _=>bail!("SHADOW_INELIGIBLE_MODE"),
            }
        }
    }
    Ok((result,mappings))
}
fn digest_manifest(m:&Manifest)->String{
    let mut hash=Sha256::new();hash.update(b"openscion/manifest/v1\0");
    for (path,leaf) in m {
        hash.update((path.len() as u32).to_le_bytes());hash.update(path);
        hash.update(leaf.mode.number().to_le_bytes());hash.update(leaf.blob.digest);
    }
    hex::encode(hash.finalize())
}
