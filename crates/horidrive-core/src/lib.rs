//! Local-first file operations. Every public path is relative to one selected root.
use anyhow::{bail, Context, Result};
use base64::Engine;
use fs2::FileExt;
use horidrive_storage::{now, Clock, Entry, Storage, TrashItem, Version};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Serialize, Clone)]
pub struct FileItem {
    pub name: String,
    pub path: String,
    pub directory: bool,
    pub size: u64,
    pub modified: u64,
    pub kind: String,
}
#[derive(Serialize)]
pub struct Overview {
    pub node_id: String,
    pub root: String,
    pub capacity: u64,
    pub available: u64,
    pub content_bytes: u64,
    pub files: usize,
    pub quota: u64,
    pub index_error: String,
}
#[derive(Serialize)]
pub struct Preview {
    pub kind: String,
    pub content: String,
}

pub struct Core {
    pub root: PathBuf,
    pub store: Storage,
    pub node_id: String,
    _lock: File,
}

pub fn is_link(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}
pub fn validate(path: &str, allow_root: bool) -> Result<()> {
    if path.is_empty() {
        if allow_root {
            return Ok(());
        }
        bail!("Choose a file or folder");
    }
    if path.len() > 4096 || path.starts_with('/') || path.contains('\\') {
        bail!("Only relative forward-slash paths are accepted");
    }
    for part in path.split('/') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.eq_ignore_ascii_case(".horidrive")
            || part.ends_with(['.', ' '])
            || part.chars().any(|c| c < ' ' || "<>:\"|?*".contains(c))
            || ["CON", "PRN", "AUX", "NUL", "CLOCK$"].contains(&stem.as_str())
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit())
        {
            bail!("Unsafe or unsupported path component");
        }
    }
    Ok(())
}
pub fn dominates(a: &Clock, b: &Clock) -> bool {
    b.iter().all(|(k, v)| a.get(k).copied().unwrap_or(0) >= *v)
}
pub fn merged(a: &Clock, b: &Clock) -> Clock {
    let mut c = a.clone();
    for (k, v) in b {
        c.entry(k.clone())
            .and_modify(|x| *x = (*x).max(*v))
            .or_insert(*v);
    }
    c
}
fn bump(clock: &mut Clock, node: &str) -> Result<()> {
    let counter = clock.entry(node.to_string()).or_default();
    *counter = counter
        .checked_add(1)
        .context("Revision counter exhausted")?;
    Ok(())
}

