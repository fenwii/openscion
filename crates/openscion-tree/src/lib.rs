//! Canonical V1 Merkle directory trees, deterministic traversal, local scanning.
use anyhow::{anyhow, bail, Context, Result};
use openscion_cas::{FsObjectStore, ObjectReader, ObjectWriter};
use openscion_types::{FileMode, ObjectId, ObjectKind, RepoName};
use std::{fs, io::{Cursor, Read}, path::Path};
#[cfg(unix)]
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};

const TREE_MAGIC: &[u8; 8] = b"OSCTREE\0";
const TREE_VERSION: u16 = 1;
pub const MAX_ENTRIES: usize = 100_000;
pub const MAX_DEPTH: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeEntry { pub name: RepoName, pub mode: FileMode, pub id: ObjectId }
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct Tree { pub entries: Vec<TreeEntry> }
impl Tree {
    pub fn new(mut entries: Vec<TreeEntry>) -> Result<Self> {
        entries.sort_by(|a,b| a.name.cmp(&b.name));
        let result = Self { entries };
        result.validate()?;
        Ok(result)
    }
    pub fn validate(&self) -> Result<()> {
        if self.entries.len() > MAX_ENTRIES { bail!("too many entries"); }
        let mut previous: Option<&RepoName> = None;
        for entry in &self.entries {
            RepoName::new(entry.name.as_bytes().to_vec())?;
            if entry.mode.expected_kind() != entry.id.kind { bail!("entry object kind mismatch"); }
            if previous.is_some_and(|p| p >= &entry.name) { bail!("unsorted or duplicate Tree entries"); }
            previous = Some(&entry.name);
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut out = Vec::with_capacity(14 + self.entries.len() * 48);
        out.extend_from_slice(TREE_MAGIC);
        out.extend_from_slice(&TREE_VERSION.to_le_bytes());
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for e in &self.entries {
            let name = e.name.as_bytes();
            out.extend_from_slice(&(name.len() as u32).to_le_bytes());
            out.extend_from_slice(name);
            out.extend_from_slice(&e.mode.number().to_le_bytes());
            out.push(1); // hash algorithm code 1 = SHA-256
            out.extend_from_slice(&e.id.digest);
        }
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 14 || &bytes[..8] != &TREE_MAGIC[..] { bail!("invalid Tree magic"); }
        let mut r = Cursor::new(&bytes[8..]);
        let version = read_u16(&mut r)?;
        if version != TREE_VERSION { bail!("unsupported Tree version {version}"); }
        let count = read_u32(&mut r)? as usize;
        if count > MAX_ENTRIES { bail!("too many Tree entries"); }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let length = read_u32(&mut r)? as usize;
            if length == 0 || length > 255 { bail!("invalid Tree name length"); }
            let mut name = vec![0u8; length]; r.read_exact(&mut name)?;
            let name = RepoName::new(name)?;
            let mode = FileMode::parse(read_u32(&mut r)?)?;
            let mut algorithm = [0u8; 1]; r.read_exact(&mut algorithm)?;
            if algorithm[0] != 1 { bail!("unknown Tree child hash algorithm"); }
            let mut digest = [0u8; 32]; r.read_exact(&mut digest)?;
            let id = ObjectId { kind: mode.expected_kind(), digest };
            entries.push(TreeEntry { name, mode, id });
        }
        if r.position() as usize != bytes.len() - 8 { bail!("trailing bytes after Tree"); }
        let tree = Self { entries }; tree.validate()?; Ok(tree)
    }
    pub fn id(&self) -> Result<ObjectId> { Ok(ObjectId::hash(ObjectKind::Tree, &self.encode()?)) }
}
fn read_u16(r: &mut impl Read) -> Result<u16> { let mut b=[0;2]; r.read_exact(&mut b)?; Ok(u16::from_le_bytes(b)) }
fn read_u32(r: &mut impl Read) -> Result<u32> { let mut b=[0;4]; r.read_exact(&mut b)?; Ok(u32::from_le_bytes(b)) }

pub fn load_tree(store: &impl ObjectReader, id: ObjectId) -> Result<Tree> {
    if id.kind != ObjectKind::Tree { bail!("expected Tree id: {id}"); }
    Tree::decode(&store.get(id)?)
}
pub fn save_tree(store: &impl ObjectWriter, tree: &Tree) -> Result<ObjectId> {
    store.put(ObjectKind::Tree, &tree.encode()?)
}

/// Store a directory recursively. Symlinks are stored as link target bytes, never followed.
/// Scanning a concurrently modified source is NOT a coherent point-in-time snapshot.
pub fn scan_dir(store: &FsObjectStore, root: &Path) -> Result<ObjectId> {
    if !root.is_dir() { bail!("not a directory: {}", root.display()); }
    scan_inner(store, root, 0)
}
#[cfg(unix)]
fn scan_inner(store: &FsObjectStore, dir: &Path, depth: usize) -> Result<ObjectId> {
    if depth > MAX_DEPTH { bail!("directory depth limit exceeded"); }
    let mut entries = Vec::new();
    for dirent in fs::read_dir(dir).with_context(|| format!("read directory {}", dir.display()))? {
        let dirent = dirent?;
        let os_name = dirent.file_name();
        if depth == 0 && os_name.as_bytes() == b".openscion" { continue; }
        let name = RepoName::new(os_name.as_bytes().to_vec())?;
        let path = dirent.path();
        let meta = fs::symlink_metadata(&path)?;
        let (mode, id) = if meta.file_type().is_symlink() {
            let target = fs::read_link(&path)?;
            let target_bytes = target.as_os_str().as_bytes();
            if target_bytes.len() > 4096 { bail!("symlink target too large: {}", path.display()); }
            (FileMode::Symlink, store.put(ObjectKind::Blob, target_bytes)?)
        } else if meta.is_dir() {
            (FileMode::Directory, scan_inner(store, &path, depth + 1)?)
        } else if meta.is_file() {
            let mode = if meta.permissions().mode() & 0o111 != 0 { FileMode::Executable } else { FileMode::Regular };
            (mode, store.put_blob_file(&path)?)
        } else { bail!("unsupported file type: {}", path.display()); };
        entries.push(TreeEntry { name, mode, id });
    }
    save_tree(store, &Tree::new(entries)?)
}
#[cfg(not(unix))]
fn scan_inner(_: &FsObjectStore, _: &Path, _: usize) -> Result<ObjectId> {
    bail!("OpenScion V0.1 currently supports Unix platforms only")
}

