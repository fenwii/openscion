//! Black-box CLI smoke test. Cargo supplies the path to the built `osc` binary.
use std::{fs, process::Command};
use tempfile::tempdir;
fn run(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_osc")).args(args).output().unwrap();
    assert!(out.status.success(), "osc {:?} failed: {}",args,String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}
#[test]
fn cli_snapshot_roundtrip() {
    let tmp=tempdir().unwrap();
    let root=tmp.path().join("repo");
    let src=tmp.path().join("src");
    fs::create_dir(&src).unwrap(); fs::write(src.join("main.rs"),b"fn main() {}\n").unwrap();
    let rootstr=root.to_str().unwrap(); let srcstr=src.to_str().unwrap();
    run(&["init",rootstr]);
    run(&["snapshot","create","--repo",rootstr,"--from",srcstr,"--name","baseline"]);
    let list=run(&["snapshot","list","--repo",rootstr]);
    assert!(list.contains("baseline"));
    fs::write(src.join("main.rs"),b"fn main() { println!(\"hi\"); }\n").unwrap();
    run(&["snapshot","create","--repo",rootstr,"--from",srcstr,"--name","next"]);
    run(&["fsck","--repo",rootstr,"--all"]);
    let dest=tmp.path().join("export");
    run(&["snapshot","export","baseline","--repo",rootstr,"--to",dest.to_str().unwrap()]);
    assert_eq!(fs::read(dest.join("main.rs")).unwrap(),b"fn main() {}\n");
    let out=run(&["--json","snapshot","list","--repo",rootstr]);
    assert!(out.contains("snapshots"));
}
