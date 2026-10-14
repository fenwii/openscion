//! Read-only, pinned Git object queries. No shell, fetch, checkout, hooks or writes.
//! V0.2 uses Git CLI via a replaceable backend. Git repository files remain authoritative.
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::Serialize;
use std::{collections::BTreeMap, fs, io::Read, path::{Path,PathBuf}, process::{Command,Stdio}, str, thread, time::Duration};
use wait_timeout::ChildExt;

const MAX_OUTPUT: usize = 32 * 1024 * 1024;
const MAX_STDERR: u64 = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all="lowercase")]
pub enum HashAlgorithm { Sha1, Sha256 }
impl HashAlgorithm {
    pub fn hex_len(self) -> usize { match self { Self::Sha1 => 40, Self::Sha256 => 64 } }
    pub fn label(self) -> &'static str { match self { Self::Sha1 => "sha1", Self::Sha256 => "sha256" } }
}
#[derive(Clone, Debug, Serialize)]
pub struct RepositoryInfo { pub kind: String, pub object_format: HashAlgorithm, pub head: Option<String>, pub shallow: bool }
#[derive(Clone, Debug, Serialize)]
pub struct CommitInfo {
    pub oid: String, pub tree_oid: String, pub parents: Vec<String>,
    pub author_b64: Option<String>, pub committer_b64: Option<String>,
    pub message_b64: String, pub message_display: String,
    pub raw_b64: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct TreeEntry {
    pub name_b64: String, pub name_display: String, pub mode: String,
    pub kind: String, pub oid: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct TreeInfo { pub revision: String, pub path_b64: String, pub tree_oid: String, pub entries: Vec<TreeEntry> }
#[derive(Clone, Debug, Serialize)]
pub struct FileInfo { pub revision: String, pub path_b64: String, pub mode: String, pub blob_oid: String, pub content: Vec<u8> }
#[derive(Clone, Debug, Serialize)]
pub struct PathChange { pub path_b64: String, pub path_display: String, pub status: String,
    pub old_mode: String, pub new_mode: String, pub old_oid: String, pub new_oid: String }
#[derive(Clone, Debug, Serialize)]
pub struct HistoryPage { pub start: String, pub skip: usize, pub limit: usize,
    pub commits: Vec<String>, pub next_skip: Option<usize>, pub shallow_repository: bool }
#[derive(Clone, Debug, Serialize)]
pub struct RefInfo { pub name: String, pub oid: String }

fn valid_oid(value: &str, alg: HashAlgorithm) -> Result<String> {
    let raw = value.strip_prefix(&format!("git:{}:", alg.label())).unwrap_or(value);
    if raw.len() != alg.hex_len() || !raw.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { bail!("invalid Git OID"); }
    Ok(raw.to_owned())
}
fn fmt_oid(hex: &str, alg: HashAlgorithm) -> String { format!("git:{}:{hex}", alg.label()) }
fn valid_ref(name: &str) -> Result<()> {
    if name == "HEAD" { return Ok(()); }
    if !name.starts_with("refs/") || name.len() > 512 || name.bytes().any(|b| b < 0x21 || b >= 0x7f || b == b'\\')
      || name.contains("..") || name.contains("@{") || name.contains("//") || name.ends_with('/')
      || name.ends_with('.') || name.ends_with(".lock") || name.contains('~') || name.contains('^')
      || name.contains(':') || name.contains('?') || name.contains('*') || name.contains('[') { bail!("invalid full reference name"); }
    Ok(())
}
fn components(input: &[u8]) -> Result<Vec<&[u8]>> {
    if input.is_empty() { return Ok(Vec::new()); }
    if input.len() > 4096 || input[0] == b'/' || input.iter().any(|b| *b == 0) { bail!("invalid Git path"); }
    let parts: Vec<_> = input.split(|b| *b == b'/').collect();
    if parts.len() > 128 || parts.iter().any(|p| p.is_empty() || *p == b"." || *p == b"..") { bail!("invalid Git path component"); }
    Ok(parts)
}
fn output_to_str(data: &[u8]) -> Result<&str> { str::from_utf8(data).context("Git metadata is not valid UTF-8") }
fn parse_hex(line: &[u8], alg: HashAlgorithm) -> Result<String> {
    let h=output_to_str(line)?.trim(); valid_oid(h,alg)
}

#[derive(Clone, Debug)]
pub struct GitRepository { path: PathBuf, pub algorithm: HashAlgorithm }
impl GitRepository {
    pub fn open(path: &Path) -> Result<Self> {
        let path = fs::canonicalize(path).context("Git repository path unavailable")?;
        if !path.is_dir() { bail!("Git repository path is not a directory"); }
        let preliminary = Self { path, algorithm: HashAlgorithm::Sha1 };
        let bare = preliminary.run(&["rev-parse", "--is-bare-repository"], 256)?;
        let bare = output_to_str(&bare)?.trim();
        if bare != "true" && bare != "false" { bail!("not a supported Git repository"); }
        let fmt = preliminary.run(&["rev-parse", "--show-object-format=storage"], 256)?;
        let algorithm=match output_to_str(&fmt)?.trim() { "sha1" => HashAlgorithm::Sha1, "sha256" => HashAlgorithm::Sha256, _ => bail!("unsupported Git object format") };
        let repo=Self { algorithm, ..preliminary };
        // A registered path must designate the repository root, not an arbitrary descendant.
        let location=if bare=="true" {
            repo.run(&["rev-parse","--absolute-git-dir"],4096)?
        } else {
            repo.run(&["rev-parse","--show-toplevel"],4096)?
        };
        let location=std::path::Path::new(output_to_str(&location)?.trim()).canonicalize()?;
        if location!=repo.path { bail!("registered path must be Git repository root"); }
        // Git alternates may reference object stores outside the admin allow-root;
        // reject this feature until an explicit cross-store trust policy exists.
        let alt=repo.run(&["rev-parse","--git-path","objects/info/alternates"],4096)?;
        let alt=std::path::Path::new(output_to_str(&alt)?.trim());
        let alt=if alt.is_absolute(){alt.to_path_buf()}else{repo.path.join(alt)};
        if alt.is_file(){bail!("Git alternate object stores are unsupported in V0.2");}
        // Avoid unexpected lazy network fetches on Git versions predating
        // GIT_NO_LAZY_FETCH. Promisor repositories are explicitly unsupported.
        let promisor=repo.run_status(&["config","--get-regexp",r"^remote\..*\.promisor$"],2048,true)?;
        if !promisor.is_empty() {bail!("promisor/partial-clone repositories are unsupported in V0.2");}
        // Reject Git replace refs, which would change identity-dependent history semantics.
        let replace=repo.run(&["for-each-ref", "--format=%(refname)", "refs/replace/"], 4096)?;
        if !replace.is_empty() { bail!("Git replace refs are unsupported in V0.2"); }
        Ok(repo)
    }
    pub fn path(&self) -> &Path { &self.path }
    /// Fixed Git subcommands only; no shell, no credentials, no implicit network operations.
    fn run(&self, args: &[&str], limit: usize) -> Result<Vec<u8>> {
        self.run_status(args,limit,false)
    }
    fn run_status(&self,args:&[&str],limit:usize,allow_one:bool)->Result<Vec<u8>> {
        let mut command=Command::new("git");
        command.env_clear().env("PATH", std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()))
            .env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_OPTIONAL_LOCKS", "0").env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_TERMINAL_PROMPT", "0").env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_LFS_SKIP_SMUDGE", "1").env("GIT_NO_LAZY_FETCH", "1")
            .arg("--no-pager").arg("-C").arg(&self.path).args(args)
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child=command.spawn().context("unable to spawn Git executable")?;
        let stdout=child.stdout.take().ok_or_else(||anyhow!("Git stdout pipe unavailable"))?;
        let stderr=child.stderr.take().ok_or_else(||anyhow!("Git stderr pipe unavailable"))?;
        let max=limit.min(MAX_OUTPUT) as u64;
        let output_thread=thread::spawn(move || { let mut out=Vec::new(); let result=stdout.take(max+1).read_to_end(&mut out); (result,out) });
        let error_thread=thread::spawn(move || { let mut out=Vec::new(); let result=stderr.take(MAX_STDERR+1).read_to_end(&mut out); (result,out) });
        let status=match child.wait_timeout(TIMEOUT)? {
            Some(status)=>status,
            None=>{ let _=child.kill(); let _=child.wait(); bail!("Git operation timed out"); }
        };
        let (read_result,out)=output_thread.join().map_err(|_|anyhow!("Git output reader failed"))?;
        let (error_result,err)=error_thread.join().map_err(|_|anyhow!("Git error reader failed"))?;
        read_result?; error_result?;
        if out.len() as u64 > max { bail!("Git output exceeds configured limit"); }
        if err.len() as u64 > MAX_STDERR { bail!("Git diagnostic output too large"); }
        if !status.success() && !(allow_one && status.code()==Some(1)) {
            // Git stderr is intentionally not returned to API clients: it may include host paths.
            bail!("Git command failed (exit {:?})",status.code());
        }
        Ok(out)
    }
    pub fn info(&self)->Result<RepositoryInfo> {
        let kind=self.run(&["rev-parse","--is-bare-repository"],256)?;
        let shallow=self.run(&["rev-parse","--is-shallow-repository"],256)?;
        let head=self.resolve_ref("HEAD").ok();
        Ok(RepositoryInfo { kind:if output_to_str(&kind)?.trim()=="true" {"bare"}else{"working"}.into(),object_format:self.algorithm,
            head,shallow:output_to_str(&shallow)?.trim()=="true" })
    }
    pub fn validate_oid(&self,oid:&str)->Result<String> { valid_oid(oid,self.algorithm) }
    pub fn resolve_ref(&self,name:&str)->Result<String> {
        valid_ref(name)?;
        let value=if name=="HEAD" { self.run(&["rev-parse","--verify","HEAD^{commit}"],256)? }
        else { self.run(&["show-ref","--verify","--hash",name],256)? };
        Ok(fmt_oid(&parse_hex(&value,self.algorithm)?,self.algorithm))
    }
    pub fn refs(&self)->Result<Vec<RefInfo>> {
        let raw=self.run_status(&["show-ref"],4*1024*1024,true)?;
        let mut refs=Vec::new();
        for line in raw.split(|b|*b==b'\n').filter(|x|!x.is_empty()) {
            let (oid,name)=line.split_once_bytes(b' ').ok_or_else(||anyhow!("invalid show-ref output"))?;
            let oid=parse_hex(oid,self.algorithm)?;
            let name=output_to_str(name)?; valid_ref(name)?;
            refs.push(RefInfo {name:name.to_string(),oid:fmt_oid(&oid,self.algorithm)});
        }
        Ok(refs)
    }
    pub fn object_type(&self,oid:&str)->Result<String> {
        let oid=self.validate_oid(oid)?;
        let raw=self.run(&["cat-file","-t",&oid],128)?;
        let value=output_to_str(&raw)?.trim();
        if !["commit","tree","blob","tag"].contains(&value) { bail!("unsupported Git object type"); }
        Ok(value.into())
    }
    pub fn read_commit(&self,oid:&str)->Result<CommitInfo> {
        let oid=self.validate_oid(oid)?;
        let bytes=self.run(&["cat-file","commit",&oid],8*1024*1024)?;
        let pos=bytes.windows(2).position(|w|w==b"\n\n").ok_or_else(||anyhow!("invalid Git commit header"))?;
        let mut tree=None; let mut parents=Vec::new(); let mut author=None; let mut committer=None;
        for line in bytes[..pos].split(|b|*b==b'\n') {
            if let Some(raw)=line.strip_prefix(b"tree ") { tree=Some(fmt_oid(&parse_hex(raw,self.algorithm)?,self.algorithm)); }
            if let Some(raw)=line.strip_prefix(b"parent ") { parents.push(fmt_oid(&parse_hex(raw,self.algorithm)?,self.algorithm)); }
            if let Some(raw)=line.strip_prefix(b"author ") { author=Some(URL_SAFE_NO_PAD.encode(raw)); }
            if let Some(raw)=line.strip_prefix(b"committer ") { committer=Some(URL_SAFE_NO_PAD.encode(raw)); }
        }
        let message=&bytes[pos+2..];
        Ok(CommitInfo { oid:fmt_oid(&oid,self.algorithm),tree_oid:tree.ok_or_else(||anyhow!("Git commit missing tree"))?,
            parents,author_b64:author,committer_b64:committer,message_b64:URL_SAFE_NO_PAD.encode(message),
            message_display:String::from_utf8_lossy(message).to_string(),raw_b64:URL_SAFE_NO_PAD.encode(&bytes) })
    }
    pub fn tree_entries(&self,oid:&str)->Result<Vec<TreeEntry>> {
        let oid=self.validate_oid(oid)?;
        // -z prevents lossy name quoting. -z has a stable raw-byte format.
        let bytes=self.run(&["ls-tree","-z",&oid],MAX_OUTPUT)?;
        let mut entries=Vec::new();
        for item in bytes.split(|b|*b==0).filter(|p|!p.is_empty()) {
            let (meta,name)=item.split_once_bytes(b'\t').ok_or_else(||anyhow!("invalid Git Tree output"))?;
            let mut parts=meta.split(|b|*b==b' ');
            let mode=output_to_str(parts.next().ok_or_else(||anyhow!("missing mode"))?)?.to_string();
            let kind=output_to_str(parts.next().ok_or_else(||anyhow!("missing kind"))?)?.to_string();
            let digest=parse_hex(parts.next().ok_or_else(||anyhow!("missing OID"))?,self.algorithm)?;
            if parts.next().is_some() || !["tree","blob","commit"].contains(&kind.as_str()) { bail!("invalid Git Tree entry"); }
            entries.push(TreeEntry {name_b64:URL_SAFE_NO_PAD.encode(name),name_display:String::from_utf8_lossy(name).to_string(),
                mode,kind,oid:fmt_oid(&digest,self.algorithm)});
        }
        Ok(entries)
    }
    fn resolve_path(&self,commit_oid:&str,path:&[u8])->Result<(String,Option<TreeEntry>)> {
        let commit=self.read_commit(commit_oid)?;
        let mut tree_oid=commit.tree_oid;
        let parts=components(path)?;
        if parts.is_empty() { return Ok((tree_oid,None)); }
        for (index,part) in parts.iter().enumerate() {
            let entries=self.tree_entries(&tree_oid)?;
            let entry=entries.into_iter().find(|e|URL_SAFE_NO_PAD.decode(&e.name_b64).map_or(false,|n|n==*part))
                .ok_or_else(||anyhow!("path not found in Git revision"))?;
            if index+1==parts.len() { return Ok((tree_oid,Some(entry))); }
            if entry.kind!="tree" { bail!("path component is not a directory"); }
            tree_oid=entry.oid.clone();
        }
        unreachable!()
    }
    pub fn tree_at(&self,commit_oid:&str,path:&[u8])->Result<TreeInfo> {
        let commit_oid=self.validate_oid(commit_oid)?;
        let (root,found)=self.resolve_path(&commit_oid,path)?;
        let tree_oid=if let Some(e)=found {if e.kind!="tree" {bail!("requested path is not a directory");} e.oid} else {root};
        let entries=self.tree_entries(&tree_oid)?;
        Ok(TreeInfo {revision:fmt_oid(&commit_oid,self.algorithm),path_b64:URL_SAFE_NO_PAD.encode(path),tree_oid,entries})
    }
    pub fn file_at(&self,commit_oid:&str,path:&[u8],max:usize)->Result<FileInfo> {
        if path.is_empty(){bail!("file path required");}
        let commit_oid=self.validate_oid(commit_oid)?;
        let (_,entry)=self.resolve_path(&commit_oid,path)?;
        let entry=entry.ok_or_else(||anyhow!("file path not found"))?;
        if entry.kind!="blob" {bail!("requested path is not a Blob");}
        let blob_oid=self.validate_oid(&entry.oid)?;
        let size=self.run(&["cat-file","-s",&blob_oid],256)?;
        let size:usize=output_to_str(&size)?.trim().parse().context("invalid Git blob size")?;
        if size>max.min(MAX_OUTPUT){bail!("Blob exceeds API size limit");}
        let content=self.run(&["cat-file","blob",&blob_oid],max)?;
        if content.len()!=size {bail!("Git Blob size mismatch");}
        Ok(FileInfo {revision:fmt_oid(&commit_oid,self.algorithm),path_b64:URL_SAFE_NO_PAD.encode(path),mode:entry.mode,
            blob_oid:fmt_oid(&blob_oid,self.algorithm),content})
    }
    pub fn merge_bases(&self,left:&str,right:&str)->Result<Vec<String>> {
        let a=self.validate_oid(left)?;let b=self.validate_oid(right)?;
        let out=self.run_status(&["merge-base","--all",&a,&b],16*1024,true)?;
        out.split(|c|*c==b'\n').filter(|x|!x.is_empty())
            .map(|line|Ok(fmt_oid(&parse_hex(line,self.algorithm)?,self.algorithm))).collect()
    }
    pub fn history(&self,start:&str,skip:usize,limit:usize,first_parent:bool)->Result<HistoryPage> {
        if limit==0 || limit>500 || skip>10000 { bail!("history pagination limits exceeded"); }
        let oid=self.validate_oid(start)?;
        let count=(limit+1).to_string(); let skip_str=skip.to_string();
        let mut args=vec!["rev-list","--topo-order"];
        if first_parent {args.push("--first-parent");}
        let max_arg=format!("--max-count={count}");let skip_arg=format!("--skip={skip_str}");
        args.push(&max_arg);args.push(&skip_arg);args.push(&oid);
        let bytes=self.run(&args,128*1024)?;
        let mut all:Vec<String>=bytes.split(|b|*b==b'\n').filter(|x|!x.is_empty())
            .map(|x|parse_hex(x,self.algorithm).map(|v|fmt_oid(&v,self.algorithm))).collect::<Result<_>>()?;
        let more=all.len()>limit;all.truncate(limit);
        let shallow=self.info()?.shallow;
        Ok(HistoryPage {start:fmt_oid(&oid,self.algorithm),skip,limit,commits:all,
            next_skip:if more{Some(skip+limit)}else{None},shallow_repository:shallow})
    }
    /// A bounded patch presentation; binary and text-conversion helpers are disabled.
    pub fn diff_patch(&self,base:&str,target:&str)->Result<Vec<u8>> {
        let a=self.validate_oid(base)?;let b=self.validate_oid(target)?;
        self.run(&["diff","--no-ext-diff","--no-textconv","--no-renames","--no-color",
            "--unified=3",&a,&b,"--"],8*1024*1024)
    }
    pub fn changed_paths(&self,base:&str,target:&str)->Result<Vec<PathChange>> {
        let a=self.validate_oid(base)?; let b=self.validate_oid(target)?;
        let bytes=self.run(&["diff-tree","--raw","--no-commit-id","-r","-z","--no-renames",&a,&b,"--"],MAX_OUTPUT)?;
        let mut results=Vec::new(); let mut chunks=bytes.split(|x|*x==0).filter(|p|!p.is_empty());
        while let Some(header)=chunks.next() {
            let header=header.strip_prefix(b":").ok_or_else(||anyhow!("invalid diff-tree raw header"))?;
            let parts:Vec<_>=header.split(|b|*b==b' ').collect();
            if parts.len()!=5 {bail!("invalid Git raw diff record");}
            let path=chunks.next().ok_or_else(||anyhow!("missing diff path"))?;
            let status=output_to_str(parts[4])?;
            results.push(PathChange {path_b64:URL_SAFE_NO_PAD.encode(path),path_display:String::from_utf8_lossy(path).to_string(),
                status:status.to_string(), old_mode:output_to_str(parts[0])?.into(),new_mode:output_to_str(parts[1])?.into(),
                old_oid:output_to_str(parts[2])?.into(),new_oid:output_to_str(parts[3])?.into()});
            if results.len()>10000 {bail!("too many changed paths");}
        }
        Ok(results)
    }
}
trait SplitBytes { fn split_once_bytes(&self,separator:u8)->Option<(&[u8],&[u8])>; }
impl SplitBytes for [u8] {
    fn split_once_bytes(&self,separator:u8)->Option<(&[u8],&[u8])> {
        let p=self.iter().position(|b|*b==separator)?;Some((&self[..p],&self[p+1..]))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    fn run_test(dir:&Path,args:&[&str]) { let status=Command::new("git").arg("-C").arg(dir).args(args).status().unwrap();assert!(status.success()); }
    #[test]
    fn git_read_only_round_trip() {
        let t=tempdir().unwrap();run_test(t.path(),&["init","-q"]);
        run_test(t.path(),&["config","user.email","test@example.com"]);run_test(t.path(),&["config","user.name","Test"]);
        fs::write(t.path().join("hello.txt"),b"hello\0binary").unwrap();
        run_test(t.path(),&["add","hello.txt"]);run_test(t.path(),&["commit","-qm","first"]);
        let repo=GitRepository::open(t.path()).unwrap(); let head=repo.resolve_ref("HEAD").unwrap();
        let commit=repo.read_commit(&head).unwrap();assert!(commit.parents.is_empty());
        let file=repo.file_at(&head,b"hello.txt",4096).unwrap();assert_eq!(file.content,b"hello\0binary");
        assert_eq!(repo.tree_at(&head,b"").unwrap().entries.len(),1);
        assert_eq!(repo.history(&head,0,10,false).unwrap().commits.len(),1);
        assert!(repo.merge_bases(&head,&head).unwrap().len()==1);
        fs::write(t.path().join("hello.txt"),b"next").unwrap();run_test(t.path(),&["add","hello.txt"]);
        run_test(t.path(),&["commit","-qm","second"]);
        let next=repo.resolve_ref("HEAD").unwrap();
        assert_eq!(repo.changed_paths(&head,&next).unwrap().len(),1);
    }
    #[test]
    fn bare_repository_and_nested_tree() {
        let t=tempdir().unwrap();
        let bare=t.path().join("bare.git");
        let status=Command::new("git").args(["init","--bare","-q"]).arg(&bare).status().unwrap();
        assert!(status.success());
        let repo=GitRepository::open(&bare).unwrap();
        assert_eq!(repo.info().unwrap().kind,"bare");
        assert!(repo.info().unwrap().head.is_none());
        let work=t.path().join("work"); fs::create_dir(&work).unwrap();
        run_test(&work,&["init","-q"]);
        run_test(&work,&["config","user.email","test@example.com"]);
        run_test(&work,&["config","user.name","Test"]);
        fs::create_dir(work.join("nested")).unwrap();
        fs::write(work.join("nested").join("test.rs"),b"fn main() {}\n").unwrap();
        run_test(&work,&["add","."]);run_test(&work,&["commit","-qm","nested"]);
        let git=GitRepository::open(&work).unwrap();
        let head=git.resolve_ref("HEAD").unwrap();
        let entries=git.tree_at(&head,b"nested").unwrap().entries;
        assert_eq!(entries.len(),1);
        assert_eq!(URL_SAFE_NO_PAD.decode(&entries[0].name_b64).unwrap(),b"test.rs");
    }
    #[test]
    fn optional_sha256_fixture() {
        let t=tempdir().unwrap();
        let status=Command::new("git").arg("init").arg("-q")
            .arg("--object-format=sha256").arg(t.path()).status().unwrap();
        if !status.success() {return;} // Older Git does not support SHA-256 repositories.
        let git=GitRepository::open(t.path()).unwrap();
        assert_eq!(git.algorithm,HashAlgorithm::Sha256);
    }
    #[test]
    fn reject_unsafe_paths_and_oids() {
        assert!(components(b"../secret").is_err());assert!(components(b"/etc/passwd").is_err());
        assert!(valid_oid("HEAD",HashAlgorithm::Sha1).is_err());
        assert!(valid_ref("refs/heads/a..b").is_err());
    }
}
