//! Immutable local content-addressed object storage.
//! Raw (uncompressed) V1 envelope. Object IDs hash logical payload, not envelope.
use anyhow::{anyhow, bail, Context, Result};
use openscion_types::{ObjectId, ObjectKind};
use sha2::Digest;
use std::{
    fs::{self, File}, io::{self, Read, Write}, path::{Path, PathBuf},
};
use tempfile::NamedTempFile;

const MAGIC: &[u8; 7] = b"OSCOBJ\0";
const VERSION: u16 = 1;
const CODEC_RAW: u8 = 0;
const HEADER_LEN: u64 = 7 + 2 + 1 + 1 + 8 + 8;
pub const MAX_BLOB: u64 = 256 * 1024 * 1024;
pub const MAX_TREE: u64 = 32 * 1024 * 1024;

pub trait ObjectReader {
    fn get(&self, id: ObjectId) -> Result<Vec<u8>>;
    fn contains(&self, id: ObjectId) -> bool;
    fn verify(&self, id: ObjectId) -> Result<()> { self.get(id).map(|_| ()) }
}
pub trait ObjectWriter: ObjectReader {
    fn put(&self, kind: ObjectKind, data: &[u8]) -> Result<ObjectId>;
}

#[derive(Debug, Clone)]
pub struct FsObjectStore { root: PathBuf }
impl FsObjectStore {
    pub fn new(root: impl Into<PathBuf>) -> Self { Self { root: root.into() } }
    pub fn root(&self) -> &Path { &self.root }
    pub fn object_path(&self, id: ObjectId) -> PathBuf {
        let hash = id.hex();
        self.root.join("sha256").join(id.kind.tag()).join(&hash[..2]).join(&hash[2..4]).join(format!("{hash}.obj"))
    }
    pub fn max_for(kind: ObjectKind) -> u64 { if kind == ObjectKind::Blob { MAX_BLOB } else { MAX_TREE } }

