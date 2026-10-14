//! SQLite registry and repository-scoped bearer-token access control.
//! Isolated from V0.1's repository metadata database.
use anyhow::{anyhow,bail,Context,Result};
use openscion_git::{GitRepository,HashAlgorithm};
use rusqlite::{params,Connection,OptionalExtension};
use serde::Serialize;
use sha2::{Digest,Sha256};
use std::{fs,path::{Path,PathBuf}};
use uuid::Uuid;

#[derive(Clone,Debug)]
pub struct Registry { data_dir: PathBuf }
#[derive(Clone,Debug,Serialize)]
pub struct Principal { pub id: String, pub role: String }
#[derive(Clone,Debug,Serialize)]
pub struct RepoRecord { pub id: String, pub name: String, #[serde(skip_serializing)] pub path: PathBuf,
    pub kind: String, pub object_format: String }
fn fingerprint(token:&str)->Vec<u8>{let mut h=Sha256::new();h.update(b"openscion-api-token-v1\0");h.update(token.as_bytes());h.finalize().to_vec()}
fn new_token()->String { format!("osc_{}{}",Uuid::new_v4().simple(),Uuid::new_v4().simple()) }
fn now()->i64 {use std::time::{SystemTime,UNIX_EPOCH};SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64}
fn connect(path:&Path)->Result<Connection> {
    let db=Connection::open(path)?;
    db.execute_batch("PRAGMA foreign_keys=ON;PRAGMA busy_timeout=5000;PRAGMA journal_mode=DELETE;PRAGMA synchronous=FULL;")?;
    Ok(db)
}
impl Registry {
    pub fn init(data_dir:&Path,allow_root:&Path)->Result<(Self,String)> {
        fs::create_dir_all(data_dir)?;
        let data_dir=data_dir.canonicalize()?;
        let root=allow_root.canonicalize().context("allow-root directory unavailable")?;
        if !root.is_dir() {bail!("allow-root must be a directory");}
        let dbpath=data_dir.join("service.sqlite");
        if dbpath.exists(){bail!("service already initialized");}
        let db=connect(&dbpath)?;
        db.execute_batch("\
           CREATE TABLE settings (key TEXT PRIMARY KEY,value TEXT NOT NULL);\
           CREATE TABLE principals (id TEXT PRIMARY KEY,role TEXT NOT NULL);\
           CREATE TABLE credentials (token_hash BLOB PRIMARY KEY,principal_id TEXT NOT NULL,expires_at INTEGER,revoked_at INTEGER,\
               FOREIGN KEY(principal_id) REFERENCES principals(id));\
           CREATE TABLE git_repositories (id TEXT PRIMARY KEY,name TEXT NOT NULL UNIQUE,path TEXT NOT NULL UNIQUE,\
               kind TEXT NOT NULL,object_format TEXT NOT NULL,registered_at INTEGER NOT NULL);\
           CREATE TABLE permissions (principal_id TEXT NOT NULL,repo_id TEXT NOT NULL,\
               PRIMARY KEY(principal_id,repo_id),\
               FOREIGN KEY(principal_id) REFERENCES principals(id),\
               FOREIGN KEY(repo_id) REFERENCES git_repositories(id));\
           CREATE TABLE audit_events (id TEXT PRIMARY KEY,at INTEGER NOT NULL,principal_id TEXT,\
               action TEXT NOT NULL,resource_id TEXT,result TEXT NOT NULL);\
           PRAGMA user_version=2;\
        ")?;
        db.execute("INSERT INTO settings(key,value) VALUES ('allowed_root',?1)",[root.to_string_lossy().to_string()])?;
        db.execute("INSERT INTO principals(id,role) VALUES ('admin','admin')",[])?;
        let token=new_token();
        db.execute("INSERT INTO credentials(token_hash,principal_id) VALUES (?1,'admin')",params![fingerprint(&token)])?;
        drop(db);
        let reg=Self::open(&data_dir)?;
        Ok((reg,token))
    }
    pub fn open(data_dir:&Path)->Result<Self> {
        let data_dir=data_dir.canonicalize().context("service data directory missing")?;
        if !data_dir.join("service.sqlite").is_file(){bail!("service not initialized");}
        let db=connect(&data_dir.join("service.sqlite"))?;
        let version:i64=db.pragma_query_value(None,"user_version",|row|row.get(0))?;
        if version==2 {
            db.execute_batch("BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS workspace_grants (principal_id TEXT NOT NULL, repo_id TEXT NOT NULL, permission TEXT NOT NULL, PRIMARY KEY(principal_id,repo_id,permission));
                CREATE TABLE IF NOT EXISTS workspaces (id TEXT PRIMARY KEY, repo_id TEXT NOT NULL, owner TEXT NOT NULL, task_id TEXT, base_oid TEXT NOT NULL, head_oid TEXT NOT NULL, path TEXT NOT NULL UNIQUE, state TEXT NOT NULL, generation INTEGER NOT NULL DEFAULT 0, lease_expires INTEGER NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, error TEXT);
                CREATE INDEX IF NOT EXISTS workspace_owner ON workspaces(owner);
                CREATE INDEX IF NOT EXISTS workspace_state ON workspaces(state,lease_expires);
                CREATE TABLE IF NOT EXISTS workspace_ops (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, principal_id TEXT NOT NULL, kind TEXT NOT NULL, idem_key TEXT NOT NULL, fingerprint TEXT NOT NULL, previous_head TEXT, result_oid TEXT, state TEXT NOT NULL, created_at INTEGER NOT NULL, UNIQUE(principal_id,workspace_id,kind,idem_key));
                CREATE TABLE IF NOT EXISTS workspace_commits (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, previous_oid TEXT NOT NULL, commit_oid TEXT NOT NULL, operation_id TEXT NOT NULL UNIQUE, created_at INTEGER NOT NULL);
                CREATE TABLE IF NOT EXISTS agent_sessions (session_id TEXT PRIMARY KEY, workspace_id TEXT, principal_id TEXT NOT NULL, task_id TEXT, agent_name TEXT, state TEXT NOT NULL, created_at INTEGER NOT NULL);
                PRAGMA user_version=3; COMMIT;")?;
        } else if version!=3 && version!=4 && version!=5 && version!=6 {bail!("unsupported service schema version {version}");}
        let actual:i64=db.pragma_query_value(None,"user_version",|row|row.get(0))?;
        if actual==3 {
            db.execute_batch(include_str!("schema_v4.sql"))?;
        }
        let upgraded:i64=db.pragma_query_value(None,"user_version",|row|row.get(0))?;
        if upgraded==4 {
            db.execute_batch(include_str!("schema_v5.sql"))?;
        }
        let v5:i64=db.pragma_query_value(None,"user_version",|row|row.get(0))?;
        if v5==5 {db.execute_batch(include_str!("schema_v6.sql"))?;}
        Ok(Self{data_dir})
    }
    fn conn(&self)->Result<Connection>{connect(&self.data_dir.join("service.sqlite"))}
    pub fn add_principal(&self,id:&str,role:&str)->Result<String> {
        if id.is_empty() || id.len()>128 || !id.bytes().all(|c|c.is_ascii_alphanumeric() || c==b'-' || c==b'_'){bail!("invalid principal identifier");}
        if role!="admin" && role!="reader" && role!="writer" && role!="validator" {bail!("invalid role");}
        let db=self.conn()?;
        db.execute("INSERT OR IGNORE INTO principals(id,role) VALUES (?1,?2)",params![id,role])?;
        let current_role:String=db.query_row("SELECT role FROM principals WHERE id=?1",[id],|r|r.get(0))?;
        if current_role!=role {bail!("principal already exists with different role");}
        let token=new_token();
        db.execute("INSERT INTO credentials(token_hash,principal_id) VALUES (?1,?2)",params![fingerprint(&token),id])?;
        Ok(token)
    }
    pub fn authenticate(&self,token:&str)->Result<Option<Principal>> {
        if token.len()>256 || token.is_empty(){return Ok(None);}
        let db=self.conn()?;
        let principal=db.query_row("SELECT p.id,p.role FROM credentials c JOIN principals p ON p.id=c.principal_id\
           WHERE c.token_hash=?1 AND c.revoked_at IS NULL AND (c.expires_at IS NULL OR c.expires_at>?2)",params![fingerprint(token),now()],
           |r|Ok(Principal{id:r.get(0)?,role:r.get(1)?})).optional()?;
        Ok(principal)
    }
    /// Revoke all current credentials for a principal (admin operation).
    pub fn revoke_principal(&self,id:&str)->Result<usize> {
        let db=self.conn()?;
        Ok(db.execute("UPDATE credentials SET revoked_at=?2 WHERE principal_id=?1 AND revoked_at IS NULL",params![id,now()])?)
    }
    pub fn register(&self,name:&str,path:&Path)->Result<RepoRecord> {
        if name.is_empty() || name.len()>128 || !name.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_'||b==b'.'){bail!("invalid repository name");}
        let canonical=path.canonicalize().context("registered Git repository missing")?;
        let db=self.conn()?;
        let root:String=db.query_row("SELECT value FROM settings WHERE key='allowed_root'",[],|r|r.get(0))?;
        if !canonical.starts_with(Path::new(&root)) {bail!("repository path outside administrator allow-root");}
        let git=GitRepository::open(&canonical)?; let info=git.info()?;
        let id=Uuid::new_v4().to_string();
        let alg=match info.object_format {HashAlgorithm::Sha1=>"sha1",HashAlgorithm::Sha256=>"sha256"};
        db.execute("INSERT INTO git_repositories(id,name,path,kind,object_format,registered_at) VALUES (?1,?2,?3,?4,?5,?6)",
            params![id,name,canonical.to_string_lossy().to_string(),info.kind,alg,now()])?;
        db.execute("INSERT OR IGNORE INTO repository_authority_v6(repo_id,mode,epoch,updated_at) VALUES (?1,'GIT_AUTHORITY',0,?2)",params![id,now()])?;
        Ok(RepoRecord{id,name:name.into(),path:canonical,kind:info.kind,object_format:alg.into()})
    }
    pub fn grant(&self,repo_name:&str,principal_id:&str)->Result<()> {
        let db=self.conn()?;
        let id:String=db.query_row("SELECT id FROM git_repositories WHERE name=?1",[repo_name],|r|r.get(0))?;
        db.execute("INSERT OR IGNORE INTO permissions(principal_id,repo_id) VALUES (?1,?2)",params![principal_id,id])?;
        Ok(())
    }
    pub fn list(&self,principal:&Principal)->Result<Vec<RepoRecord>> {
        let db=self.conn()?;
        let mut stmt=db.prepare("SELECT id,name,path,kind,object_format FROM git_repositories ORDER BY name")?;
        let mut rows=stmt.query([])?; let mut results=Vec::new();
        while let Some(row)=rows.next()? {
            let record=RepoRecord{id:row.get(0)?,name:row.get(1)?,path:PathBuf::from(row.get::<_,String>(2)?),kind:row.get(3)?,object_format:row.get(4)?};
            if self.permitted(principal,&record.id)? {results.push(record);}
        }
        Ok(results)
    }
    pub fn permitted(&self,principal:&Principal,repo_id:&str)->Result<bool> {
        if principal.role=="admin" {return Ok(true);}
        let db=self.conn()?;
        Ok(db.query_row("SELECT 1 FROM permissions WHERE principal_id=?1 AND repo_id=?2",
            params![principal.id,repo_id],|r|r.get::<_,i64>(0)).optional()?.is_some())
    }
    pub fn get_authorized(&self,id:&str,principal:&Principal)->Result<Option<RepoRecord>> {
        let db=self.conn()?;
        let record=db.query_row("SELECT id,name,path,kind,object_format FROM git_repositories WHERE id=?1",[id],|r|
            Ok(RepoRecord{id:r.get(0)?,name:r.get(1)?,path:PathBuf::from(r.get::<_,String>(2)?),kind:r.get(3)?,object_format:r.get(4)?})).optional()?;
        match record {Some(rec) if self.permitted(principal,&rec.id)?=>Ok(Some(rec)),_=>Ok(None)}
    }
    pub fn audit(&self,principal_id:&str,action:&str,resource:&str,result:&str)->Result<()> {
        let db=self.conn()?;
        db.execute("INSERT INTO audit_events(id,at,principal_id,action,resource_id,result) VALUES (?1,?2,?3,?4,?5,?6)",
            params![Uuid::new_v4().to_string(),now(),principal_id,action,resource,result])?;
        Ok(())
    }
    pub fn db_path(&self)->PathBuf { self.data_dir.join("service.sqlite") }
    pub fn workspace_root(&self)->PathBuf { self.data_dir.join("workspaces") }
    /// Workspace permissions are separate from the V0.2 read-only repository ACL.
    pub fn grant_workspace(&self,repo_name:&str,principal_id:&str,permission:&str)->Result<()> {
        if !["create","commit","dispose","renew"].contains(&permission) { bail!("unsupported workspace permission"); }
        let db=self.conn()?;
        let repo_id:String=db.query_row("SELECT id FROM git_repositories WHERE name=?1",[repo_name],|r|r.get(0))?;
        let exists:Option<String>=db.query_row("SELECT id FROM principals WHERE id=?1",[principal_id],|r|r.get(0)).optional()?;
        if exists.is_none() {bail!("principal does not exist");}
        db.execute("INSERT OR IGNORE INTO workspace_grants(principal_id,repo_id,permission) VALUES (?1,?2,?3)",params![principal_id,repo_id,permission])?;
        Ok(())
    }
    pub fn grant_change(&self,repo_name:&str,principal_id:&str,permission:&str)->Result<()> {
        if !["submit","preview","validate","review","land","reconcile","attest"].contains(&permission) {bail!("invalid change permission");}
        let db=self.conn()?;
        let repo_id:String=db.query_row("SELECT id FROM git_repositories WHERE name=?1",[repo_name],|r|r.get(0))?;
        let exists:Option<String>=db.query_row("SELECT id FROM principals WHERE id=?1",[principal_id],|r|r.get(0)).optional()?;
        if exists.is_none(){bail!("principal does not exist");}
        db.execute("INSERT OR IGNORE INTO change_grants(principal_id,repo_id,permission) VALUES (?1,?2,?3)",params![principal_id,repo_id,permission])?;
        Ok(())
    }
    pub fn change_allowed(&self,principal:&Principal,repo_id:&str,permission:&str)->Result<bool> {
        if principal.role=="admin" {return Ok(true);}
        if !self.permitted(principal,repo_id)? {return Ok(false);}
        let db=self.conn()?;
        Ok(db.query_row("SELECT 1 FROM change_grants WHERE principal_id=?1 AND repo_id=?2 AND permission=?3",params![principal.id,repo_id,permission],|r|r.get::<_,i64>(0)).optional()?.is_some())
    }
    /// Common authority fence for all V0.3/V0.4 Git mutation paths. Authority
    /// freeze serializes protected landing using the same per-repo lock.
    pub fn require_git_authority(&self,repo_id:&str)->Result<()> {
        let db=self.conn()?;
        let mode:Option<String>=db.query_row("SELECT mode FROM repository_authority_v6 WHERE repo_id=?1",[repo_id],|r|r.get(0)).optional()?;
        if mode.as_deref()!=Some("GIT_AUTHORITY") {bail!("AUTHORITY_FENCED");}
        Ok(())
    }
    pub fn workspace_allowed(&self,principal:&Principal,repo_id:&str,permission:&str)->Result<bool> {
        if principal.role=="admin" {return Ok(true);}
        if !self.permitted(principal,repo_id)? {return Ok(false);}
        let db=self.conn()?;
        Ok(db.query_row("SELECT 1 FROM workspace_grants WHERE principal_id=?1 AND repo_id=?2 AND permission=?3",params![principal.id,repo_id,permission],|r|r.get::<_,i64>(0)).optional()?.is_some())
    }
    pub fn require_admin(principal:&Principal)->Result<()> {
        if principal.role!="admin" {return Err(anyhow!("administrator role required"));} Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn token_authorization() {
        let t=tempfile::tempdir().unwrap();
        let (reg,admin)=Registry::init(&t.path().join("data"),t.path()).unwrap();
        let p=reg.authenticate(&admin).unwrap().unwrap();assert_eq!(p.role,"admin");
        assert!(reg.authenticate("incorrect").unwrap().is_none());
        let token=reg.add_principal("agent","reader").unwrap();
        assert_eq!(reg.authenticate(&token).unwrap().unwrap().id,"agent");
    }
}
