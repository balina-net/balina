//! Per-tab case data: where a tab's `.baltsv` section body lives.
//!
//! A body is immutable once written, so tabs share it by reference
//! (`Arc`): duplicating a tab costs nothing, and a temp snapshot is
//! deleted when the last tab holding it lets go. Desktop keeps bodies in
//! files — a snapshot in the cache dir, a section of a sidecar read in
//! place, or a pack extracted to the cache dir — and reads rows on demand;
//! web keeps them in memory.

use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bn_core::io::baltsv::{RowIndex, SectionMeta};

use crate::error::CmdError;

/// Identifies the on-disk state of a file we read in place but do not own,
/// so an outside rewrite is noticed instead of read as garbage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FileStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
}

impl FileStamp {
    pub fn of(path: &Path) -> std::io::Result<FileStamp> {
        let md = std::fs::metadata(path)?;
        Ok(FileStamp { len: md.len(), modified: md.modified().ok() })
    }
}

/// A file holding one or more bodies.
#[derive(Debug)]
pub struct DataFile {
    pub path: PathBuf,
    /// Ours (cache dir): deleted on drop.
    temp: bool,
    /// Someone else's (a sidecar): must still look like this when read.
    stamp: Option<FileStamp>,
}

impl DataFile {
    pub fn temp(path: PathBuf) -> DataFile {
        DataFile { path, temp: true, stamp: None }
    }

    pub fn in_place(path: PathBuf) -> std::io::Result<DataFile> {
        let stamp = FileStamp::of(&path)?;
        Ok(DataFile { path, temp: false, stamp: Some(stamp) })
    }

    /// The file was changed or removed behind our back.
    pub fn is_stale(&self) -> bool {
        self.stamp.is_some_and(|st| FileStamp::of(&self.path).ok() != Some(st))
    }
}

impl Drop for DataFile {
    fn drop(&mut self) {
        if self.temp {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[derive(Clone, Debug)]
pub enum DataStore {
    File(Arc<DataFile>),
    Memory(Arc<[u8]>),
}

/// Anything a body can be read from.
pub trait DataRead: BufRead + Seek {}
impl<T: BufRead + Seek> DataRead for T {}

impl DataStore {
    pub fn same(&self, other: &DataStore) -> bool {
        match (self, other) {
            (DataStore::File(a), DataStore::File(b)) => Arc::ptr_eq(a, b),
            (DataStore::Memory(a), DataStore::Memory(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    pub fn is_stale(&self) -> bool {
        matches!(self, DataStore::File(f) if f.is_stale())
    }

    pub fn open(&self) -> Result<Box<dyn DataRead + Send>, CmdError> {
        match self {
            DataStore::File(f) => {
                if f.is_stale() {
                    return Err(CmdError::Io(format!(
                        "data file changed on disk: {}",
                        f.path.display()
                    )));
                }
                Ok(Box::new(BufReader::new(File::open(&f.path)?)))
            }
            DataStore::Memory(bytes) => Ok(Box::new(Cursor::new(bytes.clone()))),
        }
    }
}

/// Source of [`TabData::id`]: never reused, unlike an allocation address.
static NEXT_BODY_ID: AtomicU64 = AtomicU64::new(1);

/// The case data attached to one tab: a section body somewhere.
#[derive(Clone, Debug)]
pub struct TabData {
    /// Which body this is, for "did the data change since the last save"
    /// checks. Clones (duplicated tabs) share it.
    pub(crate) id: u64,
    pub store: DataStore,
    pub body_offset: u64,
    pub body_len: u64,
    /// Origin / source / created. `tab_uid` and `name` are filled in from
    /// the tab whenever the data is written.
    pub meta: SectionMeta,
    pub rows: u64,
    pub cols: usize,
    /// Column names; empty until the body has been indexed.
    pub columns: Arc<Vec<String>>,
    /// Sparse row index; `None` until built (sidecars read in place are
    /// indexed lazily, on first view).
    pub index: Option<Arc<RowIndex>>,
}

impl TabData {
    pub(crate) fn new(
        store: DataStore,
        body_offset: u64,
        body_len: u64,
        meta: SectionMeta,
        rows: u64,
        cols: usize,
    ) -> TabData {
        TabData {
            id: NEXT_BODY_ID.fetch_add(1, Ordering::Relaxed),
            store,
            body_offset,
            body_len,
            meta,
            rows,
            cols,
            columns: Arc::default(),
            index: None,
        }
    }

    /// Open a reader positioned at the start of the body.
    pub fn open_body(&self) -> Result<Box<dyn DataRead + Send>, CmdError> {
        let mut r = self.store.open()?;
        r.seek(SeekFrom::Start(self.body_offset))?;
        Ok(r)
    }

    /// Identity of the body (shared by tabs holding the same data; changes
    /// when the data is replaced or moved into a saved sidecar).
    pub fn body_id(&self) -> u64 {
        self.id
    }

    pub fn same_body(&self, other: &TabData) -> bool {
        self.store.same(&other.store) && self.body_offset == other.body_offset
    }
}

/// One entry of a sidecar signature: which body was written under which
/// tab identity.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct SigEntry {
    uid: String,
    name: String,
    body: u64,
}

impl SigEntry {
    pub(crate) fn new(uid: &str, name: &str, data: &TabData) -> SigEntry {
        SigEntry { uid: uid.to_string(), name: name.to_string(), body: data.id }
    }
}

/// What the data file at `path` (a `.baltsv`, or the data entry of a
/// `.balpck`) holds, as of the last save or discovery.
#[derive(Clone, Debug)]
pub(crate) struct SidecarState {
    pub path: PathBuf,
    pub sig: Vec<SigEntry>,
}
