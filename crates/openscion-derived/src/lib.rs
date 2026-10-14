//! Deterministic first-parent batch derivation of V0.1 Merkle Trees.
//! Rebuilds directory structure from a normalized leaf manifest. This deliberately
//! does NOT reuse V0.1 apply_edits, which cannot create/prune arbitrary directories.
use anyhow::{bail, Result};
use openscion_cas::{ObjectReader, ObjectWriter};
use openscion_native::{validate_path,FileChange};
use openscion_tree::{load_tree,save_tree,Tree,TreeEntry};
use openscion_types::{FileMode,ObjectId,ObjectKind,RepoName};
use std::collections::BTreeMap;

pub const MAX_MANIFEST_FILES:usize=100_000;
pub const MAX_DEPTH:usize=128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Leaf {pub mode:FileMode,pub blob:ObjectId}
pub type Manifest=BTreeMap<Vec<u8>,Leaf>;

/// Extract only tracked leaf files; V0.1 empty directories are not valid Git
/// source states. Strictly bounded recursive traversal and verified CAS reads.
pub fn manifest_from_tree(store:&impl ObjectReader,root:ObjectId)->Result<Manifest>{
    let mut files=Manifest::new();
    descend(store,root,&[],0,&mut files)?;
    Ok(files)
}
fn descend(store:&impl ObjectReader,id:ObjectId,prefix:&[u8],depth:usize,out:&mut Manifest)->Result<()> {
    if depth>MAX_DEPTH {bail!("native Tree exceeds depth limit");}
    let tree=load_tree(store,id)?;
    if depth>0 && tree.entries.is_empty(){bail!("empty directory is incompatible with Git source Tree");}
    for entry in tree.entries {
        let mut path=prefix.to_vec();if !path.is_empty(){path.push(b'/');}path.extend_from_slice(entry.name.as_bytes());
        validate_path(&path)?;
        if entry.mode==FileMode::Directory {
            descend(store,entry.id,&path,depth+1,out)?;
        } else {
            if entry.id.kind!=ObjectKind::Blob {bail!("invalid native file object kind");}
            store.verify(entry.id)?;
            if out.len()>=MAX_MANIFEST_FILES || out.insert(path,Leaf{mode:entry.mode,blob:entry.id}).is_some(){bail!("invalid or oversized native manifest");}
        }
    }
    Ok(())
}

/// Diff two canonical leaf manifests, preserving raw path bytes and file modes.
pub fn difference(base:&Manifest,target:&Manifest)->Result<Vec<FileChange>>{
    if base.len()>MAX_MANIFEST_FILES||target.len()>MAX_MANIFEST_FILES {bail!("manifest size exceeded");}
    let mut result=Vec::new();
    for path in base.keys(){
        if !target.contains_key(path){result.push(FileChange::Delete{path:path.clone()});}
    }
    for (path,new) in target {
        if base.get(path)!=Some(new){
            result.push(FileChange::Put{path:path.clone(),mode:new.mode,blob:new.blob});
        }
    }
    result.sort_by(|a,b|a.path().cmp(b.path()));
    if result.len()>MAX_MANIFEST_FILES*2 {bail!("delta too large");}
    Ok(result)
}

/// Applies the *entire batch* before deriving Trees. Supports file->directory,
/// directory->file transitions, nested creation, and automatic empty-dir pruning.
pub fn apply_changes(base:&Manifest,changes:&[FileChange])->Result<Manifest>{
    let mut result=base.clone();let mut last:Option<&[u8]>=None;
    for c in changes {
        validate_path(c.path())?;
        if last.is_some_and(|p|p>=c.path()){bail!("duplicate or noncanonical file changes");}
        last=Some(c.path());
        match c {
            FileChange::Put{path,mode,blob}=>{
                if *mode==FileMode::Directory || blob.kind!=ObjectKind::Blob {bail!("invalid leaf mode or Blob kind");}
                result.insert(path.clone(),Leaf{mode:*mode,blob:*blob});
            }
            FileChange::Delete{path}=>{
                if result.remove(path).is_none() {bail!("deleting missing file in Changeset");}
            }
        }
        if result.len()>MAX_MANIFEST_FILES {bail!("manifest too large");}
    }
    validate_manifest(&result)?;
    Ok(result)
}
fn validate_manifest(manifest:&Manifest)->Result<()>{
    for (path,leaf) in manifest {
        validate_path(path)?;
        if leaf.mode==FileMode::Directory || leaf.blob.kind!=ObjectKind::Blob {bail!("invalid manifest entry");}
        // Adjacent prefix collisions after lexicographic sorting are enough to
        // detect file-as-parent conflicts (e.g. a and a/b).
        let mut prefix=Vec::new();
        let parts:Vec<_>=path.split(|x|*x==b'/').collect();
        for (i,part) in parts.iter().enumerate(){
            if i>0 {prefix.push(b'/');}
            prefix.extend_from_slice(part);
            if i+1<parts.len() && manifest.contains_key(&prefix) {bail!("file/directory collision");}
        }
    }
    Ok(())
}

