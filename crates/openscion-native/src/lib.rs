//! OpenScion V0.5 canonical, immutable, domain-separated Changeset objects.
//!
//! NOTE: These objects are shadow-only: none of this crate's APIs update Git refs.
//! Object storage is separate from V0.1 Blob/Tree CAS to preserve its wire format.
use anyhow::{anyhow, bail, Context, Result};
use openscion_types::{FileMode, ObjectId, ObjectKind, RepoName};
use sha2::{Digest, Sha256};
use std::{fmt, fs::{self, File}, io::{self, Read, Write, Cursor}, path::{Path, PathBuf}, str::FromStr};
use tempfile::NamedTempFile;

const MAGIC: &[u8; 8] = b"OSCCSET\0";
const VERSION: u16 = 1;
const DOMAIN: &[u8] = b"openscion\0v1\0changeset\0";
pub const MAX_CHANGESET_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CHANGES: usize = 100_000;
const MAX_PARENTS: usize = 64;
const MAX_PATH_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ChangesetId(pub [u8; 32]);
impl ChangesetId {
    pub fn hash(bytes: &[u8]) -> Self {
        let mut h = Sha256::new(); h.update(DOMAIN); h.update(bytes);
        let mut digest = [0u8;32]; digest.copy_from_slice(&h.finalize()); Self(digest)
    }
    pub fn hex(self) -> String { hex::encode(self.0) }
}
impl fmt::Display for ChangesetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f,"osc:changeset:sha256:{}",self.hex())
    }
}
impl FromStr for ChangesetId {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        let hexpart = s.strip_prefix("osc:changeset:sha256:").ok_or_else(||anyhow!("INVALID_CHANGESET_ID"))?;
        if hexpart.len()!=64 || !hexpart.bytes().all(|c|c.is_ascii_digit()||(b'a'..=b'f').contains(&c)) {bail!("INVALID_CHANGESET_ID");}
        let bytes=hex::decode(hexpart)?; let mut digest=[0u8;32];digest.copy_from_slice(&bytes);Ok(Self(digest))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileChange {
    Put { path: Vec<u8>, mode: FileMode, blob: ObjectId },
    Delete { path: Vec<u8> },
}
impl FileChange {
    pub fn path(&self) -> &[u8] {match self { Self::Put{path,..}|Self::Delete{path}=>path }}
}

/// A Changeset delta applies to its first parent only. Additional ordered
/// parents preserve lineage, including for Git merge commits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changeset {
    pub parents: Vec<ChangesetId>,
    pub git_origin: String,
    pub git_raw_sha256: [u8;32],
    pub author: Vec<u8>,
    pub committer: Vec<u8>,
    pub message: Vec<u8>,
    pub changes: Vec<FileChange>,
}
impl Changeset {
    pub fn validate(&self)->Result<()> {
        if self.parents.len()>MAX_PARENTS {bail!("too many Changeset parents");}
        if self.changes.len()>MAX_CHANGES {bail!("too many file changes");}
        if self.git_origin.len()>128 || !self.git_origin.starts_with("git:") ||
           !self.git_origin.bytes().all(|c|c.is_ascii_alphanumeric()||c==b':') {
            bail!("invalid Git provenance");
        }
        if self.author.len()>65536 || self.committer.len()>65536 || self.message.len()>4*1024*1024 {bail!("commit metadata limit");}
        let mut prev:Option<&[u8]>=None;
        for change in &self.changes {
            validate_path(change.path())?;
            if prev.is_some_and(|p|p>=change.path()) {bail!("duplicate or noncanonical path ordering");}
            prev=Some(change.path());
            if let FileChange::Put{mode,blob,..}=change {
                if *mode==FileMode::Directory || blob.kind!=ObjectKind::Blob {bail!("invalid Changeset file kind");}
            }
        }
        Ok(())
    }
    pub fn canonical_bytes(&self)->Result<Vec<u8>> {
        self.validate()?;
        let mut out=Vec::new();out.extend_from_slice(MAGIC);out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(self.parents.len() as u16).to_le_bytes());
        for p in &self.parents {out.extend_from_slice(&p.0);}
        put_bytes(&mut out,self.git_origin.as_bytes())?;
        out.extend_from_slice(&self.git_raw_sha256);
        put_bytes(&mut out,&self.author)?;
        put_bytes(&mut out,&self.committer)?;
        put_bytes(&mut out,&self.message)?;
        out.extend_from_slice(&(self.changes.len() as u32).to_le_bytes());
        for change in &self.changes {
            match change {
                FileChange::Put{path,mode,blob}=>{
                    out.push(1);put_bytes(&mut out,path)?;
                    out.extend_from_slice(&mode.number().to_le_bytes());
                    out.extend_from_slice(&blob.digest);
                }
                FileChange::Delete{path}=>{out.push(2);put_bytes(&mut out,path)?;}
            }
        }
        if out.len()>MAX_CHANGESET_BYTES {bail!("Changeset exceeds size limit");}
        Ok(out)
    }
    pub fn id(&self)->Result<ChangesetId>{Ok(ChangesetId::hash(&self.canonical_bytes()?))}
    pub fn decode(input:&[u8])->Result<Self>{
        if input.len()>MAX_CHANGESET_BYTES || input.len()<12 {bail!("invalid Changeset length");}
        let mut cursor=Cursor::new(input);
        let mut magic=[0;8];cursor.read_exact(&mut magic)?;
        if &magic!=MAGIC {bail!("invalid Changeset magic");}
        if read_u16(&mut cursor)?!=VERSION {bail!("unsupported Changeset encoding");}
        let pcount=read_u16(&mut cursor)? as usize;
        if pcount>MAX_PARENTS {bail!("too many parents");}
        let mut parents=Vec::with_capacity(pcount);
        for _ in 0..pcount {let mut bytes=[0;32];cursor.read_exact(&mut bytes)?;parents.push(ChangesetId(bytes));}
        let git_origin=String::from_utf8(read_bytes(&mut cursor,128)?)?;
        let mut git_raw_sha256=[0;32];cursor.read_exact(&mut git_raw_sha256)?;
        let author=read_bytes(&mut cursor,65536)?;
        let committer=read_bytes(&mut cursor,65536)?;
        let message=read_bytes(&mut cursor,4*1024*1024)?;
        let count=read_u32(&mut cursor)? as usize;
        if count>MAX_CHANGES {bail!("too many changes");}
        let mut changes=Vec::with_capacity(count);
        for _ in 0..count {
            let mut tag=[0];cursor.read_exact(&mut tag)?;
            let path=read_bytes(&mut cursor,MAX_PATH_BYTES)?;
            let change=match tag[0] {
                1=>{
                    let mode=FileMode::parse(read_u32(&mut cursor)?)?;
                    let mut digest=[0;32];cursor.read_exact(&mut digest)?;
                    FileChange::Put{path,mode,blob:ObjectId{kind:ObjectKind::Blob,digest}}
                }
                2=>FileChange::Delete{path},
                _=>bail!("unknown Changeset change opcode"),
            };
            changes.push(change);
        }
        if cursor.position() as usize !=input.len() {bail!("trailing Changeset data");}
        let decoded=Self{parents,git_origin,git_raw_sha256,author,committer,message,changes};
        decoded.validate()?;Ok(decoded)
    }
}
fn put_bytes(out:&mut Vec<u8>,data:&[u8])->Result<()>{
    let len:u32=data.len().try_into().context("field too large")?;
    out.extend_from_slice(&len.to_le_bytes());out.extend_from_slice(data);Ok(())
}
fn read_u16(r:&mut impl Read)->Result<u16>{let mut b=[0;2];r.read_exact(&mut b)?;Ok(u16::from_le_bytes(b))}
fn read_u32(r:&mut impl Read)->Result<u32>{let mut b=[0;4];r.read_exact(&mut b)?;Ok(u32::from_le_bytes(b))}
fn read_bytes(r:&mut impl Read,max:usize)->Result<Vec<u8>>{
    let len=read_u32(r)? as usize;
    if len>max {bail!("oversized Changeset field");}
    let mut out=vec![0;len];r.read_exact(&mut out)?;Ok(out)
}
pub fn validate_path(path:&[u8])->Result<()> {
    if path.is_empty()||path.len()>MAX_PATH_BYTES {bail!("invalid Changeset path length");}
    let parts:Vec<_>=path.split(|c|*c==b'/').collect();
    if parts.len()>128 {bail!("Changeset path too deep");}
    for c in parts {RepoName::new(c.to_vec())?;}
    Ok(())
}

