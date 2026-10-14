//! Local repository, SQLite snapshot publication, integrity validation, and safe export.
use anyhow::{anyhow, bail, Context, Result};
use fs2::FileExt;
use openscion_cas::{FsObjectStore, ObjectReader};
use openscion_tree::{load_tree, scan_dir, MAX_DEPTH};
use openscion_types::{FileMode, ObjectId, ObjectKind};
use rusqlite::{params, Connection, OptionalExtension};
use std::{collections::HashSet, ffi::OsString, fs::{self, File, OpenOptions}, io::Write, path::{Path, PathBuf}, str::FromStr};
#[cfg(unix)]
use std::os::unix::{ffi::OsStringExt, fs::{symlink, PermissionsExt}};
use uuid::Uuid;

const FORMAT_VERSION: i64 = 1;

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub id: String,
    pub name: String,
    pub root_tree: ObjectId,
    pub created_at: i64,
    pub source_description: Option<String>,
}
#[derive(Clone, Debug, Default)]
pub struct VerifyReport {
    pub snapshots: usize,
    pub trees: usize,
    pub blobs: usize,
    pub published_objects: usize,
}

pub struct Repository {
    pub root: PathBuf,
    pub store: FsObjectStore,
    db: Connection,
}
impl Repository {
    pub fn init(root: &Path) -> Result<Self> {
        fs::create_dir_all(root).with_context(|| format!("create repository root {}",root.display()))?;
        let root = root.canonicalize()?;
        let meta = root.join(".openscion");
        if meta.exists() { bail!("OpenScion repository already initialized: {}", meta.display()); }
        // Initial schema lives in SQLite. An interrupted init is never advertised as valid.
        fs::create_dir(&meta)?;
        let result = (|| -> Result<()> {
            fs::create_dir(meta.join("objects"))?;
            fs::create_dir(meta.join("locks"))?;
            fs::create_dir(meta.join("tmp"))?;
            let db = connect_db(&meta.join("metadata.sqlite"))?;
            install_schema(&db)?;
            let id = Uuid::new_v4().to_string();
            db.execute("INSERT INTO repositories (id, format_version, created_at) VALUES (?1, ?2, strftime('%s','now'))", params![id, FORMAT_VERSION])?;
            let config = format!("format_version = 1\nrepository_id = \"{id}\"\n");
            let mut f = OpenOptions::new().write(true).create_new(true).open(meta.join("config.toml"))?;
            f.write_all(config.as_bytes())?; f.sync_all()?;
            File::open(&meta)?.sync_all()?;
            Ok(())
        })();
        if let Err(error) = result {
            // Do not delete a partially-created database or content automatically.
            return Err(error).context("initialization incomplete; inspect the .openscion directory before retrying");
        }
        Self::open(&root)
    }
    pub fn open(root: &Path) -> Result<Self> {
        let root = root.canonicalize().context("repository root does not exist")?;
        let meta = root.join(".openscion");
        if !meta.join("config.toml").is_file() { bail!("not an initialized OpenScion repository: {}",root.display()); }
        let db = connect_db(&meta.join("metadata.sqlite"))?;
        let version: i64 = db.query_row("SELECT format_version FROM repositories LIMIT 1", [], |r| r.get(0))?;
        if version != FORMAT_VERSION { bail!("unsupported repository format: {version}"); }
        Ok(Self { root, store: FsObjectStore::new(meta.join("objects")), db })
    }
    pub fn metadata_dir(&self) -> PathBuf { self.root.join(".openscion") }
    fn write_lock(&self) -> Result<File> {
        let file = OpenOptions::new().create(true).write(true).open(self.metadata_dir().join("locks/write.lock"))?;
        file.try_lock_exclusive().context("repository is busy with another writer")?;
        Ok(file) // OS lock automatically released on close
    }
    pub fn create_snapshot(&mut self, source: &Path, name: &str) -> Result<Snapshot> {
        if name.trim().is_empty() || name.len() > 200 || name.contains('\0') { bail!("invalid snapshot name"); }
        let _lock = self.write_lock()?;
        let source = source.canonicalize().context("snapshot source not available")?;
        let root_tree = scan_dir(&self.store, &source)?;
        // Durability ordering: publish all objects, verify reachability, THEN commit SQLite ref.
        let mut seen = HashSet::new();
        verify_reachable(&self.store, root_tree, 0, &mut seen)?;
        let id = Uuid::new_v4().to_string();
        let created_at = unix_now();
        let tx = self.db.transaction()?;
        tx.execute("INSERT INTO snapshots (id,name,root_tree,created_at,source_description) VALUES (?1,?2,?3,?4,?5)",
            params![id,name,root_tree.to_string(),created_at,source.to_string_lossy().to_string()])?;
        tx.commit()?;
        Ok(Snapshot { id, name: name.into(),root_tree,created_at,source_description:Some(source.to_string_lossy().to_string()) })
    }
    pub fn get_snapshot(&self, name_or_id: &str) -> Result<Snapshot> {
        let found: Option<(String,String,String,i64,Option<String>)> = self.db.query_row(
            "SELECT id,name,root_tree,created_at,source_description FROM snapshots WHERE id=?1 OR name=?1",
            [name_or_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
        ).optional()?;
        let (id,name,root,created_at,source_description) = found.ok_or_else(|| anyhow!("snapshot not found: {name_or_id}"))?;
        Ok(Snapshot { id, name, root_tree: ObjectId::from_str(&root)?, created_at,source_description })
    }
    pub fn list_snapshots(&self) -> Result<Vec<Snapshot>> {
        let mut stmt = self.db.prepare("SELECT id,name,root_tree,created_at,source_description FROM snapshots ORDER BY created_at,id")?;
        let mut rows = stmt.query([])?;
        let mut snapshots = Vec::new();
        while let Some(row) = rows.next()? {
            let root: String = row.get(2)?;
            snapshots.push(Snapshot { id:row.get(0)?,name:row.get(1)?,root_tree:root.parse()?,created_at:row.get(3)?,source_description:row.get(4)? });
        }
        Ok(snapshots)
    }
    pub fn fsck(&self, check_all: bool) -> Result<VerifyReport> {
        let mut report = VerifyReport::default();
        let mut visited = HashSet::new();
        for snapshot in self.list_snapshots()? {
            report.snapshots += 1;
            verify_reachable(&self.store, snapshot.root_tree, 0, &mut visited)
                .with_context(|| format!("verify snapshot {}", snapshot.name))?;
        }
        report.trees = visited.iter().filter(|x| x.kind == ObjectKind::Tree).count();
        report.blobs = visited.iter().filter(|x| x.kind == ObjectKind::Blob).count();
        if check_all {
            let all = self.store.list_objects()?;
            report.published_objects = all.len();
            for id in all {
                self.store.verify(id).with_context(|| format!("verify published object {id}"))?;
                if id.kind == ObjectKind::Tree { load_tree(&self.store,id)?; }
            }
        }
        Ok(report)
    }
    pub fn export_snapshot(&self, snapshot: &Snapshot, destination: &Path) -> Result<()> {
        let _lock = self.write_lock()?;
        if fs::symlink_metadata(destination).is_ok() { bail!("export destination must not exist"); }
        let parent = destination.parent().ok_or_else(|| anyhow!("export requires destination parent"))?;
        let parent = parent.canonicalize().context("export destination parent missing")?;
        let filename = destination.file_name().ok_or_else(|| anyhow!("export requires destination filename"))?;
        let resolved_destination = parent.join(filename);
        if fs::symlink_metadata(&resolved_destination).is_ok() { bail!("export destination already exists"); }
        let stage = parent.join(format!(".openscion-export-{}", Uuid::new_v4()));
        fs::create_dir(&stage)?;
        let result = (|| -> Result<()> {
            let mut deferred_symlinks = Vec::new();
            write_export_tree(&self.store, snapshot.root_tree, &stage, 0, &mut deferred_symlinks)?;
            // Symlinks are materialized only AFTER all ordinary files/directories.
            for (path, target) in deferred_symlinks {
                create_safe_symlink(&path, &target)?;
            }
            fs::rename(&stage, &resolved_destination)?;
            File::open(&parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() { let _ = fs::remove_dir_all(&stage); }
        result
    }
}
fn unix_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}
fn connect_db(path: &Path) -> Result<Connection> {
    let db = Connection::open(path)?;
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;")?;
    Ok(db)
}
fn install_schema(db: &Connection) -> Result<()> {
    db.execute_batch("\
        CREATE TABLE repositories (id TEXT PRIMARY KEY,format_version INTEGER NOT NULL,created_at INTEGER NOT NULL);\
        CREATE TABLE snapshots (id TEXT PRIMARY KEY,name TEXT NOT NULL UNIQUE,root_tree TEXT NOT NULL,created_at INTEGER NOT NULL,source_description TEXT);\
        CREATE INDEX snapshots_created_at ON snapshots(created_at);\
        CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY,description TEXT NOT NULL);\
        INSERT INTO schema_migrations (version,description) VALUES (1,'Initial schema');\
    ")?;
    Ok(())
}
fn verify_reachable(store: &FsObjectStore, id: ObjectId, depth: usize, seen: &mut HashSet<ObjectId>) -> Result<()> {
    if depth > MAX_DEPTH { bail!("Merkle tree depth exceeded"); }
    if !seen.insert(id) { return Ok(()); }
    let data = store.get(id)?; // Hash verification always performed.
    if id.kind == ObjectKind::Tree {
        let tree = openscion_tree::Tree::decode(&data)?;
        for entry in tree.entries { verify_reachable(store, entry.id, depth + 1, seen)?; }
    }
    Ok(())
}
#[cfg(unix)]
fn write_export_tree(store: &FsObjectStore, id: ObjectId, into: &Path, depth: usize,
                     symlinks: &mut Vec<(PathBuf,Vec<u8>)>) -> Result<()> {
    if depth > MAX_DEPTH { bail!("export exceeds maximum depth"); }
    let tree = load_tree(store, id)?;
    for entry in tree.entries {
        let name = OsString::from_vec(entry.name.as_bytes().to_vec());
        let dest = into.join(name);
        match entry.mode {
            FileMode::Directory => {
                fs::create_dir(&dest)?;
                write_export_tree(store, entry.id, &dest, depth + 1, symlinks)?;
            }
            FileMode::Regular | FileMode::Executable => {
                let data = store.get(entry.id)?;
                let mut f = OpenOptions::new().write(true).create_new(true).open(&dest)?;
                f.write_all(&data)?;
                let mode = if entry.mode == FileMode::Executable { 0o755 } else { 0o644 };
                f.set_permissions(fs::Permissions::from_mode(mode))?;
                f.sync_all()?;
            }
            FileMode::Symlink => {
                let bytes = store.get(entry.id)?;
                symlinks.push((dest, bytes));
            }
        }
    }
    File::open(into)?.sync_all()?;
    Ok(())
}
#[cfg(not(unix))]
fn write_export_tree(_: &FsObjectStore, _: ObjectId, _: &Path, _: usize, _: &mut Vec<(PathBuf,Vec<u8>)>) -> Result<()> {
    bail!("export currently supported only on Unix")
}
#[cfg(unix)]
fn create_safe_symlink(path: &Path, target_bytes: &[u8]) -> Result<()> {
    use std::path::Component;
    let target = PathBuf::from(OsString::from_vec(target_bytes.to_vec()));
    // Conservative policy: no absolute, parent, or current-dir components.
    if target_bytes.is_empty() || target.is_absolute() || target.components().any(|c| !matches!(c, Component::Normal(_))) {
        bail!("unsafe symlink target refused: {}",path.display());
    }
    symlink(&target,path)?;
    Ok(())
}
#[cfg(not(unix))]
fn create_safe_symlink(_: &Path, _: &[u8]) -> Result<()> { bail!("symlink export unsupported") }

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    #[test]
    fn snapshots_preserve_history_and_survive_restart() {
        let t = tempdir().unwrap();
        let src = t.path().join("source"); fs::create_dir(&src).unwrap();
        fs::write(src.join("hello.txt"), b"alpha").unwrap();
        let repo_dir = t.path().join("repo");
        let mut repo = Repository::init(&repo_dir).unwrap();
        let a = repo.create_snapshot(&src,"baseline").unwrap();
        fs::write(src.join("hello.txt"), b"beta").unwrap();
        let b = repo.create_snapshot(&src,"updated").unwrap();
        assert_ne!(a.root_tree, b.root_tree);
        drop(repo);
        let repo = Repository::open(&repo_dir).unwrap();
        assert_eq!(repo.get_snapshot("baseline").unwrap().root_tree,a.root_tree);
        assert_eq!(repo.fsck(true).unwrap().snapshots,2);
        let out = t.path().join("export"); repo.export_snapshot(&a,&out).unwrap();
        assert_eq!(fs::read(out.join("hello.txt")).unwrap(),b"alpha");
    }
    #[test]
    fn cannot_overwrite_snapshot_name() {
        let t = tempdir().unwrap(); let src=t.path().join("source"); fs::create_dir(&src).unwrap();
        let mut repo=Repository::init(&t.path().join("repo")).unwrap();
        repo.create_snapshot(&src,"x").unwrap();
        assert!(repo.create_snapshot(&src,"x").is_err());
    }
}