enum Node {Dir(BTreeMap<RepoName,Node>), File(Leaf)}
impl Node {
    fn new_dir()->Self{Self::Dir(BTreeMap::new())}
    fn insert(&mut self,parts:&[&[u8]],leaf:Leaf)->Result<()>{
        if parts.is_empty(){bail!("empty path");}
        let name=RepoName::new(parts[0].to_vec())?;
        match self {
            Node::File(_)=>bail!("parent path is a file"),
            Node::Dir(children)=>{
                if parts.len()==1 {
                    if children.insert(name,Node::File(leaf)).is_some(){bail!("duplicate leaf or file/directory collision");}
                } else {
                    let child=children.entry(name).or_insert_with(Node::new_dir);
                    child.insert(&parts[1..],leaf)?;
                }
                Ok(())
            }
        }
    }
}
/// Materialize an entire manifest, storing immutable canonical V0.1 Trees.
/// Deduplicates content by hash and never emits empty non-root directories.
pub fn build_tree(store:&impl ObjectWriter,manifest:&Manifest)->Result<ObjectId>{
    if manifest.len()>MAX_MANIFEST_FILES {bail!("manifest exceeds file limit");}
    validate_manifest(manifest)?;
    let mut root=Node::new_dir();
    for (path,leaf) in manifest {
        store.verify(leaf.blob)?;
        let components:Vec<_>=path.split(|x|*x==b'/').collect();
        root.insert(&components,*leaf)?;
    }
    write_node(store,&root)
}
fn write_node(store:&impl ObjectWriter,node:&Node)->Result<ObjectId>{
    let Node::Dir(children)=node else {bail!("expected directory");};
    let mut entries=Vec::with_capacity(children.len());
    for (name,child) in children {
        let (mode,id)=match child {
            Node::File(leaf)=>(leaf.mode,leaf.blob),
            Node::Dir(_)=>(FileMode::Directory,write_node(store,child)?),
        };
        entries.push(TreeEntry{name:name.clone(),mode,id});
    }
    save_tree(store,&Tree::new(entries)?)
}

pub fn derive_root(store:&impl ObjectWriter,parent_root:Option<ObjectId>,changes:&[FileChange])->Result<ObjectId>{
    let base=if let Some(root)=parent_root {manifest_from_tree(store,root)?} else {Manifest::new()};
    let target=apply_changes(&base,changes)?;
    build_tree(store,&target)
}

/// Reconstruct and compare all tracked leaf paths and verified native Blob IDs.
pub fn verify_parity(store:&impl ObjectReader,root:ObjectId,expected:&Manifest)->Result<()> {
    let actual=manifest_from_tree(store,root)?;
    if &actual!=expected {bail!("SHADOW_TREE_MISMATCH");}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openscion_cas::FsObjectStore;
    use tempfile::tempdir;
    #[test]fn creates_nested_paths_and_prunes_empty_ancestors(){
        let tmp=tempdir().unwrap();let cas=FsObjectStore::new(tmp.path());
        let a=cas.put(ObjectKind::Blob,b"A").unwrap();let b=cas.put(ObjectKind::Blob,b"B").unwrap();
        let initial=derive_root(&cas,None,&[FileChange::Put{path:b"src/deep/a.rs".to_vec(),mode:FileMode::Regular,blob:a}]).unwrap();
        let next=derive_root(&cas,Some(initial),&[
            FileChange::Delete{path:b"src/deep/a.rs".to_vec()},
            FileChange::Put{path:b"README".to_vec(),mode:FileMode::Executable,blob:b},
        ]).unwrap_err();
        assert!(next.to_string().contains("noncanonical"));
        let next=derive_root(&cas,Some(initial),&[
            FileChange::Put{path:b"README".to_vec(),mode:FileMode::Executable,blob:b},
            FileChange::Delete{path:b"src/deep/a.rs".to_vec()},
        ]).unwrap();
        let got=manifest_from_tree(&cas,next).unwrap();
        assert_eq!(got.len(),1);assert_eq!(got.get(b"README".as_slice()).unwrap().blob,b);
        assert_eq!(manifest_from_tree(&cas,initial).unwrap().len(),1);
    }
    #[test]fn file_directory_transitions(){
        let t=tempdir().unwrap();let cas=FsObjectStore::new(t.path());let b=cas.put(ObjectKind::Blob,b"z").unwrap();
        let original=derive_root(&cas,None,&[FileChange::Put{path:b"a".to_vec(),mode:FileMode::Regular,blob:b}]).unwrap();
        let derived=derive_root(&cas,Some(original),&[
            FileChange::Delete{path:b"a".to_vec()},FileChange::Put{path:b"a/b".to_vec(),mode:FileMode::Regular,blob:b},
        ]).unwrap();
        assert!(manifest_from_tree(&cas,derived).unwrap().contains_key(b"a/b".as_slice()));
    }
}