impl Core {
    pub fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        if is_link(&fs::symlink_metadata(root)?) {
            bail!("Storage root cannot be a link or reparse point");
        }
        let root = root.canonicalize()?;
        let state = root.join(".horidrive");
        if state.exists() && is_link(&fs::symlink_metadata(&state)?) {
            bail!("Internal state cannot be a link");
        }
        fs::create_dir_all(&state)?;
        // Reject links in all pre-existing internal state before opening SQLite or blocks.
        Self::check_tree(&state)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(state.join("node.lock"))?;
        lock.try_lock_exclusive()
            .context("This folder is already open in another HoriDrive process")?;
        let store = Storage::open(&state)?;
        let node_id = store
            .setting("node_id")?
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        store.set_setting("node_id", &node_id)?;
        let mut core = Self {
            root,
            store,
            node_id,
            _lock: lock,
        };
        core.recover_install()?;
        // Staging files never replace originals until fully verified. Remove abandoned staging.
        for item in fs::read_dir(core.store.dir.join("staging"))? {
            let item = item?;
            if item.file_type()?.is_file() {
                fs::remove_file(item.path())?;
            }
        }
        if let Err(e) = core.scan() {
            core.store.set_setting("index_error", &e.to_string())?;
        }
        for mut job in core.store.jobs()? {
            if job["status"] == "running" {
                job["status"] = serde_json::json!("interrupted");
                job["bytes_per_second"] = serde_json::json!(0);
                job["error"] =
                    serde_json::json!("Previous process stopped; retry to resume verified blocks");
                if let Some(id) = job["id"].as_str() {
                    core.store.job(id, &serde_json::to_string(&job)?)?;
                }
            }
        }
        Ok(core)
    }
    fn check_tree(path: &Path) -> Result<()> {
        if is_link(&fs::symlink_metadata(path)?) {
            bail!("Symbolic links / reparse points are excluded");
        }
        if path.is_dir() {
            for item in fs::read_dir(path)? {
                Self::check_tree(&item?.path())?;
            }
        }
        Ok(())
    }
    pub fn resolve(&self, path: &str, allow_root: bool) -> Result<PathBuf> {
        validate(path, allow_root)?;
        let mut target = self.root.clone();
        for part in path.split('/').filter(|s| !s.is_empty()) {
            #[cfg(windows)]
            if target.is_dir() {
                for child in fs::read_dir(&target)? {
                    let name = child?.file_name().to_string_lossy().to_string();
                    if name.to_lowercase() == part.to_lowercase() && name != part {
                        bail!("Filename case conflict; use the exact existing spelling");
                    }
                }
            }
            target.push(part);
            match fs::symlink_metadata(&target) {
                Ok(meta) if is_link(&meta) => bail!("Symbolic links / reparse points are excluded"),
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(target)
    }
    pub fn overview(&self) -> Result<Overview> {
        Ok(Overview {
            node_id: self.node_id.clone(),
            root: self.root.display().to_string(),
            capacity: fs2::total_space(&self.root)?,
            available: fs2::available_space(&self.root)?,
            content_bytes: self.store.used()?,
            files: self
                .store
                .entries()?
                .iter()
                .filter(|e| !e.deleted && !e.directory)
                .count(),
            quota: self.store.quota(),
            index_error: self.store.setting("index_error")?.unwrap_or_default(),
        })
    }
    fn item(&self, path: &Path) -> Result<FileItem> {
        let m = fs::symlink_metadata(path)?;
        if is_link(&m) {
            bail!("Link excluded");
        }
        let relative = path
            .strip_prefix(&self.root)?
            .to_str()
            .context("Filename is not UTF-8")?
            .replace('\\', "/");
        validate(&relative, false)?;
        Ok(FileItem {
            name: path
                .file_name()
                .context("No filename")?
                .to_string_lossy()
                .into(),
            path: relative,
            directory: m.is_dir(),
            size: if m.is_dir() { 0 } else { m.len() },
            modified: m
                .modified()?
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            kind: if m.is_dir() {
                "Folder".into()
            } else {
                path.extension()
                    .map(|s| s.to_string_lossy().to_ascii_uppercase())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("File".into())
            },
        })
    }
    pub fn list(&self, path: &str) -> Result<Vec<FileItem>> {
        let folder = self.resolve(path, true)?;
        let mut out = vec![];
        for child in fs::read_dir(folder)? {
            let child = child?;
            if child.file_name().eq_ignore_ascii_case(".horidrive") || is_link(&child.metadata()?) {
                continue;
            }
            out.push(self.item(&child.path())?);
        }
        out.sort_by(|a, b| {
            b.directory
                .cmp(&a.directory)
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(out)
    }
    fn walk(&self, path: &str, out: &mut Vec<FileItem>) -> Result<()> {
        for item in self.list(path)? {
            if item.directory {
                self.walk(&item.path, out)?;
            }
            out.push(item);
        }
        Ok(())
    }
    pub fn search(&self, query: &str) -> Result<Vec<FileItem>> {
        let mut out = vec![];
        self.walk("", &mut out)?;
        let query = query.to_lowercase();
        out.retain(|e| e.path.to_lowercase().contains(&query));
        Ok(out)
    }
    pub fn recent(&self) -> Result<Vec<FileItem>> {
        let mut out = self.search("")?;
        out.retain(|e| !e.directory);
        out.sort_by_key(|e| std::cmp::Reverse(e.modified));
        out.truncate(100);
        Ok(out)
    }
    pub fn scan(&mut self) -> Result<Vec<Entry>> {
        let mut files = vec![];
        self.walk("", &mut files)?;
        let old: BTreeMap<_, _> = self
            .store
            .entries()?
            .into_iter()
            .map(|e| (e.path.clone(), e))
            .collect();
        let mut seen = BTreeSet::new();
        for item in files {
            seen.insert(item.path.clone());
            let (hash, blocks, size) = if item.directory {
                (String::new(), vec![], 0)
            } else {
                self.store.ingest(&self.resolve(&item.path, false)?)?
            };
            let mut e = Entry {
                path: item.path.clone(),
                directory: item.directory,
                deleted: false,
                hash,
                blocks,
                size,
                modified: item.modified,
                clock: old
                    .get(&item.path)
                    .map(|e| e.clock.clone())
                    .unwrap_or_default(),
            };
            if old.get(&item.path).is_none_or(|o| !o.same_content(&e)) {
                bump(&mut e.clock, &self.node_id)?;
                self.store.save(&e)?;
            }
        }
        for (_, mut e) in old {
            if !e.deleted && !seen.contains(&e.path) {
                e.deleted = true;
                e.modified = now();
                bump(&mut e.clock, &self.node_id)?;
                self.store.save(&e)?;
            }
        }
        self.store.set_setting("index_error", "")?;
        self.store.entries()
    }
    pub fn mkdir(&mut self, path: &str) -> Result<()> {
        let p = self.resolve(path, false)?;
        fs::create_dir(&p)?;
        self.scan()?;
        Ok(())
    }
    fn copy_tree(source: &Path, dest: &Path) -> Result<()> {
        Self::check_tree(source)?;
        if source.is_dir() {
            fs::create_dir(dest)?;
            for child in fs::read_dir(source)? {
                let child = child?;
                Self::copy_tree(&child.path(), &dest.join(child.file_name()))?;
            }
        } else {
            let mut src = File::open(source)?;
            let mut dst = OpenOptions::new().create_new(true).write(true).open(dest)?;
            std::io::copy(&mut src, &mut dst)?;
            dst.sync_all()?;
        }
        Ok(())
    }
    pub fn import(&mut self, source: &Path, dest: &str) -> Result<()> {
        let target = self.resolve(dest, false)?;
        if target.exists() {
            bail!("Destination exists; choose another name");
        }
        if is_link(&fs::symlink_metadata(source)?) {
            bail!("Cannot import a symbolic link / reparse point");
        }
        let source = source.canonicalize()?;
        if target.starts_with(&source) {
            bail!("Cannot import a folder into itself");
        }
        Self::check_tree(&source)?;
        let stage = self
            .store
            .dir
            .join("staging")
            .join(uuid::Uuid::new_v4().to_string());
        let result = (|| -> Result<()> {
            Self::copy_tree(&source, &stage)?;
            fs::rename(&stage, &target)?;
            Ok(())
        })();
        if result.is_err() {
            if stage.is_dir() {
                let _ = fs::remove_dir_all(&stage);
            } else {
                let _ = fs::remove_file(&stage);
            }
        }
        result?;
        self.scan()?;
        Ok(())
    }
    pub fn copy(&mut self, from: &str, to: &str) -> Result<()> {
        let source = self.resolve(from, false)?;
        self.import(&source, to)
    }
    pub fn move_file(&mut self, from: &str, to: &str) -> Result<()> {
        let source = self.resolve(from, false)?;
        let dest = self.resolve(to, false)?;
        if dest.exists() || dest.starts_with(&source) {
            bail!("Destination exists or is inside source");
        }
        Self::check_tree(&source)?;
        self.scan()?;
        fs::rename(source, dest)?;
        self.scan()?;
        Ok(())
    }
    pub fn delete(&mut self, path: &str) -> Result<String> {
        self.scan()?;
        let source = self.resolve(path, false)?;
        Self::check_tree(&source)?;
        let id = uuid::Uuid::new_v4().to_string();
        self.store.record_trash(&TrashItem {
            id: id.clone(),
            path: path.into(),
            deleted_at: now(),
            reason: "deleted".into(),
        })?;
        if let Err(err) = fs::rename(source, self.store.dir.join("trash").join(&id)) {
            self.store.forget_trash(&id)?;
            return Err(err.into());
        }
        self.scan()?;
        Ok(id)
    }
    pub fn trash(&self) -> Result<Vec<TrashItem>> {
        Ok(self
            .store
            .trash()?
            .into_iter()
            .filter(|t| self.store.dir.join("trash").join(&t.id).exists())
            .collect())
    }
    pub fn restore_trash(&mut self, id: &str) -> Result<()> {
        let item = self
            .trash()?
            .into_iter()
            .find(|t| t.id == id)
            .context("Trash item not found")?;
        let dest = self.resolve(&item.path, false)?;
        if dest.exists() {
            bail!("Original path exists; move it before restoring");
        }
        fs::create_dir_all(dest.parent().context("Missing parent")?)?;
        fs::rename(self.store.dir.join("trash").join(id), dest)?;
        self.store.forget_trash(id)?;
        self.scan()?;
        Ok(())
    }
    pub fn export(&self, path: &str, dest: &Path) -> Result<()> {
        let source = self.resolve(path, false)?;
        Self::copy_tree(&source, dest)
    }
    pub fn preview(&self, path: &str) -> Result<Preview> {
        let file = self.resolve(path, false)?;
        let f = File::open(&file)?;
        if f.metadata()?.len() > 8 * 1024 * 1024 {
            bail!("Preview limited to 8 MiB; export to view larger files");
        }
        let mut data = Vec::new();
        f.take(8 * 1024 * 1024 + 1).read_to_end(&mut data)?;
        if data.len() > 8 * 1024 * 1024 {
            bail!("Preview limit exceeded");
        }
        let ext = file
            .extension()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let mime = match ext.as_str() {
            "png" => Some("image/png"),
            "jpg" | "jpeg" => Some("image/jpeg"),
            "gif" => Some("image/gif"),
            "webp" => Some("image/webp"),
            _ => None,
        };
        if let Some(mime) = mime {
            Ok(Preview {
                kind: "image".into(),
                content: format!(
                    "data:{mime};base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(data)
                ),
            })
        } else {
            if data.len() > 1024 * 1024 {
                bail!("Text preview limited to 1 MiB");
            }
            let text =
                String::from_utf8(data).context("This file does not support text preview")?;
            if text.contains('\0') {
                bail!("Binary file; export to open");
            }
            Ok(Preview {
                kind: "text".into(),
                content: text,
            })
        }
    }
    pub fn versions(&self, path: &str) -> Result<Vec<Version>> {
        self.resolve(path, false)?;
        self.store.versions(path)
    }
    fn archive(&self, path: &str) -> Result<String> {
        let source = self.resolve(path, false)?;
        Self::check_tree(&source)?;
        let id = uuid::Uuid::new_v4().to_string();
        self.store.record_trash(&TrashItem {
            id: id.clone(),
            path: path.into(),
            deleted_at: now(),
            reason: "replaced".into(),
        })?;
        if let Err(e) = fs::rename(&source, self.store.dir.join("trash").join(&id)) {
            self.store.forget_trash(&id)?;
            return Err(e.into());
        }
        Ok(id)
    }
    fn recover_install(&mut self) -> Result<()> {
        if let Some(json) = self
            .store
            .setting("pending_install")?
            .filter(|s| !s.is_empty())
        {
            let (e, name): (Entry, String) = serde_json::from_str(&json)?;
            if uuid::Uuid::parse_str(&name).is_err() {
                bail!("Invalid installation journal");
            }
            let target = self.resolve(&e.path, false)?;
            let temp = self.store.dir.join("staging").join(name);
            if !target.exists() && temp.exists() {
                fs::create_dir_all(target.parent().context("Missing parent")?)?;
                fs::rename(&temp, &target)?;
            }
            if target.is_file() {
                let (hash, _, size) = self.store.ingest(&target)?;
                if hash == e.hash && size == e.size {
                    self.store.save(&e)?;
                }
            }
            // A separately modified destination is indexed as a new local revision; never blindly overwritten.
            self.store.set_setting("pending_install", "")?;
        }
        Ok(())
    }
    fn trim_replacements(&self) -> Result<()> {
        let mut counts = BTreeMap::<String, usize>::new();
        for item in self.store.trash()? {
            if item.reason != "replaced" {
                continue;
            }
            let count = counts.entry(item.path.clone()).or_default();
            *count += 1;
            if *count > 20 {
                anyhow::ensure!(uuid::Uuid::parse_str(&item.id).is_ok(), "Invalid trash ID");
                let payload = self.store.dir.join("trash").join(&item.id);
                if payload.exists() {
                    Self::check_tree(&payload)?;
                    if payload.is_dir() {
                        fs::remove_dir_all(&payload)?;
                    } else {
                        fs::remove_file(&payload)?;
                    }
                }
                self.store.forget_trash(&item.id)?;
            }
        }
        Ok(())
    }
    fn install(&mut self, e: &Entry) -> Result<()> {
        let target = self.resolve(&e.path, false)?;
        if !e.deleted
            && target.exists()
            && self
                .store
                .entry(&e.path)?
                .is_some_and(|local| local.same_content(e))
        {
            self.store.save(e)?;
            return Ok(());
        }
        if e.deleted {
            // Non-empty directories contain independent revisions. Delete only when children are already removed.
            if target.exists() {
                if e.directory && target.is_dir() && fs::read_dir(&target)?.next().is_some() {
                    return Ok(());
                }
                self.archive(&e.path)?;
            }
        } else if e.directory {
            if target.exists() && !target.is_dir() {
                self.archive(&e.path)?;
            }
            fs::create_dir_all(&target)?;
        } else {
            fs::create_dir_all(target.parent().context("Missing parent")?)?;
            let name = uuid::Uuid::new_v4().to_string();
            let temp = self.store.dir.join("staging").join(&name);
            self.store.assemble(e, &temp)?;
            self.store
                .set_setting("pending_install", &serde_json::to_string(&(e, &name))?)?;
            let backup = if target.exists() {
                Some(self.archive(&e.path)?)
            } else {
                None
            };
            if let Err(err) = fs::rename(&temp, &target) {
                if let Some(id) = backup {
                    let _ = fs::rename(self.store.dir.join("trash").join(&id), &target);
                }
                let _ = fs::remove_file(temp);
                return Err(err.into());
            }
        }
        self.store.save(e)?;
        self.store.set_setting("pending_install", "")?;
        self.trim_replacements()?;
        Ok(())
    }
    pub fn restore_version(&mut self, path: &str, id: i64) -> Result<()> {
        self.scan()?;
        let mut e = self
            .versions(path)?
            .into_iter()
            .find(|v| v.id == id)
            .context("Version not found")?
            .entry;
        e.clock = self.store.entry(path)?.map(|e| e.clock).unwrap_or_default();
        bump(&mut e.clock, &self.node_id)?;
        e.modified = now();
        self.install(&e)
    }
    pub fn apply(&mut self, remote: &Entry) -> Result<bool> {
        self.resolve(&remote.path, false)?;
        if remote.clock.is_empty()
            || remote.clock.len() > 256
            || remote.blocks.len() > 262144
            || remote.size > 1024 * 1024 * 1024 * 1024
            || remote
                .clock
                .iter()
                .any(|(id, value)| id.len() > 128 || *value == 0 || *value == u64::MAX)
            || (!remote.directory
                && (remote.hash.len() != 64
                    || !remote
                        .hash
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())))
            || (remote.directory
                && (!remote.hash.is_empty() || !remote.blocks.is_empty() || remote.size != 0))
        {
            bail!("Invalid remote revision");
        }
        // Download may take minutes. Re-index writes made since the manifest snapshot
        // before comparing clocks, so an offline edit is retained as a conflict.
        self.scan()?;
        let local = self.store.entry(&remote.path)?;
        let Some(local) = local else {
            self.install(remote)?;
            return Ok(true);
        };
        if dominates(&local.clock, &remote.clock) {
            return Ok(false);
        }
        if dominates(&remote.clock, &local.clock) {
            self.install(remote)?;
            return Ok(true);
        }
        let clock = merged(&local.clock, &remote.clock);
        let mut winner = remote.clone();
        if !local.same_content(remote) {
            // Deterministic resolution: live beats deleted, file beats folder, then greater content hash.
            let key = |e: &Entry| (!e.deleted, !e.directory, e.hash.clone());
            let (win, lose) = if key(&local) >= key(remote) {
                (&local, remote)
            } else {
                (remote, &local)
            };
            winner = win.clone();
            if !lose.deleted && !lose.directory {
                let mut conflict = lose.clone();
                conflict.path = format!(
                    "{}.conflict-{}",
                    lose.path,
                    &lose.hash[..12.min(lose.hash.len())]
                );
                if self.store.entry(&conflict.path)?.is_none() {
                    self.install(&conflict)?;
                }
            }
        }
        winner.clock = clock;
        self.install(&winner)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_file_lifecycle_and_restart() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let source = tmp.path().join("input");
        fs::write(&source, "第一版")?;
        let root = tmp.path().join("drive");
        let mut c = Core::open(&root)?;
        c.mkdir("资料")?;
        c.import(&source, "资料/中文 #+.txt")?;
        c.copy("资料/中文 #+.txt", "副本.txt")?;
        c.move_file("副本.txt", "重命名.txt")?;
        let id = c.delete("重命名.txt")?;
        c.restore_trash(&id)?;
        assert_eq!(c.preview("重命名.txt")?.content, "第一版");
        fs::write(root.join("重命名.txt"), "第二版")?;
        c.scan()?;
        let versions = c.versions("重命名.txt")?;
        assert_eq!(versions.len(), 3);
        c.restore_version("重命名.txt", versions.last().unwrap().id)?;
        drop(c);
        let c = Core::open(&root)?;
        assert_eq!(c.preview("重命名.txt")?.content, "第一版");
        assert!(!c.trash()?.is_empty());
        Ok(())
    }
    #[test]
    fn rejects_traversal_and_reserved_names() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let c = Core::open(tmp.path())?;
        for p in [
            "../escape",
            "/abs",
            "C:/x",
            "foo\\bar",
            ".horidrive/metadata.sqlite3",
            "x/../../y",
            "x:ads",
            "CON.txt",
            "x.",
            "x//y",
        ] {
            assert!(c.resolve(p, false).is_err(), "{p}");
        }
        assert!(Core::open(tmp.path()).is_err());
        Ok(())
    }
    #[test]
    fn concurrent_revisions_keep_both() -> Result<()> {
        let t = tempfile::tempdir()?;
        let mut a = Core::open(&t.path().join("a"))?;
        let mut b = Core::open(&t.path().join("b"))?;
        fs::write(a.root.join("x"), b"base")?;
        let base = a.scan()?.remove(0);
        for h in &base.blocks {
            b.store.put_block(h, &a.store.block(h)?)?;
        }
        b.apply(&base)?;
        fs::write(a.root.join("x"), b"from a")?;
        fs::write(b.root.join("x"), b"from b")?;
        let ae = a.scan()?.into_iter().find(|e| e.path == "x").unwrap();
        let be = b.scan()?.into_iter().find(|e| e.path == "x").unwrap();
        for h in &ae.blocks {
            b.store.put_block(h, &a.store.block(h)?)?;
        }
        for h in &be.blocks {
            a.store.put_block(h, &b.store.block(h)?)?;
        }
        a.apply(&be)?;
        b.apply(&ae)?;
        assert_eq!(fs::read(a.root.join("x"))?, fs::read(b.root.join("x"))?);
        assert!(a.list("")?.iter().any(|f| f.name.contains("conflict")));
        assert!(b.list("")?.iter().any(|f| f.name.contains("conflict")));
        Ok(())
    }
    #[test]
    fn write_during_download_is_detected_before_install() -> Result<()> {
        let t = tempfile::tempdir()?;
        let mut a = Core::open(&t.path().join("a"))?;
        let mut b = Core::open(&t.path().join("b"))?;
        fs::write(a.root.join("x"), "base")?;
        let base = a.scan()?.remove(0);
        for hash in &base.blocks {
            b.store.put_block(hash, &a.store.block(hash)?)?;
        }
        b.apply(&base)?;
        fs::write(b.root.join("x"), "remote edit")?;
        let remote = b.scan()?.into_iter().find(|e| e.path == "x").unwrap();
        for hash in &remote.blocks {
            a.store.put_block(hash, &b.store.block(hash)?)?;
        }
        fs::write(a.root.join("x"), "offline edit while downloading")?;
        a.apply(&remote)?;
        let content = a
            .list("")?
            .into_iter()
            .filter(|e| !e.directory)
            .map(|e| fs::read_to_string(a.root.join(e.path)))
            .collect::<std::io::Result<Vec<_>>>()?;
        assert!(content
            .iter()
            .any(|s| s == "offline edit while downloading"));
        assert!(content.iter().any(|s| s == "remote edit"));
        Ok(())
    }
    #[test]
    fn deletion_tombstone_survives_restart() -> Result<()> {
        let t = tempfile::tempdir()?;
        let mut c = Core::open(t.path())?;
        fs::write(c.root.join("empty"), [])?;
        c.scan()?;
        c.delete("empty")?;
        drop(c);
        let c = Core::open(t.path())?;
        assert!(c.store.entry("empty")?.unwrap().deleted);
        Ok(())
    }
    #[test]
    fn install_journal_recovers_verified_file_after_crash() -> Result<()> {
        let t = tempfile::tempdir()?;
        let mut c = Core::open(t.path())?;
        fs::write(c.root.join("recover.txt"), "new data")?;
        let e = c.scan()?.remove(0);
        let name = uuid::Uuid::new_v4().to_string();
        let stage = c.store.dir.join("staging").join(&name);
        c.store.assemble(&e, &stage)?;
        c.store
            .set_setting("pending_install", &serde_json::to_string(&(&e, &name))?)?;
        fs::remove_file(c.root.join("recover.txt"))?;
        drop(c);
        let c = Core::open(t.path())?;
        assert_eq!(fs::read_to_string(c.root.join("recover.txt"))?, "new data");
        assert!(!c.store.entry("recover.txt")?.unwrap().deleted);
        Ok(())
    }
    #[test]
    fn corrupt_replacement_keeps_original() -> Result<()> {
        let t = tempfile::tempdir()?;
        let mut c = Core::open(t.path())?;
        fs::write(c.root.join("x"), "keep me")?;
        let mut e = c.scan()?.remove(0);
        e.hash = blake3::hash(b"incorrect").to_hex().to_string();
        e.clock.insert("remote".into(), 1);
        assert!(c.apply(&e).is_err());
        assert_eq!(fs::read_to_string(c.root.join("x"))?, "keep me");
        Ok(())
    }
    #[test]
    fn replacement_history_is_bounded_user_trash_is_retained() -> Result<()> {
        let t = tempfile::tempdir()?;
        let mut c = Core::open(&t.path().join("drive"))?;
        fs::write(c.root.join("keep.txt"), "keep")?;
        c.scan()?;
        let id = c.delete("keep.txt")?;
        let source = t.path().join("source");
        for n in 1..=25 {
            fs::write(&source, format!("revision {n}"))?;
            let (hash, blocks, size) = c.store.ingest(&source)?;
            let e = Entry {
                path: "x.txt".into(),
                directory: false,
                deleted: false,
                hash,
                blocks,
                size,
                modified: now(),
                clock: BTreeMap::from([("remote".into(), n)]),
            };
            c.apply(&e)?;
        }
        assert_eq!(c.versions("x.txt")?.len(), 20);
        assert_eq!(
            c.trash()?.iter().filter(|t| t.reason == "replaced").count(),
            20
        );
        assert!(c.trash()?.iter().any(|t| t.id == id));
        Ok(())
    }
    #[test]
    fn symlink_or_junction_cannot_escape() -> Result<()> {
        let t = tempfile::tempdir()?;
        let root = t.path().join("drive");
        let outside = t.path().join("outside");
        fs::create_dir(&outside)?;
        let c = Core::open(&root)?;
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(root.join("escape"))
                .arg(&outside)
                .stdout(std::process::Stdio::null())
                .status()?;
            anyhow::ensure!(status.success(), "Could not create test junction");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("escape"))?;
        assert!(c.resolve("escape/secret", false).is_err());
        Ok(())
    }
    #[cfg(windows)]
    #[test]
    fn case_alias_cannot_overwrite() -> Result<()> {
        let t = tempfile::tempdir()?;
        let mut c = Core::open(t.path())?;
        fs::write(c.root.join("File.txt"), "keep")?;
        c.scan()?;
        assert!(c.resolve("file.txt", false).is_err());
        Ok(())
    }
    #[test]
    #[ignore = "Writes and hashes >1 GiB; run explicitly with sufficient disk space"]
    fn gigabyte_streaming() -> Result<()> {
        use std::io::Write;
        let t = tempfile::tempdir()?;
        let root = t.path().join("drive");
        fs::create_dir(&root)?;
        let mut f = File::create(root.join("large.bin"))?;
        let block = vec![7u8; 4 * 1024 * 1024];
        for _ in 0..257 {
            f.write_all(&block)?;
        }
        drop(f);
        let c = Core::open(&root)?;
        assert_eq!(
            c.store.entry("large.bin")?.unwrap().size,
            257 * 4 * 1024 * 1024
        );
        c.export("large.bin", &t.path().join("export.bin"))?;
        Ok(())
    }
}
