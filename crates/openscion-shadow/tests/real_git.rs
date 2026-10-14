//! End-to-end V0.5 Git/native shadow parity on real local repositories.
use anyhow::{bail,Result};
use openscion_native::{ChangesetId,ChangesetStore};
use openscion_registry::Registry;
use openscion_shadow::ShadowService;
use openscion_cas::FsObjectStore;
use openscion_derived::manifest_from_tree;
use openscion_types::ObjectId;
use std::{fs,path::Path,process::Command};
use tempfile::tempdir;

fn git(dir:&Path,args:&[&str])->Result<String>{
    let output=Command::new("git").arg("-C").arg(dir).args(args)
        .env("GIT_AUTHOR_NAME","Shadow Test")
        .env("GIT_AUTHOR_EMAIL","shadow@example.invalid")
        .env("GIT_COMMITTER_NAME","Shadow Test")
        .env("GIT_COMMITTER_EMAIL","shadow@example.invalid")
        .output()?;
    if !output.status.success(){bail!("Git test command {:?} failed: {}",args,String::from_utf8_lossy(&output.stderr));}
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}
#[test]
fn merges_directory_transitions_and_parity()->Result<()> {
    let tmp=tempdir()?;
    let repo=tmp.path().join("source");fs::create_dir_all(&repo)?;
    git(&repo,&["init","-q"])?;
    git(&repo,&["config","user.name","Shadow Test"])?;
    git(&repo,&["config","user.email","shadow@example.invalid"])?;
    fs::write(repo.join("a"),b"old file")?;
    git(&repo,&["add","-A"])?;git(&repo,&["commit","-qm","initial"])?;
    let base=git(&repo,&["rev-parse","HEAD"])?;
    git(&repo,&["checkout","-qb","feature"])?;
    fs::remove_file(repo.join("a"))?;
    fs::create_dir_all(repo.join("a/deep"))?;
    fs::write(repo.join("a/deep/file.rs"),b"fn main() {}\n")?;
    git(&repo,&["add","-A"])?;git(&repo,&["commit","-qm","file to directory"])?;
    let feature=git(&repo,&["rev-parse","HEAD"])?;
    git(&repo,&["checkout","-q","-"])?;
    fs::write(repo.join("README.md"),b"documentation")?;
    git(&repo,&["add","-A"])?;git(&repo,&["commit","-qm","add docs"])?;
    git(&repo,&["merge","--no-ff","-m","merge feature","feature"])?;
    let tip=git(&repo,&["rev-parse","HEAD"])?;
    let baseline_tip=tip.clone();
    let data=tmp.path().join("service");
    let (registry,token)=Registry::init(&data,tmp.path())?;
    let who=registry.authenticate(&token)?.unwrap();
    let record=registry.register("source",&repo)?;
    let service=ShadowService::new(registry);
    service.enable(&who,&record.id,"HEAD").err().expect("HEAD is not a full tracked ref");
    let mainref=git(&repo,&["symbolic-ref","HEAD"])?;
    service.enable(&who,&record.id,&mainref)?;
    let jobs=service.work(&who,&record.id,1)?;
    assert_eq!(jobs.len(),1);
    assert_eq!(jobs[0].state,"VERIFIED", "{:?}",jobs[0]);
    let verified=service.mapping(&who,&record.id,&tip)?;
    assert_eq!(verified.state,"VERIFIED");
    let checked=service.verify_mapping(&who,&record.id,&tip)?;
    assert_eq!(checked.native_id,verified.native_id);
    let native_store=ChangesetStore::new(data.join("shadow-native").join(&record.id).join("changesets"));
    let cset=native_store.get(verified.native_id.parse::<ChangesetId>()?)?;
    assert_eq!(cset.parents.len(),2,"merge parents must be preserved");
    let cas=FsObjectStore::new(data.join("shadow-native").join(&record.id).join("cas"));
    let manifest=manifest_from_tree(&cas,verified.root_tree_id.parse::<ObjectId>()?)?;
    assert!(manifest.contains_key(b"a/deep/file.rs".as_slice()));
    assert!(!manifest.contains_key(b"a".as_slice()));
    assert!(manifest.contains_key(b"README.md".as_slice()));
    assert!(service.mapping(&who,&record.id,&base).is_ok());
    assert!(service.mapping(&who,&record.id,&feature).is_ok());
    assert_eq!(git(&repo,&["rev-parse","HEAD"])?,baseline_tip,"shadow processing must not move Git HEAD");
    let status=service.status(&who,&record.id)?;
    assert!(status.verified_mappings>=4);
    assert!(service.verify_sample(&who,&record.id,20)?.len()>=4);
    Ok(())
}

#[test]
fn shadow_access_requires_admin_for_mutations()->Result<()> {
    let tmp=tempdir()?;let repo=tmp.path().join("repo");fs::create_dir_all(&repo)?;
    git(&repo,&["init","-q"])?;
    fs::write(repo.join("hello.txt"),b"hello")?;
    git(&repo,&["add","-A"])?;git(&repo,&["commit","-qm","initial"])?;
    let (registry,admin)=Registry::init(&tmp.path().join("data"),tmp.path())?;
    let record=registry.register("repo",&repo)?;
    let token=registry.add_principal("reader","reader")?;
    registry.grant("repo","reader")?;
    let reader=registry.authenticate(&token)?.unwrap();
    let shadow=ShadowService::new(registry.clone());
    assert!(shadow.enable(&reader,&record.id,"refs/heads/main").is_err());
    assert!(!shadow.status(&reader,&record.id)?.enabled);
    let admin=registry.authenticate(&admin)?.unwrap();
    let branch=git(&repo,&["symbolic-ref","HEAD"])?;
    assert!(shadow.enable(&admin,&record.id,&branch).is_ok());
    assert!(shadow.enqueue(&reader,&record.id,&git(&repo,&["rev-parse","HEAD"])?).is_err());
    Ok(())
}
