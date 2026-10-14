use openscion_registry::{Principal,Registry};
use openscion_workspace::{WorkspaceManager,CreateRequest,CommitRequest};
use std::{fs,process::Command};

fn run(dir:&std::path::Path,args:&[&str])->String {
    let o=Command::new("git").current_dir(dir).args(args).output().unwrap();
    assert!(o.status.success(),"git {:?}: {}",args,String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap().trim().into()
}
#[test]
fn separate_worktrees_and_retained_commits() {
    let t=tempfile::tempdir().unwrap();
    let source=t.path().join("source");fs::create_dir(&source).unwrap();
    run(&source,&["init"]);
    run(&source,&["config","user.email","test@example.invalid"]);
    run(&source,&["config","user.name","Test"]);
    fs::write(source.join("file.txt"),b"initial\n").unwrap();
    run(&source,&["add","file.txt"]);run(&source,&["commit","-m","initial"]);
    let main_before=run(&source,&["rev-parse","HEAD"]);
    let (registry,_token)=Registry::init(&t.path().join("service"),t.path()).unwrap();
    let repo=registry.register("example",&source).unwrap();
    let admin=Principal{id:"admin".into(),role:"admin".into()};
    let manager=WorkspaceManager::new(registry);
    let create=|suffix:&str|manager.create(&admin,CreateRequest{
        repository_id:repo.id.clone(),base_revision:format!("git:sha1:{main_before}"),
        task_id:Some(suffix.into()),lease_seconds:Some(3600),idempotency_key:format!("create-{suffix}")
    }).unwrap();
    let left=create("left");let right=create("right");
    assert_ne!(left.path,right.path);
    fs::write(left.path.join("file.txt"),b"left\n").unwrap();
    fs::write(right.path.join("file.txt"),b"right\n").unwrap();
    assert!(manager.status(&admin,&left.id).unwrap().dirty);
    for (w,msg) in [(&left,"left"),(&right,"right")] {
        let req=CommitRequest{expected_head:format!("git:sha1:{main_before}"),message:msg.into(),
            selected_paths:vec!["file.txt".into()],idempotency_key:format!("commit-{msg}")};
        let result=manager.commit(&admin,&w.id,req.clone()).unwrap();
        assert_eq!(manager.commit(&admin,&w.id,req).unwrap().commit_oid,result.commit_oid);
        let gitref=format!("refs/openscion/workspaces/{}",w.id);
        assert_eq!(run(&source,&["rev-parse",&gitref]),result.commit_oid);
        manager.dispose(&admin,&w.id).unwrap();
    }
    assert_eq!(run(&source,&["rev-parse","HEAD"]),main_before);
    assert!(!left.path.exists() && !right.path.exists());
}
