//! Deterministic policy schema. A policy must be explicitly installed per target ref.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug,Clone,Serialize,Deserialize,PartialEq,Eq)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub version:u32,
    pub target_ref:String,
    pub required_checks:Vec<String>,
    pub required_approvals:u32,
    pub independent_review:bool,
    pub max_attempts:u32,
}
impl Policy {
    /// Deliberately conservative: the sole integrated check proves candidate Git object integrity,
    /// NOT functional correctness of the source code. Never use as a production CI substitute.
    pub fn development(target:&str)->Self {Self{version:1,target_ref:target.into(),required_checks:vec!["git-tree-valid".into()],required_approvals:0,independent_review:true,max_attempts:3}}
    pub fn validate(&self)->Result<()> {
        if self.version!=1 || !valid_target(&self.target_ref) {bail!("POLICY_INVALID_TARGET");}
        if self.max_attempts==0 || self.max_attempts>10 || self.required_approvals>10 {bail!("POLICY_INVALID_LIMIT");}
        if self.required_checks.is_empty() || self.required_checks.len()>20 {bail!("POLICY_CHECKS_REQUIRED");}
        if !self.required_checks.iter().any(|x|x=="git-tree-valid") {bail!("POLICY_GIT_INTEGRITY_REQUIRED");}
        let mut unique=std::collections::HashSet::new();
        for check in &self.required_checks {
            if check.is_empty() || check.len()>64 || !check.bytes().all(|b|b.is_ascii_alphanumeric() || matches!(b,b'-'|b'_'|b'.')) || !unique.insert(check) {bail!("POLICY_INVALID_CHECK");}
        }
        Ok(())
    }
    pub fn digest(&self)->Result<String> {self.validate()?;let mut h=Sha256::new();h.update(b"openscion/policy/v1\0");h.update(serde_json::to_vec(self)?);Ok(hex::encode(h.finalize()))}
}
/// Explicit protected branch only. No HEAD, symbolic refs, or tag mutations.
pub fn valid_target(s:&str)->bool{
    s.starts_with("refs/heads/") && s.len()<=240 && !s.contains("..") && !s.contains("//") && !s.ends_with('/')
        && !s.ends_with('.') && !s.contains("@{") && !s.ends_with(".lock")
        && s.bytes().all(|x| x.is_ascii_alphanumeric() || matches!(x,b'/'|b'_'|b'-'|b'.'))
}
#[cfg(test)] mod tests{use super::*;
 #[test]fn policy_hash(){let p=Policy::development("refs/heads/main");assert_eq!(p.digest().unwrap(),p.digest().unwrap());}
 #[test]fn invalid_target(){assert!(!valid_target("refs/tags/evil"));assert!(!valid_target("refs/heads/a..b"));}
 #[test]fn allow_external_checks(){let mut p=Policy::development("refs/heads/main");p.required_checks.push("unit-tests".into());assert!(p.validate().is_ok());}
}
