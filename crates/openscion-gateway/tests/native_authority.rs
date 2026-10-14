//! A real-Git operator flow. Requires Git and cargo; never modifies external repositories.
use anyhow::{bail,Result};
use openscion_authority::{AuthorityMode,AuthorityService};
use openscion_gateway::GatewayService;
use openscion_git::GitRepository;
use openscion_native_ref::{NativeRefService,CasRequest};
use openscion_operations::BackupService;
use openscion_registry::{Principal,Registry};
use openscion_shadow::ShadowService;
use std::{fs,path::Path,process::Command};
fn git(dir:&Path,args:&[&str])->Result<String>{
 let output=Command::new("git").arg("-C").arg(dir).args(args).output()?;
 if !output.status.success(){bail!("git test fixture failed: {:?}",args);}
 Ok(String::from_utf8(output.stdout)?.trim().to_string())
}
#[test]
fn imported_history_preview_cas_mirror_and_safe_fallback()->Result<()> {
 let temp=tempfile::tempdir()?;
 let repo=temp.path().join("source");fs::create_dir(&repo)?;
 git(&repo,&["init","-b","main"])?;
 git(&repo,&["config","user.name","Test Author"])?;
 git(&repo,&["config","user.email","test@example.invalid"])?;
 fs::write(repo.join("README.md"),"base\n")?;
 git(&repo,&["add","README.md"])?;git(&repo,&["commit","-m","base"])?;
 let base=git(&repo,&["rev-parse","HEAD"])?;
 git(&repo,&["checkout","-b","feature"])?;
 fs::write(repo.join("README.md"),"feature\n")?;
 git(&repo,&["add","README.md"])?;git(&repo,&["commit","-m","feature"])?;
 let feature=git(&repo,&["rev-parse","HEAD"])?;
 git(&repo,&["checkout","main"])?;
 let (reg,_)=Registry::init(&temp.path().join("service"),temp.path())?;
 let rr=reg.register("demo",&repo)?;
 let admin=Principal{id:"admin".into(),role:"admin".into()};
 let shadow=ShadowService::new(reg.clone());
 shadow.enable(&admin,&rr.id,"refs/heads/main")?;
 let gitrepo=GitRepository::open(&repo)?;
 shadow.enqueue(&admin,&rr.id,&format!("git:{}:{feature}",gitrepo.algorithm.label()))?;
 let results=shadow.work(&admin,&rr.id,2)?;
 assert_eq!(results.len(),2);
 assert!(results.iter().all(|j|j.state=="VERIFIED"));
 let auth=AuthorityService::new(reg.clone());
 auth.qualify_preview(&admin,&rr.id)?;
 let backup=BackupService::new(reg.clone()).create_offline(&admin,&temp.path().join("backups"),true)?;
 let backup_root=temp.path().join("backups").join(format!("backup-{}",backup.backup_id));
 BackupService::verify(&backup_root)?;
 let (git_tip,native_tip)=auth.preflight(&admin,&rr.id,"refs/heads/main")?;
 assert!(git_tip.ends_with(&base));
 let frozen=auth.freeze(&admin,&rr.id,0,&git_tip,&native_tip,&backup.backup_id,true)?;
 assert_eq!(frozen.mode,AuthorityMode::CutoverFrozen);
 let refs=NativeRefService::new(reg.clone());
 let initial=refs.bootstrap(&admin,&rr.id,"refs/heads/main",frozen.epoch)?;
 assert_eq!(initial.target_changeset_id,native_tip);
 auth.set_preview_flag(&admin,true)?;
 let preview=auth.promote_preview(&admin,&rr.id,frozen.epoch,true)?;
 assert_eq!(preview.mode,AuthorityMode::NativeAuthorityPreview);
 assert!(auth.assert_git_writer(&rr.id).is_err());
 let after=refs.read(&admin,&rr.id,"refs/heads/main")?;
 let native_feature=shadow.mapping(&admin,&rr.id,&feature)?.native_id;
 let update=CasRequest{repo_id:rr.id.clone(),ref_name:"refs/heads/main".into(),
  expected_target:after.target_changeset_id.clone(),expected_generation:after.generation,
  authority_epoch:preview.epoch,new_target:native_feature.clone(),
  idempotency_key:"feature-accept".into(),policy_evidence:"test-policy-evidence".into()};
 let advanced=refs.cas(&admin,update.clone())?;
 assert_eq!(advanced.generation,1);
 assert_eq!(refs.cas(&admin,update)?.generation,1); // idempotent retry
 let gateway=GatewayService::new(reg.clone());
 assert!(gateway.fallback_to_git(&admin,&rr.id,preview.epoch,true).is_err()); // mirror not caught up
 let mirror=gateway.sync_verified(&admin,&rr.id,"refs/heads/main")?;
 assert_eq!(mirror.mirror_lag,0);
 assert_eq!(mirror.native_id,native_feature);
 let finished=gateway.fallback_to_git(&admin,&rr.id,preview.epoch,true)?;
 assert_eq!(finished.mode,AuthorityMode::GitAuthority);
 assert_eq!(git(&repo,&["rev-parse","refs/heads/main"])?,feature);
 Ok(())
}
