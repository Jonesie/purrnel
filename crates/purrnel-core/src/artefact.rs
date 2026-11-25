//! Versioned, content-addressed artefact store.
//!
//! Layout under the store root:
//! - `objects/<sha256>`  immutable content blobs
//! - `index/<id>.json`   per-artefact metadata and version list
//! - `data/<id>.json`    small key/value state the artefact itself keeps
//!
//! Every update appends a version, so undo and history are free.
//! Ids come from the model and the UI, so they are strictly validated.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Version {
    pub hash: String,
    pub ts: u64,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Meta {
    pub id: String,
    pub title: String,
    pub deleted: bool,
    pub versions: Vec<Version>,
}

impl Meta {
    pub fn latest(&self) -> &Version {
        self.versions.last().expect("an artefact always has a version")
    }
}

#[derive(Debug)]
pub enum Error {
    NotFound(String),
    BadId(String),
    BadVersion(usize),
    Io(io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound(id) => write!(f, "no artefact called '{id}'"),
            Error::BadId(id) => write!(f, "'{id}' is not a valid artefact id"),
            Error::BadVersion(n) => write!(f, "no version {n}"),
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::Json(e) => write!(f, "json error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Ids are lowercase ascii letters, digits and dashes only (no path tricks).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-');
    let out: String = out.chars().take(40).collect();
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "artefact".into() } else { out }
}

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        for d in ["objects", "index", "data"] {
            fs::create_dir_all(root.join(d))?;
        }
        Ok(Self { root })
    }

    fn index_path(&self, id: &str) -> Result<PathBuf> {
        if !valid_id(id) {
            return Err(Error::BadId(id.into()));
        }
        Ok(self.root.join("index").join(format!("{id}.json")))
    }

    fn data_path(&self, id: &str) -> Result<PathBuf> {
        if !valid_id(id) {
            return Err(Error::BadId(id.into()));
        }
        Ok(self.root.join("data").join(format!("{id}.json")))
    }

    fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    fn read_meta(&self, id: &str) -> Result<Meta> {
        let p = self.index_path(id)?;
        match fs::read(&p) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(Error::NotFound(id.into())),
            Err(e) => Err(e.into()),
        }
    }

    fn write_meta(&self, meta: &Meta) -> Result<()> {
        let p = self.index_path(&meta.id)?;
        Self::write_atomic(&p, &serde_json::to_vec_pretty(meta)?)
    }

    fn put_blob(&self, content: &str) -> Result<String> {
        let hash = hex(&Sha256::digest(content.as_bytes()));
        let p = self.root.join("objects").join(&hash);
        if !p.exists() {
            Self::write_atomic(&p, content.as_bytes())?;
        }
        Ok(hash)
    }

    fn read_blob(&self, hash: &str) -> Result<String> {
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::BadId(hash.into()));
        }
        Ok(fs::read_to_string(self.root.join("objects").join(hash))?)
    }

    pub fn exists(&self, id: &str) -> bool {
        self.index_path(id).map(|p| p.exists()).unwrap_or(false)
    }

    pub fn create(&self, title: &str, content: &str) -> Result<Meta> {
        let base = slugify(title);
        let mut id = base.clone();
        let mut n = 2;
        while self.exists(&id) {
            id = format!("{base}-{n}");
            n += 1;
        }
        let hash = self.put_blob(content)?;
        let meta = Meta {
            id,
            title: title.trim().to_string(),
            deleted: false,
            versions: vec![Version { hash, ts: now(), note: "created".into() }],
        };
        self.write_meta(&meta)?;
        Ok(meta)
    }

    /// Appends a version. Identical content is not versioned twice.
    pub fn update(&self, id: &str, content: &str, note: &str) -> Result<Meta> {
        let mut meta = self.read_meta(id)?;
        let hash = self.put_blob(content)?;
        if meta.latest().hash != hash {
            meta.versions.push(Version { hash, ts: now(), note: note.into() });
            self.write_meta(&meta)?;
        }
        Ok(meta)
    }

    pub fn get(&self, id: &str) -> Result<(Meta, String)> {
        let meta = self.read_meta(id)?;
        let content = self.read_blob(&meta.latest().hash)?;
        Ok((meta, content))
    }

    /// 1-based version number.
    pub fn get_version(&self, id: &str, n: usize) -> Result<String> {
        let meta = self.read_meta(id)?;
        let v = n
            .checked_sub(1)
            .and_then(|i| meta.versions.get(i))
            .ok_or(Error::BadVersion(n))?;
        self.read_blob(&v.hash)
    }

    /// Newest first.
    pub fn list(&self, include_deleted: bool) -> Result<Vec<Meta>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(self.root.join("index"))? {
            let p = entry?.path();
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(meta) = serde_json::from_slice::<Meta>(&fs::read(&p)?) {
                if include_deleted || !meta.deleted {
                    out.push(meta);
                }
            }
        }
        out.sort_by(|a, b| b.latest().ts.cmp(&a.latest().ts).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    /// Soft delete / restore: nothing is lost.
    pub fn set_deleted(&self, id: &str, deleted: bool) -> Result<Meta> {
        let mut meta = self.read_meta(id)?;
        meta.deleted = deleted;
        self.write_meta(&meta)?;
        Ok(meta)
    }

    /// Permanent erase of the artefact, its data, and any blobs nothing else uses.
    pub fn purge(&self, id: &str) -> Result<()> {
        let meta = self.read_meta(id)?;
        fs::remove_file(self.index_path(id)?)?;
        let _ = fs::remove_file(self.data_path(id)?);
        let still_used: HashSet<String> = self
            .list(true)?
            .into_iter()
            .flat_map(|m| m.versions.into_iter().map(|v| v.hash))
            .collect();
        for v in meta.versions {
            if !still_used.contains(&v.hash) {
                let _ = fs::remove_file(self.root.join("objects").join(&v.hash));
            }
        }
        Ok(())
    }

    fn read_data(&self, id: &str) -> Result<Map<String, Value>> {
        match fs::read(self.data_path(id)?) {
            Ok(b) => Ok(serde_json::from_slice(&b).unwrap_or_default()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Map::new()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn data_get(&self, id: &str, key: &str) -> Result<Option<Value>> {
        if !self.exists(id) {
            return Err(Error::NotFound(id.into()));
        }
        Ok(self.read_data(id)?.get(key).cloned())
    }

    pub fn data_set(&self, id: &str, key: &str, value: Value) -> Result<()> {
        if !self.exists(id) {
            return Err(Error::NotFound(id.into()));
        }
        let mut data = self.read_data(id)?;
        data.insert(key.to_string(), value);
        Self::write_atomic(&self.data_path(id)?, &serde_json::to_vec(&data)?)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        (dir, s)
    }

    #[test]
    fn create_get_and_unique_ids() {
        let (_d, s) = store();
        let a = s.create("My Notes!", "<p>one</p>").unwrap();
        let b = s.create("My Notes!", "<p>two</p>").unwrap();
        assert_eq!(a.id, "my-notes");
        assert_eq!(b.id, "my-notes-2");
        assert_eq!(s.get("my-notes").unwrap().1, "<p>one</p>");
    }

    #[test]
    fn updates_are_versioned_and_deduplicated() {
        let (_d, s) = store();
        let m = s.create("Timer", "v1").unwrap();
        s.update(&m.id, "v2", "second").unwrap();
        let m2 = s.update(&m.id, "v2", "same again").unwrap();
        assert_eq!(m2.versions.len(), 2);
        assert_eq!(s.get_version(&m.id, 1).unwrap(), "v1");
        assert_eq!(s.get_version(&m.id, 2).unwrap(), "v2");
        assert!(matches!(s.get_version(&m.id, 3), Err(Error::BadVersion(3))));
        assert!(matches!(s.get_version(&m.id, 0), Err(Error::BadVersion(0))));
    }

    #[test]
    fn soft_delete_hides_but_keeps() {
        let (_d, s) = store();
        let m = s.create("Game", "x").unwrap();
        s.set_deleted(&m.id, true).unwrap();
        assert!(s.list(false).unwrap().is_empty());
        assert_eq!(s.list(true).unwrap().len(), 1);
        s.set_deleted(&m.id, false).unwrap();
        assert_eq!(s.list(false).unwrap().len(), 1);
    }

    #[test]
    fn purge_removes_everything_but_shared_blobs() {
        let (d, s) = store();
        let a = s.create("A", "same").unwrap();
        let b = s.create("B", "same").unwrap();
        s.data_set(&a.id, "k", Value::from(1)).unwrap();
        s.purge(&a.id).unwrap();
        assert!(matches!(s.get(&a.id), Err(Error::NotFound(_))));
        // B still reads its content: the shared blob survived.
        assert_eq!(s.get(&b.id).unwrap().1, "same");
        s.purge(&b.id).unwrap();
        assert_eq!(fs::read_dir(d.path().join("objects")).unwrap().count(), 0);
    }

    #[test]
    fn rejects_path_traversal_ids() {
        let (_d, s) = store();
        for bad in ["../x", "a/b", "", "A", "a.b"] {
            assert!(matches!(s.get(bad), Err(Error::BadId(_))), "{bad}");
        }
    }

    #[test]
    fn artefact_key_value_data() {
        let (_d, s) = store();
        let m = s.create("Todo", "x").unwrap();
        assert_eq!(s.data_get(&m.id, "items").unwrap(), None);
        s.data_set(&m.id, "items", serde_json::json!(["milk"])).unwrap();
        assert_eq!(s.data_get(&m.id, "items").unwrap(), Some(serde_json::json!(["milk"])));
        assert!(s.data_set("nope", "k", Value::Null).is_err());
    }

    #[test]
    fn slugify_edge_cases() {
        assert_eq!(slugify("  Hello,  World  "), "hello-world");
        assert_eq!(slugify("!!!"), "artefact");
        assert!(slugify(&"x".repeat(100)).len() <= 40);
    }
}
