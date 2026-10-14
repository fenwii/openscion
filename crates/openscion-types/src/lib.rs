//! Portable, canonical OpenScion object identity and repository path rules.
use sha2::{Digest, Sha256};
use std::{fmt, str::FromStr};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TypeError {
    #[error("invalid object id: {0}")]
    InvalidObjectId(String),
    #[error("invalid repository path: {0}")]
    InvalidPath(String),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum ObjectKind { Blob, Tree }
impl ObjectKind {
    pub const fn tag(self) -> &'static str { match self { Self::Blob => "blob", Self::Tree => "tree" } }
    pub const fn code(self) -> u8 { match self { Self::Blob => 1, Self::Tree => 2 } }
    pub fn from_code(code: u8) -> Result<Self, TypeError> {
        match code { 1 => Ok(Self::Blob), 2 => Ok(Self::Tree), _ => Err(TypeError::InvalidObjectId("unknown object kind".into())) }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ObjectId { pub kind: ObjectKind, pub digest: [u8; 32] }
impl ObjectId {
    pub fn hash(kind: ObjectKind, contents: &[u8]) -> Self {
        let mut hasher = Self::hasher(kind);
        hasher.update(contents);
        Self::from_hasher(kind, hasher)
    }
    pub fn hasher(kind: ObjectKind) -> Sha256 {
        let mut h = Sha256::new();
        h.update(b"openscion\0v1\0");
        h.update(kind.tag().as_bytes());
        h.update(b"\0");
        h
    }
    pub fn from_hasher(kind: ObjectKind, hasher: Sha256) -> Self {
        let result = hasher.finalize();
        let mut digest = [0; 32];
        digest.copy_from_slice(&result);
        Self { kind, digest }
    }
    pub fn hex(self) -> String { hex::encode(self.digest) }
}
impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "osc:{}:sha256:{}", self.kind.tag(), self.hex())
    }
}
impl FromStr for ObjectId {
    type Err = TypeError;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let parts: Vec<_> = input.split(':').collect();
        if parts.len() != 4 || parts[0] != "osc" || parts[2] != "sha256" || parts[3].len() != 64 || !parts[3].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(TypeError::InvalidObjectId(input.into()));
        }
        let kind = match parts[1] { "blob" => ObjectKind::Blob, "tree" => ObjectKind::Tree, _ => return Err(TypeError::InvalidObjectId(input.into())) };
        let raw = hex::decode(parts[3]).map_err(|_| TypeError::InvalidObjectId(input.into()))?;
        let mut digest = [0; 32]; digest.copy_from_slice(&raw);
        Ok(Self { kind, digest })
    }
}

/// Canonical single path component; raw bytes, not lossy Unicode conversion.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RepoName(Vec<u8>);
impl RepoName {
    pub fn new(raw: Vec<u8>) -> Result<Self, TypeError> {
        if raw.is_empty() || raw.len() > 255 || raw == b"." || raw == b".." || raw.contains(&0) || raw.contains(&b'/') {
            return Err(TypeError::InvalidPath(format!("unsafe path component ({} bytes)", raw.len())));
        }
        Ok(Self(raw))
    }
    pub fn as_bytes(&self) -> &[u8] { &self.0 }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FileMode { Directory, Regular, Executable, Symlink }
impl FileMode {
    pub fn number(self) -> u32 {
        match self { Self::Directory => 0o040000, Self::Regular => 0o100644, Self::Executable => 0o100755, Self::Symlink => 0o120000 }
    }
    pub fn parse(n: u32) -> Result<Self, TypeError> {
        match n { 0o040000 => Ok(Self::Directory), 0o100644 => Ok(Self::Regular), 0o100755 => Ok(Self::Executable), 0o120000 => Ok(Self::Symlink), _ => Err(TypeError::InvalidPath(format!("unsupported mode {n:o}"))) }
    }
    pub fn expected_kind(self) -> ObjectKind { if self == Self::Directory { ObjectKind::Tree } else { ObjectKind::Blob } }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn object_id_round_trip() {
        let id = ObjectId::hash(ObjectKind::Blob, b"hello");
        assert_eq!(id.to_string().parse::<ObjectId>().unwrap(), id);
        assert_ne!(id, ObjectId::hash(ObjectKind::Tree, b"hello"));
    }
    #[test]
    fn golden_empty_blob() {
        assert_eq!(ObjectId::hash(ObjectKind::Blob, b"").hex(), "8f3578ae73c639d41b8f611bacb065fdabed9fdc7d1e467b970ad440a75d14c7");
    }
    #[test]
    fn name_rejects_traversal() {
        for p in [b"..".as_slice(), b".".as_slice(), b"a/b".as_slice(), b"".as_slice(), b"bad\0name".as_slice()] {
            assert!(RepoName::new(p.to_vec()).is_err());
        }
    }
}