/// Immutable disk storage, separate from the V0.1 Blob/Tree envelope.
#[derive(Clone,Debug)]
pub struct ChangesetStore {root:PathBuf}
impl ChangesetStore {
    pub fn new(root:impl Into<PathBuf>)->Self{Self{root:root.into()}}
    fn path(&self,id:ChangesetId)->PathBuf{
        let hex=id.hex();self.root.join("sha256").join(&hex[..2]).join(format!("{hex}.cset"))
    }
    pub fn put(&self,changeset:&Changeset)->Result<ChangesetId>{
        let bytes=changeset.canonical_bytes()?;let id=ChangesetId::hash(&bytes);
        let dst=self.path(id);let parent=dst.parent().context("invalid Changeset path")?;
        fs::create_dir_all(parent)?;
        let mut tmp=NamedTempFile::new_in(parent)?;tmp.write_all(&bytes)?;tmp.as_file_mut().sync_all()?;
        match tmp.persist_noclobber(&dst){
            Ok(file)=>{file.sync_all()?;File::open(parent)?.sync_all()?;},
            Err(e) if e.error.kind()==io::ErrorKind::AlreadyExists=>{self.get(id).context("existing Changeset corrupt")?;},
            Err(e)=>return Err(e.into()),
        }
        Ok(id)
    }
    pub fn get(&self,id:ChangesetId)->Result<Changeset>{
        let mut f=File::open(self.path(id)).with_context(||format!("missing Changeset {id}"))?;
        if f.metadata()?.len()>MAX_CHANGESET_BYTES as u64 {bail!("Changeset object too large");}
        let mut bytes=Vec::new();f.read_to_end(&mut bytes)?;
        if ChangesetId::hash(&bytes)!=id {bail!("CHANGESET_HASH_MISMATCH");}
        Changeset::decode(&bytes)
    }
    pub fn contains(&self,id:ChangesetId)->bool{self.path(id).is_file()}
    pub fn root(&self)->&Path{&self.root}
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sample()->Changeset{
        Changeset{parents:vec![],git_origin:format!("git:sha1:{}","a".repeat(40)),git_raw_sha256:[3;32],
          author:b"A <a@b> 1 +0000".to_vec(),committer:b"A <a@b> 1 +0000".to_vec(),message:b"initial".to_vec(),
          changes:vec![FileChange::Put{path:b"src/main.rs".to_vec(),mode:FileMode::Regular,blob:ObjectId::hash(ObjectKind::Blob,b"hello")}]}
    }
    #[test]fn roundtrip_and_integrity(){
        let src=sample();let raw=src.canonical_bytes().unwrap();let dec=Changeset::decode(&raw).unwrap();assert_eq!(src,dec);
        assert_eq!(src.id().unwrap().to_string(), "osc:changeset:sha256:ae973279f358821d6a5fa5c832ff686a4612dacc0acbd6ef9b597a020b6d4088");
        let tmp=tempfile::tempdir().unwrap();let cas=ChangesetStore::new(tmp.path());let id=cas.put(&src).unwrap();
        assert_eq!(id,cas.put(&src).unwrap());assert_eq!(cas.get(id).unwrap(),src);
        let mut corrupt=fs::read(cas.path(id)).unwrap();corrupt[14]^=1;fs::write(cas.path(id),corrupt).unwrap();
        assert!(cas.get(id).is_err());
    }
    #[test]fn rejects_unsorted_and_unsafe_paths(){
        let mut src=sample();src.changes.push(FileChange::Delete{path:b"a".to_vec()});
        assert!(src.canonical_bytes().is_err());
        src.changes=vec![FileChange::Delete{path:b"x/../y".to_vec()}];
        assert!(src.canonical_bytes().is_err());
    }
}
