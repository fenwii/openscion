//! Commercial release gate inspection: evidence, not marketing assertions.
//! A missing compiler/CI/supply-chain signature remains NOT VERIFIED.
use anyhow::{bail,Result};
use openscion_registry::Registry;
use rusqlite::Connection;
use serde::{Deserialize,Serialize};
use sha2::{Digest,Sha256};
use std::{fs,path::Path};
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct Gate {pub id:String,pub description:String,pub evidence:Option<String>,pub passed:bool}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct ReleaseReport {pub release:String,pub gates:Vec<Gate>,pub commercial_ga_qualified:bool}
impl ReleaseReport {
 pub fn from_file(path:&Path)->Result<Self>{
  let bytes=fs::read(path)?;if bytes.len()>4*1024*1024{bail!("RELEASE_EVIDENCE_TOO_LARGE");}
  let result:Self=serde_json::from_slice(&bytes)?;
  let required=["G0","G1","G2","G3","G4","G5","G6","G7"];
  if result.release!="0.6.0" {bail!("RELEASE_VERSION_MISMATCH");}
  for k in required {if result.gates.iter().filter(|x|x.id==k).count()!=1{bail!("RELEASE_GATE_MISSING_OR_DUPLICATED");}}
  if result.commercial_ga_qualified && result.gates.iter().any(|x|!x.passed||x.evidence.as_deref().is_none_or(|p|p.trim().is_empty())){bail!("FALSE_GA_QUALIFICATION");}
  Ok(result)
 }
 pub fn qualified(&self)->bool{self.gates.iter().all(|g|g.passed && g.evidence.as_ref().is_some_and(|s|!s.trim().is_empty())) && self.commercial_ga_qualified}
 pub fn digest(&self)->Result<String>{Ok(hex::encode(Sha256::digest(serde_json::to_vec(self)?)))}
}
pub fn service_db_doctor(reg:&Registry)->Result<serde_json::Value>{
 let c=Connection::open(reg.db_path())?;
 let version:i64=c.pragma_query_value(None,"user_version",|r|r.get(0))?;
 let integrity:String=c.query_row("PRAGMA integrity_check",[],|r|r.get(0))?;
 let foreign_keys:i64=c.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check",[],|r|r.get(0))?;
 let quarantined:i64=if version==6{c.query_row("SELECT COUNT(*) FROM repository_authority_v6 WHERE mode='QUARANTINED'",[],|r|r.get(0))?}else{0};
 Ok(serde_json::json!({"schema_version":version,"sqlite_integrity":integrity,"foreign_key_errors":foreign_keys,"quarantined_repositories":quarantined,"release":"0.6.0","qualified":false}))
}