/// Resolve a path represented as separately validated raw components.
pub fn resolve(store: &impl ObjectReader, root: ObjectId, components: &[RepoName]) -> Result<TreeEntry> {
    if components.is_empty() { bail!("empty path"); }
    let mut cursor = root;
    for (index, component) in components.iter().enumerate() {
        let tree = load_tree(store, cursor)?;
        let entry = tree.entries.into_iter().find(|e| &e.name == component)
            .ok_or_else(|| anyhow!("missing path component at depth {index}"))?;
        if index == components.len() - 1 { return Ok(entry); }
        if entry.mode != FileMode::Directory { bail!("not a directory in path"); }
        cursor = entry.id;
    }
    unreachable!()
}

#[derive(Clone, Debug)]
pub enum Edit { Put { path: Vec<RepoName>, mode: FileMode, id: ObjectId }, Delete { path: Vec<RepoName> } }
impl Edit {
    fn path(&self) -> &[RepoName] { match self { Self::Put { path, .. } | Self::Delete { path } => path } }
}
/// Functional Merkle tree update. Rebuilds only modified directory paths.
pub fn apply_edits(store: &impl ObjectWriter, root: ObjectId, edits: &[Edit]) -> Result<ObjectId> {
    let mut current = root;
    for edit in edits {
        let path = edit.path();
        if path.is_empty() || path.len() > MAX_DEPTH { bail!("invalid edit path depth"); }
        current = apply_one(store, current, path, edit)?;
    }
    Ok(current)
}
fn apply_one(store: &impl ObjectWriter, tree_id: ObjectId, path: &[RepoName], edit: &Edit) -> Result<ObjectId> {
    let mut tree = load_tree(store, tree_id)?;
    let index = tree.entries.iter().position(|e| e.name == path[0]);
    if path.len() == 1 {
        match edit {
            Edit::Put { mode, id, .. } => {
                if mode.expected_kind() != id.kind { bail!("edit mode and object mismatch"); }
                let new = TreeEntry { name: path[0].clone(), mode: *mode, id: *id };
                if let Some(i) = index { tree.entries[i] = new; } else { tree.entries.push(new); }
            }
            Edit::Delete { .. } => {
                if let Some(i) = index { tree.entries.remove(i); } else { bail!("delete target missing"); }
            }
        }
    } else {
        let i = index.ok_or_else(|| anyhow!("parent directory absent"))?;
        if tree.entries[i].mode != FileMode::Directory { bail!("parent is not directory"); }
        tree.entries[i].id = apply_one(store, tree.entries[i].id, &path[1..], edit)?;
    }
    save_tree(store, &Tree::new(tree.entries)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    #[test]
    fn empty_tree_encoding_and_roundtrip() {
        let t = Tree::default(); let bytes = t.encode().unwrap();
        assert_eq!(&bytes[..8], b"OSCTREE\0");
        assert_eq!(bytes.len(), 14);
        assert_eq!(t.id().unwrap().hex(), "504660685d293a976e82fb8402e5deefe29d5636cb9aae0380de6bf4c364aa31");
        assert_eq!(Tree::decode(&bytes).unwrap(), t);
    }
    #[test]
    fn rejects_duplicate_and_trailing_bytes() {
        let name = RepoName::new(b"x".to_vec()).unwrap();
        let entry = TreeEntry { name, mode: FileMode::Regular, id: ObjectId::hash(ObjectKind::Blob, b"x") };
        assert!(Tree::new(vec![entry.clone(),entry]).is_err());
        let mut bytes = Tree::default().encode().unwrap(); bytes.push(1);
        assert!(Tree::decode(&bytes).is_err());
    }
    #[test]
    fn old_tree_survives_patch() {
        let td = tempdir().unwrap(); let cas = FsObjectStore::new(td.path());
        let a = cas.put(ObjectKind::Blob, b"a").unwrap();
        let b = cas.put(ObjectKind::Blob, b"b").unwrap();
        let name = RepoName::new(b"x".to_vec()).unwrap();
        let before = save_tree(&cas,&Tree::new(vec![TreeEntry { name: name.clone(), mode: FileMode::Regular, id:a }]).unwrap()).unwrap();
        let after = apply_edits(&cas,before,&[Edit::Put { path: vec![name],mode:FileMode::Regular,id:b }]).unwrap();
        assert_ne!(before, after);
        assert_eq!(load_tree(&cas,before).unwrap().entries[0].id,a);
        assert_eq!(load_tree(&cas,after).unwrap().entries[0].id,b);
    }
}
