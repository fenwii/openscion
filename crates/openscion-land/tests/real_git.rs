//! End-to-end Git contract: source workspace -> candidate -> validation -> CAS.
use anyhow::{bail,Result};
use openscion_change::{ChangeService,SubmitChange};
use openscion_land::{LandService,PreviewRequest};
use openscion_policy::Policy;
use openscion_registry::{Principal,Registry};
use openscion_workspace::{WorkspaceManager,CreateRequest,CommitRequest};
use std::{fs,path::Path,process::Command};
fn git(repo:&Path,args:&[&str])->Result<String>{
 let o=Command::new("git").arg("-C").arg(repo).args(args).output()?;
 if !o.status.success(){bail!("git failed: {}",String::from_utf8_lossy(&o.stderr));}
 Ok(String::from_utf8(o.stdout)?.trim().into())
}
fn setup()->Result<(tempfile::TempDir,Registry,Principal,String,std::path::PathBuf)>{
 let t=tempfile::tempdir()?;let repo=t.path().join("gitrepo");fs::create_dir(&repo)?;
 git(&repo,&["init","--initial-branch=main"])?;
 fs::write(repo.join("a.txt"),"initial\n")?;
 git(&repo,&["add","a.txt"])?;
 git(&repo,&["-c","user.name=Test","-c","user.email=test@example.invalid","commit","-m","initial"])?;
 let (r,token)=Registry::init(&t.path().join("service"),t.path())?;
 let who=r.authenticate(&token)?.unwrap();
 let rec=r.register("demo",&repo)?;
 let land=LandService::new(r.clone());land.install_policy("demo",Policy::development("refs/heads/main"))?;
 Ok((t,r,who,rec.id,repo))
}
fn make_change(reg:&Registry,actor:&Principal,repo_id:&str,path:&str,content:&str,key:&str)->Result<String>{
 let mgr=WorkspaceManager::new(reg.clone());
 let ws=mgr.create(actor,CreateRequest{repository_id:repo_id.into(),base_revision:"refs/heads/main".into(),task_id:Some(key.into()),lease_seconds:None,idempotency_key:format!("create-{key}")})?;
 fs::write(ws.path.join(path),content)?;
 let result=mgr.commit(actor,&ws.id,CommitRequest{expected_head:ws.head_oid,message:format!("change {key}"),selected_paths:vec![path.into()],idempotency_key:format!("commit-{key}")})?;
 let cr=ChangeService::new(reg.clone()).submit(actor,SubmitChange{repository_id:repo_id.into(),source_workspace_id:ws.id,source_commit_oid:result.commit_oid,target_ref:"refs/heads/main".into(),title:format!("change {key}"),description:String::new(),idempotency_key:format!("submit-{key}")})?;
 Ok(cr.id)
}
#[test]
fn clean_landing_and_idempotence()->Result<()> {
 let (_tmp,reg,who,id,repo)=setup()?;
 let start=git(&repo,&["rev-parse","refs/heads/main"])?;
 let cr=make_change(&reg,&who,&id,"a.txt","changed\n","one")?;
 let land=LandService::new(reg.clone());
 let op=land.start(&who,PreviewRequest{change_id:cr,idempotency_key:"preview-one".into()})?;
 assert_eq!(op.state,"CANDIDATE_READY");
 let validated=land.validate(&who,&op.id)?;assert_eq!(validated.state,"VALIDATED");
 let landed=land.land(&who,&op.id)?;assert_eq!(landed.state,"MERGED");
 assert_eq!(landed.result_oid.as_deref(),landed.candidate_oid.as_deref());
 assert_ne!(git(&repo,&["rev-parse","refs/heads/main"])?,start);
 assert_eq!(land.land(&who,&op.id)?.result_oid,landed.result_oid);
 assert_eq!(fs::read_to_string(repo.join("a.txt"))?,"initial\n"); // no implicit checkout
 assert_eq!(git(&repo,&["show","main:a.txt"])?,"changed");
 Ok(())
}
#[test]
fn conflicting_changes_are_not_merged()->Result<()> {
 let (_tmp,reg,who,id,repo)=setup()?;
 let mgr=WorkspaceManager::new(reg.clone());
 let initial="refs/heads/main".to_string();
 let w=mgr.create(&who,CreateRequest{repository_id:id.clone(),base_revision:initial,task_id:None,lease_seconds:None,idempotency_key:"parallel-create".into()})?;
 fs::write(w.path.join("a.txt"),"second\n")?;
 let source2=mgr.commit(&who,&w.id,CommitRequest{expected_head:w.head_oid,message:"second change".into(),selected_paths:vec!["a.txt".into()],idempotency_key:"second-commit".into()})?;
 let second=ChangeService::new(reg.clone()).submit(&who,SubmitChange{repository_id:id.clone(),source_workspace_id:w.id,source_commit_oid:source2.commit_oid,target_ref:"refs/heads/main".into(),title:"second".into(),description:String::new(),idempotency_key:"second-submit".into()})?;
 let first=make_change(&reg,&who,&id,"a.txt","first\n","first")?;
 let land=LandService::new(reg.clone());
 let op=land.start(&who,PreviewRequest{change_id:first,idempotency_key:"first-preview".into()})?;
 land.validate(&who,&op.id)?;land.land(&who,&op.id)?;
 let tip=git(&repo,&["rev-parse","refs/heads/main"])?;
 let conflicting=land.start(&who,PreviewRequest{change_id:second.id,idempotency_key:"second-preview".into()})?;
 assert_eq!(conflicting.state,"CONFLICT");
 assert_eq!(git(&repo,&["rev-parse","refs/heads/main"])?,tip);
 Ok(())
}
#[test]
fn permission_and_evidence_gates()->Result<()> {
 let (_tmp,reg,who,id,_repo)=setup()?;
 let readonly_token=reg.add_principal("reader_one","reader")?;
 reg.grant("demo","reader_one")?;
 let reader=reg.authenticate(&readonly_token)?.unwrap();
 let cr=make_change(&reg,&who,&id,"a.txt","other\n","security")?;
 let land=LandService::new(reg.clone());
 assert!(land.start(&reader,PreviewRequest{change_id:cr.clone(),idempotency_key:"unauthorized".into()}).is_err());
 let op=land.start(&who,PreviewRequest{change_id:cr,idempotency_key:"authorized".into()})?;
 assert!(land.land(&who,&op.id).is_err()); // validation required
 Ok(())
}
#[test]
fn external_attestation_and_independent_review_are_required()->Result<()> {
 use openscion_land::TrustedAttestation;
 let (_tmp,reg,author,id,repo)=setup()?;
 let mut policy=Policy::development("refs/heads/main");
 policy.required_checks.push("unit-tests".into());policy.required_approvals=1;
 LandService::new(reg.clone()).install_policy("demo",policy)?;
 let validator_token=reg.add_principal("validator_one","validator")?;
 let reviewer_token=reg.add_principal("reviewer_one","writer")?;
 for name in ["validator_one","reviewer_one"] {reg.grant("demo",name)?;}
 reg.grant_change("demo","validator_one","attest")?;
 reg.grant_change("demo","reviewer_one","review")?;
 let validator=reg.authenticate(&validator_token)?.unwrap();
 let reviewer=reg.authenticate(&reviewer_token)?.unwrap();
 let change=make_change(&reg,&author,&id,"a.txt","new source\n","trusted")?;
 let land=LandService::new(reg.clone());
 let op=land.start(&author,PreviewRequest{change_id:change,idempotency_key:"trusted-preview".into()})?;
 let v=land.validate(&author,&op.id)?;assert_eq!(v.state,"VALIDATING");
 assert!(land.land(&author,&op.id).is_err());
 let proof=TrustedAttestation{check_name:"unit-tests".into(),
    candidate_oid:op.candidate_oid.clone().unwrap(),tree_oid:op.candidate_tree.clone().unwrap(),
    suite_digest:"a".repeat(64),runner_digest:"b".repeat(64),passed:true};
 let attested=land.attest(&validator,&op.id,proof)?;
 assert_eq!(attested.state,"VALIDATED");
 assert!(land.review(&author,&op.id,"APPROVE").is_err());
 land.review(&reviewer,&op.id,"APPROVE")?;
 let result=land.land(&author,&op.id)?;
 assert_eq!(result.state,"MERGED");
 assert_eq!(git(&repo,&["rev-parse","main"])?,result.candidate_oid.unwrap());
 Ok(())
}