    /// Store from a reader without buffering the entire payload in memory.
    pub fn put_reader<R: Read>(&self, kind: ObjectKind, reader: &mut R, expected_len: u64) -> Result<ObjectId> {
        if expected_len > Self::max_for(kind) { bail!("object exceeds the {} byte limit", Self::max_for(kind)); }
        // Stage in the *same filesystem* as final objects; we don't yet know the digest.
        let staging = self.root.join("staging");
        fs::create_dir_all(&staging).with_context(|| format!("create staging: {}", staging.display()))?;
        let mut tmp = NamedTempFile::new_in(&staging)?;
        tmp.write_all(MAGIC)?;
        tmp.write_all(&VERSION.to_le_bytes())?;
        tmp.write_all(&[kind.code(), CODEC_RAW])?;
        tmp.write_all(&expected_len.to_le_bytes())?;
        tmp.write_all(&expected_len.to_le_bytes())?;
        let mut h = ObjectId::hasher(kind);
        let mut total = 0u64;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 { break; }
            total = total.checked_add(n as u64).ok_or_else(|| anyhow!("size overflow"))?;
            if total > expected_len { bail!("source grew while being read"); }
            tmp.write_all(&buf[..n])?;
            h.update(&buf[..n]);
        }
        if total != expected_len { bail!("source size changed while being read"); }
        tmp.as_file_mut().sync_all()?;
        let id = ObjectId::from_hasher(kind, h);
        let dst = self.object_path(id);
        let parent = dst.parent().context("object path has no parent")?;
        fs::create_dir_all(parent)?;
        // Tempfile::persist_noclobber does not overwrite existing objects.
        // It may copy/link between dirs on the same filesystem; the staging dir
        // is created under the same repository-owned objects tree.
        match tmp.persist_noclobber(&dst) {
            Ok(published) => { published.sync_all()?; sync_directory(parent)?; }
            Err(err) if err.error.kind() == io::ErrorKind::AlreadyExists => {
                self.verify(id).context("existing content-addressed object is corrupt")?;
            }
            Err(err) => return Err(err.into()),
        }
        Ok(id)
    }
    pub fn put_blob_file(&self, path: &Path) -> Result<ObjectId> {
        let before = fs::symlink_metadata(path)?;
        if !before.is_file() { bail!("not a regular file: {}", path.display()); }
        let mut f = File::open(path)?;
        let id = self.put_reader(ObjectKind::Blob, &mut f, before.len())?;
        let after = fs::symlink_metadata(path)?;
        // This is best-effort concurrent-write detection, not a point-in-time snapshot.
        if after.len() != before.len() || after.modified().ok() != before.modified().ok() {
            bail!("source changed while scanning: {}", path.display());
        }
        Ok(id)
    }
    pub fn put_bytes(&self, kind: ObjectKind, data: &[u8]) -> Result<ObjectId> {
        self.put_reader(kind, &mut io::Cursor::new(data), data.len() as u64)
    }
    pub fn read_verified(&self, id: ObjectId) -> Result<Vec<u8>> {
        let path = self.object_path(id);
        let mut f = File::open(&path).with_context(|| format!("open object {id}"))?;
        let mut magic = [0u8; 7]; f.read_exact(&mut magic)?;
        if &magic != MAGIC { bail!("object {id}: bad envelope magic"); }
        let version = read_u16(&mut f)?;
        if version != VERSION { bail!("object {id}: unsupported envelope version {version}"); }
        let mut kinds = [0u8; 2]; f.read_exact(&mut kinds)?;
        if kinds[0] != id.kind.code() || kinds[1] != CODEC_RAW { bail!("object {id}: incorrect kind or codec"); }
        let logical_len = read_u64(&mut f)?;
        let encoded_len = read_u64(&mut f)?;
        if logical_len != encoded_len || logical_len > Self::max_for(id.kind) { bail!("object {id}: invalid length"); }
        let actual_size = f.metadata()?.len();
        if actual_size != HEADER_LEN + encoded_len { bail!("object {id}: truncated or extra content"); }
        let size: usize = logical_len.try_into().context("length does not fit usize")?;
        let mut content = vec![0u8; size]; f.read_exact(&mut content)?;
        if ObjectId::hash(id.kind, &content) != id { bail!("object {id}: HASH MISMATCH"); }
        Ok(content)
    }
    pub fn verify_id(&self, id: ObjectId) -> Result<()> { self.read_verified(id).map(|_| ()) }

    /// Enumerate all published objects; malformed paths are reported, not ignored.
    pub fn list_objects(&self) -> Result<Vec<ObjectId>> {
        let mut out = Vec::new();
        for kind in [ObjectKind::Blob, ObjectKind::Tree] {
            let dir = self.root.join("sha256").join(kind.tag());
            if !dir.exists() { continue; }
            walk_published(&dir, kind, &mut out)?;
        }
        Ok(out)
    }
    pub fn is_present(&self, id: ObjectId) -> bool { self.object_path(id).is_file() }
}
impl ObjectReader for FsObjectStore {
    fn get(&self, id: ObjectId) -> Result<Vec<u8>> { self.read_verified(id) }
    fn contains(&self, id: ObjectId) -> bool { self.is_present(id) }
}
impl ObjectWriter for FsObjectStore {
    fn put(&self, kind: ObjectKind, data: &[u8]) -> Result<ObjectId> { self.put_bytes(kind, data) }
}
fn walk_published(dir: &Path, kind: ObjectKind, out: &mut Vec<ObjectId>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_symlink() { bail!("symlink found in object storage: {}", entry.path().display()); }
        if ty.is_dir() { walk_published(&entry.path(), kind, out)?; }
        else if ty.is_file() {
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| anyhow!("non-Unicode object filename"))?;
            let hex_digest = name.strip_suffix(".obj").ok_or_else(|| anyhow!("unexpected file in CAS: {name}"))?;
            let id: ObjectId = format!("osc:{}:sha256:{}", kind.tag(),hex_digest).parse()?;
            out.push(id);
        } else { bail!("unsupported object storage entry"); }
    }
    Ok(())
}
fn read_u16(r: &mut impl Read) -> Result<u16> { let mut b = [0; 2]; r.read_exact(&mut b)?; Ok(u16::from_le_bytes(b)) }
fn read_u64(r: &mut impl Read) -> Result<u64> { let mut b = [0; 8]; r.read_exact(&mut b)?; Ok(u64::from_le_bytes(b)) }
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all().with_context(|| format!("sync directory {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_and_corruption() {
        let tmp = tempfile::tempdir().unwrap();
        let cas = FsObjectStore::new(tmp.path());
        let a = cas.put_bytes(ObjectKind::Blob, b"example").unwrap();
        assert_eq!(a, cas.put_bytes(ObjectKind::Blob, b"example").unwrap());
        assert_eq!(cas.get(a).unwrap(), b"example");
        let p = cas.object_path(a);
        let mut bytes = fs::read(&p).unwrap(); bytes[HEADER_LEN as usize] ^= 1;
        fs::write(p, bytes).unwrap();
        assert!(cas.verify(a).is_err());
        assert!(cas.put_bytes(ObjectKind::Blob, b"example").is_err());
    }
    #[test]
    fn empty_blob_and_size_mismatch() {
        let t = tempfile::tempdir().unwrap(); let cas = FsObjectStore::new(t.path());
        let id = cas.put_bytes(ObjectKind::Blob, b"").unwrap();
        assert!(cas.get(id).unwrap().is_empty());
        assert!(cas.put_reader(ObjectKind::Blob, &mut io::Cursor::new(b"x"), 2).is_err());
    }
}
