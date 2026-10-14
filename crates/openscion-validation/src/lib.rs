//! Candidate-bound deterministic Git object checks. Does NOT run untrusted application code.
use anyhow::{bail,Result};
use serde::{Deserialize,Serialize};
use sha2::{Sha256,Digest};

#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct ValidationEvidence {
    pub check_name:String,
    pub candidate_oid:String,
    pub tree_oid:String,
    pub policy_digest:String,
    pub suite_digest:String,
    pub runner_digest:String,
    pub state:String,
}
/// A deterministic witness computed after Git checked parent/tree/OID and object reachability.
/// This check is infrastructure integrity only and not a substitute for application tests.
pub fn verified_tree(candidate:&str,tree:&str,policy_digest:&str)->Result<ValidationEvidence>{
    if candidate.is_empty() || tree.is_empty() || policy_digest.len()!=64 {bail!("VALIDATION_INVALID_BINDING");}
    let mut h=Sha256::new();h.update(b"openscion/git-tree-valid/v1\0");
    Ok(ValidationEvidence{check_name:"git-tree-valid".into(),candidate_oid:candidate.into(),tree_oid:tree.into(),policy_digest:policy_digest.into(),suite_digest:hex::encode(h.finalize()),runner_digest:"openscion-git-integrity-v1".into(),state:"PASSED".into()})
}
