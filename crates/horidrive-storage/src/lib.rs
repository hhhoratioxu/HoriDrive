//! SQLite metadata and immutable, verified 4 MiB content blocks.
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs::{self, File, OpenOptions}, io::{Read, Write}, path::{Path, PathBuf}, time::{SystemTime, UNIX_EPOCH}};

pub const BLOCK_SIZE: usize = 4 * 1024 * 1024;
pub fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() }
pub type Clock = BTreeMap<String, u64>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub directory: bool,
    pub deleted: bool,
    pub hash: String,
    pub blocks: Vec<String>,
    pub size: u64,
    pub modified: u64,
    pub clock: Clock,
}
impl Entry {
    pub fn same_content(&self, other: &Self) -> bool {
        self.directory == other.directory && self.deleted == other.deleted && self.hash == other.hash
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Version { pub id: i64, pub path: String, pub created: u64, pub entry: Entry }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrashItem { pub id: String, pub path: String, pub deleted_at: u64 }

pub struct Storage { pub dir: PathBuf, db: Connection, quota: u64 }
impl Storage {
    pub fn open(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir.join("blocks"))?;
        fs::create_dir_all(dir.join("trash"))?;
        fs::create_dir_all(dir.join("staging"))?;
        let db = Connection::open(dir.join("metadata.sqlite3"))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS entries(path TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS versions(id INTEGER PRIMARY KEY, path TEXT NOT NULL, created INTEGER NOT NULL, value TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS versions_path ON versions(path,id);
            CREATE TABLE IF NOT EXISTS trash(id TEXT PRIMARY KEY, path TEXT NOT NULL, deleted_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS jobs(id TEXT PRIMARY KEY, value TEXT NOT NULL);")?;
        let mut store = Self { dir: dir.to_owned(), db, quota: 0 };
        store.quota = store.setting("quota")?.and_then(|s| s.parse().ok()).unwrap_or(0);
        Ok(store)
    }
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.db.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get(0)).optional()?)
    }
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.db.execute("INSERT INTO settings VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value])?; Ok(())
    }
    pub fn set_quota(&mut self, bytes: u64) -> Result<()> { self.set_setting("quota", &bytes.to_string())?; self.quota=bytes; Ok(()) }
    pub fn quota(&self) -> u64 { self.quota }
    pub fn entries(&self) -> Result<Vec<Entry>> {
        let mut q = self.db.prepare("SELECT value FROM entries ORDER BY path")?;
        let rows = q.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
    pub fn entry(&self, path: &str) -> Result<Option<Entry>> {
        let json: Option<String> = self.db.query_row("SELECT value FROM entries WHERE path=?1", [path], |r| r.get(0)).optional()?;
        json.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }
    pub fn save(&mut self, entry: &Entry) -> Result<()> {
        let previous = self.entry(&entry.path)?;
        let tx = self.db.transaction()?;
        if !entry.deleted && !entry.directory && previous.as_ref().is_none_or(|e| e.hash != entry.hash || e.deleted) {
            tx.execute("INSERT INTO versions(path,created,value) VALUES(?1,?2,?3)", params![entry.path,now() as i64,serde_json::to_string(entry)?])?;
            // Bound history per path; tombstones remain indefinitely.
            tx.execute("DELETE FROM versions WHERE path=?1 AND id NOT IN (SELECT id FROM versions WHERE path=?1 ORDER BY id DESC LIMIT 20)", [&entry.path])?;
        }
        tx.execute("INSERT INTO entries VALUES(?1,?2) ON CONFLICT(path) DO UPDATE SET value=excluded.value", params![entry.path,serde_json::to_string(entry)?])?;
        tx.commit()?; Ok(())
    }
    pub fn versions(&self, path: &str) -> Result<Vec<Version>> {
        let mut q=self.db.prepare("SELECT id,created,value FROM versions WHERE path=?1 ORDER BY id DESC")?;
        let rows=q.query_map([path], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)? as u64,r.get::<_,String>(2)?)))?;
        rows.map(|r| { let (id,created,json)=r?; Ok(Version{id,path:path.into(),created,entry:serde_json::from_str(&json)?}) }).collect()
    }
    pub fn record_trash(&self, item: &TrashItem) -> Result<()> {
        self.db.execute("INSERT INTO trash VALUES(?1,?2,?3)", params![item.id,item.path,item.deleted_at as i64])?; Ok(())
    }
    pub fn trash(&self) -> Result<Vec<TrashItem>> {
        let mut q=self.db.prepare("SELECT id,path,deleted_at FROM trash ORDER BY deleted_at DESC")?;
        let result=q.query_map([], |r| Ok(TrashItem{id:r.get(0)?,path:r.get(1)?,deleted_at:r.get::<_,i64>(2)? as u64}))?.collect::<rusqlite::Result<_>>()?; Ok(result)
    }
    pub fn forget_trash(&self, id: &str) -> Result<()> { self.db.execute("DELETE FROM trash WHERE id=?1", [id])?; Ok(()) }
    pub fn job(&self, id: &str, value: &str) -> Result<()> {
        self.db.execute("INSERT INTO jobs VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET value=excluded.value", params![id,value])?; Ok(())
    }
    pub fn jobs(&self) -> Result<Vec<serde_json::Value>> {
        let mut q=self.db.prepare("SELECT value FROM jobs ORDER BY id")?;
        let result=q.query_map([], |r| r.get::<_,String>(0))?.map(|r| Ok(serde_json::from_str(&r?)?)).collect(); result
    }
    fn block_path(&self, hash: &str) -> Result<PathBuf> {
        if hash.len()!=64 || !hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) { bail!("Invalid block hash"); }
        Ok(self.dir.join("blocks").join(hash))
    }
    pub fn has_block(&self, hash: &str) -> Result<bool> {
        let path=self.block_path(hash)?;
        if !path.exists() { return Ok(false); }
        let data=fs::read(path)?;
        Ok(data.len() <= BLOCK_SIZE && blake3::hash(&data).to_hex().as_str()==hash)
    }
    pub fn block(&self, hash: &str) -> Result<Vec<u8>> {
        let path=self.block_path(hash)?;
        if fs::metadata(&path)?.len()>BLOCK_SIZE as u64 { bail!("Oversized block"); }
        let data=fs::read(path)?;
        if blake3::hash(&data).to_hex().as_str()!=hash { bail!("Block integrity check failed"); }
        Ok(data)
    }
    pub fn put_block(&self, hash: &str, data: &[u8]) -> Result<()> {
        if data.len()>BLOCK_SIZE || blake3::hash(data).to_hex().as_str()!=hash { bail!("Block integrity check failed"); }
        let path=self.block_path(hash)?;
        if self.has_block(hash)? { return Ok(()); }
        if path.exists() { bail!("Corrupt block exists; repair required"); }
        if self.quota>0 && self.used()? + data.len() as u64>self.quota { bail!("Content storage quota exceeded"); }
        if fs2::available_space(&self.dir)? < data.len() as u64 + 1024*1024 { bail!("Insufficient disk space"); }
        let temp=self.dir.join("staging").join(uuid::Uuid::new_v4().to_string());
        let result=(|| -> Result<()> {
            let mut f=OpenOptions::new().write(true).create_new(true).open(&temp)?;
            f.write_all(data)?; f.sync_all()?; drop(f);
            fs::rename(&temp,&path)?; Ok(())
        })();
        if result.is_err() { let _=fs::remove_file(temp); }
        result
    }
    pub fn ingest(&self, path: &Path) -> Result<(String,Vec<String>,u64)> {
        let mut file=File::open(path).context("Cannot read file")?;
        let before=file.metadata()?;
        let mut buf=vec![0;BLOCK_SIZE]; let mut hash=blake3::Hasher::new(); let mut blocks=vec![]; let mut size=0;
        loop {
            let mut n=0; while n<buf.len() { let count=file.read(&mut buf[n..])?; if count==0 { break; } n+=count; }
            if n==0 { break; }
            hash.update(&buf[..n]); let block=blake3::hash(&buf[..n]).to_hex().to_string();
            self.put_block(&block,&buf[..n])?; blocks.push(block); size+=n as u64;
        }
        let after=file.metadata()?;
        if before.len()!=size || before.modified()?!=after.modified()? { bail!("File changed during indexing; retry after writer finishes"); }
        Ok((hash.finalize().to_hex().to_string(),blocks,size))
    }
    pub fn assemble(&self, entry: &Entry, target: &Path) -> Result<()> {
        if entry.directory || entry.deleted { bail!("Expected live file"); }
        if fs2::available_space(target.parent().context("Missing parent")?)? < entry.size.saturating_add(1024*1024) { bail!("Insufficient disk space"); }
        let mut file=OpenOptions::new().write(true).create_new(true).open(target)?;
        let result=(|| -> Result<()> {
            let mut hash=blake3::Hasher::new(); let mut size=0;
            for block in &entry.blocks { let data=self.block(block)?; hash.update(&data); file.write_all(&data)?; size+=data.len() as u64; }
            if size!=entry.size || hash.finalize().to_hex().as_str()!=entry.hash { bail!("File integrity check failed"); }
            file.sync_all()?; Ok(())
        })();
        drop(file); if result.is_err() { let _=fs::remove_file(target); } result
    }
    pub fn used(&self) -> Result<u64> { Ok(fs::read_dir(self.dir.join("blocks"))?.map(|e| Ok(e?.metadata()?.len())).collect::<Result<Vec<u64>>>()?.iter().sum()) }
    pub fn prune(&self) -> Result<u64> {
        let mut keep=std::collections::HashSet::new();
        for entry in self.entries()? { keep.extend(entry.blocks); }
        let mut q=self.db.prepare("SELECT value FROM versions")?;
        for json in q.query_map([],|r| r.get::<_,String>(0))? { keep.extend(serde_json::from_str::<Entry>(&json?)?.blocks); }
        let mut freed=0;
        for file in fs::read_dir(self.dir.join("blocks"))? { let file=file?; if !keep.contains(&file.file_name().to_string_lossy().to_string()) { freed+=file.metadata()?.len(); fs::remove_file(file.path())?; } }
        Ok(freed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn rejects_bad_blocks_and_quota() -> Result<()> {
        let tmp=tempfile::tempdir()?; let mut s=Storage::open(tmp.path())?;
        assert!(s.put_block("../evil",b"x").is_err());
        assert!(s.put_block(&blake3::hash(b"a").to_hex().to_string(),b"b").is_err());
        s.set_quota(1)?; let h=blake3::hash(b"xx").to_hex().to_string(); assert!(s.put_block(&h,b"xx").is_err()); Ok(())
    }
    #[test] fn streaming_roundtrip_and_integrity() -> Result<()> {
        let tmp=tempfile::tempdir()?; let s=Storage::open(&tmp.path().join("state"))?;
        for (n,size) in [0,1,BLOCK_SIZE+17].into_iter().enumerate() {
            let source=tmp.path().join(format!("input{n}")); fs::write(&source,vec![42;size])?;
            let (hash,blocks,size)=s.ingest(&source)?;
            let e=Entry{path:"x".into(),directory:false,deleted:false,hash,blocks,size,modified:now(),clock:Clock::new()};
            let dest=tmp.path().join(format!("output{n}")); s.assemble(&e,&dest)?; assert_eq!(fs::read(source)?,fs::read(dest)?);
        } Ok(())
    }
}
