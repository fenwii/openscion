//! V0.4 ChangeRequest persistence. A workspace Commit remains authoritative in Git.
use anyhow::{anyhow,bail,Result};
use openscion_registry::{Registry,Principal};
use rusqlite::{params,Connection,OptionalExtension};
use serde::{Deserialize,Serialize};
use sha2::{Sha256,Digest};
use std::time::{SystemTime,UNIX_EPOCH};
use uuid::Uuid;

pub fn now()->i64{SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64}
pub fn db(reg:&Registry)->Result<Connection>{let c=Connection::open(reg.db_path())?;c.execute_batch("PRAGMA foreign_keys=ON;PRAGMA busy_timeout=5000;PRAGMA synchronous=FULL;")?;Ok(c)}
pub fn hash_request<T:Serialize>(x:&T)->Result<String>{let bytes=serde_json::to_vec(x)?;let mut h=Sha256::new();h.update(b"openscion/change-request/v1\0");h.update(bytes);Ok(hex::encode(h.finalize()))}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct SubmitChange {
 pub repository_id:String,pub source_workspace_id:String,pub source_commit_oid:String,
 pub target_ref:String,pub title:String,#[serde(default)]pub description:String,
 pub idempotency_key:String,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct ChangeRequest {
 pub id:String,pub repo_id:String,pub workspace_id:String,pub source_oid:String,
 pub base_oid:String,pub target_ref:String,pub author:String,pub title:String,
 pub description:String,pub state:String,pub revision:i64,pub generation:i64,
}
#[derive(Clone)]pub struct ChangeService {pub registry:Registry}
impl ChangeService{
 pub fn new(registry:Registry)->Self{Self{registry}}
 pub fn submit(&self,principal:&Principal,req:SubmitChange)->Result<ChangeRequest>{
    if !self.registry.change_allowed(principal,&req.repository_id,"submit")? {bail!("PERMISSION_DENIED");}
    if req.title.trim().is_empty() || req.title.len()>256 || req.description.len()>8192 || req.idempotency_key.is_empty() || req.idempotency_key.len()>128 {bail!("CHANGE_INVALID_REQUEST");}
    let mut c=db(&self.registry)?;
    let ws:Option<(String,String,String,String,String)>=c.query_row("SELECT repo_id,owner,base_oid,head_oid,state FROM workspaces WHERE id=?1",[&req.source_workspace_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let (repo,owner,base,head,state)=ws.ok_or_else(||anyhow!("WORKSPACE_NOT_FOUND"))?;
    if repo!=req.repository_id || (owner!=principal.id && principal.role!="admin") || state!="SUBMITTED" && state!="ACTIVE" && state!="READY" {bail!("CHANGE_INVALID_WORKSPACE");}
    let expected=req.source_commit_oid.rsplit(':').next().unwrap_or("");
    if !head.ends_with(expected) || !(expected.len()==40 || expected.len()==64) || !expected.bytes().all(|x|x.is_ascii_hexdigit()) {bail!("CHANGE_SOURCE_MISMATCH");}
    let fp=hash_request(&req)?;
    let previous:Option<(String,String)>=c.query_row("SELECT id,request_fingerprint FROM change_requests WHERE author=?1 AND repo_id=?2 AND idem_key=?3",params![principal.id,req.repository_id,req.idempotency_key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((id,fingerprint))=previous{if fingerprint!=fp{bail!("CHANGE_IDEMPOTENCY_CONFLICT");}return self.get(principal,&id);}
    let id=Uuid::new_v4().to_string();
    c.execute("INSERT INTO change_requests(id,repo_id,source_workspace_id,source_oid,base_oid,target_ref,author,title,description,idem_key,request_fingerprint,state,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'SUBMITTED',?12,?12)",params![id,req.repository_id,req.source_workspace_id,expected,base,req.target_ref,principal.id,req.title,req.description,req.idempotency_key,fp,now()])?;
    self.get(principal,&id)
 }
 pub fn get(&self,principal:&Principal,id:&str)->Result<ChangeRequest>{
  let c=db(&self.registry)?;
  let row=c.query_row("SELECT id,repo_id,source_workspace_id,source_oid,base_oid,target_ref,author,title,description,state,revision,generation FROM change_requests WHERE id=?1",[id],|r|
   Ok(ChangeRequest{id:r.get(0)?,repo_id:r.get(1)?,workspace_id:r.get(2)?,source_oid:r.get(3)?,base_oid:r.get(4)?,target_ref:r.get(5)?,author:r.get(6)?,title:r.get(7)?,description:r.get(8)?,state:r.get(9)?,revision:r.get(10)?,generation:r.get(11)?})).optional()?.ok_or_else(||anyhow!("CHANGE_NOT_FOUND"))?;
  if !self.registry.permitted(principal,&row.repo_id)? {bail!("PERMISSION_DENIED");}
  Ok(row)
 }
 pub fn list(&self,principal:&Principal)->Result<Vec<ChangeRequest>>{
  let c=db(&self.registry)?;
  let mut q=c.prepare("SELECT id FROM change_requests ORDER BY created_at DESC,id DESC LIMIT 200")?;
  let ids=q.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
  let mut out=Vec::new();for id in ids {if let Ok(v)=self.get(principal,&id){out.push(v);}}Ok(out)
 }
}
