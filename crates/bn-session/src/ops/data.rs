//! Case-data ops: attach / remove a tab's data, read rows for the data
//! panel, and keep the `.baltsv` sidecar (or a `.balpck`'s data entry) in
//! step with the project.
//!
//! [`prepare`], [`index`], [`case_bytes`], [`load_baltsv_file`] and
//! [`extract_pack_file`] touch no session, so the UI runs them off it
//! (`spawn_blocking` on desktop); the rest are ordinary ops.

use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bn_core::io::baltsv::{self, BaltsvFile, BodyStats, Section, SectionMeta, SectionSource};
use bn_core::io::pack::PackReader;

use crate::data::{DataFile, DataStore, SidecarState, SigEntry, TabData};
use crate::error::CmdError;
use crate::session::{new_uid, Session, TabId};

pub const ORIGIN_LEARN_CPTS: &str = "learn-cpts";
pub const ORIGIN_LEARN_STRUCTURE: &str = "learn-structure";
pub const ORIGIN_SIMULATE: &str = "simulate";
pub const ORIGIN_ATTACH: &str = "attach";

/// Delimiter of the bytes [`case_bytes`] returns.
pub const CASE_BYTES_DELIM: u8 = b'\t';

// ---------------------------------------------------------------------------
// Attach / remove
// ---------------------------------------------------------------------------

/// Where [`prepare`] puts the converted body.
pub enum DataTarget<'a> {
    /// Web: keep it in memory.
    Memory,
    /// Desktop: a snapshot file in this (cache) directory.
    Dir(&'a Path),
}

/// Converted case data, ready to [`attach`].
pub struct PreparedData(TabData);

impl PreparedData {
    pub fn rows(&self) -> u64 {
        self.0.rows
    }
}

/// Convert a case file (parsed exactly as learning parses it) into a
/// section body. Off the session.
pub fn prepare(
    src: &[u8],
    forced_delim: Option<u8>,
    target: DataTarget<'_>,
    origin: &str,
    source: &str,
    progress: &mut dyn FnMut(f32),
) -> Result<PreparedData, CmdError> {
    let meta = SectionMeta {
        origin: origin.to_string(),
        source: source.to_string(),
        created: now_rfc3339(),
        ..Default::default()
    };
    let (store, stats) = match target {
        DataTarget::Memory => {
            let mut out = Vec::new();
            let stats = baltsv::convert_body(src, forced_delim, &mut out, progress)?;
            (DataStore::Memory(out.into()), stats)
        }
        DataTarget::Dir(dir) => {
            std::fs::create_dir_all(dir)?;
            // Declared before the writer so a failed conversion closes the
            // file before `DataFile`'s drop deletes it.
            let file = DataFile::temp(dir.join(format!("{}.body", new_uid())));
            let mut out = BufWriter::new(File::create(&file.path)?);
            let stats = baltsv::convert_body(src, forced_delim, &mut out, progress)?;
            out.flush()?;
            drop(out);
            (DataStore::File(Arc::new(file)), stats)
        }
    };
    let mut data =
        TabData::new(store, 0, stats.len, meta, stats.index.rows, stats.columns.len());
    data.columns = Arc::new(stats.columns);
    data.index = Some(Arc::new(stats.index));
    Ok(PreparedData(data))
}

/// Attach prepared data to a tab, replacing what it had. `false` if the tab
/// was closed in the meantime.
pub fn attach(s: &mut Session, tab: TabId, data: PreparedData) -> bool {
    if !s.tabs().contains_key(tab) {
        return false;
    }
    s.tab_mut(tab).data = Some(data.0);
    s.modified = true;
    true
}

pub fn remove(s: &mut Session, tab: TabId) {
    if s.tabs().contains_key(tab) && s.tab_mut(tab).data.take().is_some() {
        s.modified = true;
    }
}

pub fn any_data(s: &Session) -> bool {
    s.tab_order().iter().any(|&id| s.tab(id).data.is_some())
}

fn data_tabs(s: &Session) -> Vec<TabId> {
    s.tab_order().iter().copied().filter(|&id| s.tab(id).data.is_some()).collect()
}

/// Some tab's data comes from a file that changed on disk.
pub fn has_stale(s: &Session) -> bool {
    s.tab_order().iter().any(|&id| s.tab(id).data.as_ref().is_some_and(|d| d.store.is_stale()))
}

/// Drop data whose backing file changed on disk; returns the tab names.
pub fn drop_stale(s: &mut Session) -> Vec<String> {
    let mut names = Vec::new();
    for id in s.tab_order().to_vec() {
        if s.tab(id).data.as_ref().is_some_and(|d| d.store.is_stale()) {
            s.tab_mut(id).data = None;
            names.push(s.tab_label(id).to_string());
        }
    }
    names
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Up to `count` rows from row `start`. Needs the body's index.
pub fn read_rows(data: &TabData, start: u64, count: usize) -> Result<Vec<Vec<String>>, CmdError> {
    let index = data
        .index
        .as_ref()
        .ok_or_else(|| CmdError::BadRequest("data is not indexed yet".into()))?;
    let mut r = data.store.open()?;
    Ok(baltsv::read_rows(&mut r, data.body_offset, index, start, count)?)
}

/// Index a body by reading it once. Off the session.
pub fn index(data: &TabData) -> Result<BodyStats, CmdError> {
    let body = data.open_body()?.take(data.body_len);
    Ok(baltsv::index_body(body)?)
}

/// Store a finished index on every tab holding that body.
pub fn set_index(s: &mut Session, body: &TabData, stats: BodyStats) {
    let columns = Arc::new(stats.columns);
    let index = Arc::new(stats.index);
    for id in s.tab_order().to_vec() {
        if let Some(d) = s.tab_mut(id).data.as_mut()
            && d.id == body.id
        {
            d.rows = index.rows;
            d.cols = columns.len();
            d.columns = columns.clone();
            d.index = Some(index.clone());
        }
    }
}

/// The body as plain TSV, for learning from a tab's data with
/// [`CASE_BYTES_DELIM`] forced. Off the session.
pub fn case_bytes(data: &TabData) -> Result<Vec<u8>, CmdError> {
    let body = data.open_body()?.take(data.body_len);
    Ok(baltsv::body_to_tsv(body)?)
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// `<stem>.baltsv` next to a `.balina`.
pub fn sidecar_path(project: &Path) -> PathBuf {
    project.with_extension("baltsv")
}

/// Sibling path written first and renamed over `path`, so a failed save
/// never leaves a half-written file behind.
pub(crate) fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Every tab's data as one `.baltsv` stream, in tab order. Returns where
/// each body landed.
pub fn write_baltsv<W: Write>(
    s: &Session,
    out: &mut W,
) -> Result<Vec<(TabId, Section)>, CmdError> {
    let ids = data_tabs(s);
    let mut readers = Vec::with_capacity(ids.len());
    for &id in &ids {
        readers.push(s.tab(id).data.as_ref().unwrap().open_body()?);
    }
    let mut sources: Vec<SectionSource> = ids
        .iter()
        .zip(readers.iter_mut())
        .map(|(&id, body)| {
            let tab = s.tab(id);
            let d = tab.data.as_ref().unwrap();
            SectionSource {
                meta: SectionMeta {
                    tab_uid: tab.uid.clone(),
                    name: tab.doc.net.name.clone(),
                    ..d.meta.clone()
                },
                rows: d.rows,
                cols: d.cols,
                body_len: d.body_len,
                body,
            }
        })
        .collect();
    let placed = baltsv::write(out, &s.uid, &mut sources)?;
    Ok(ids.into_iter().zip(placed).collect())
}

/// Rough size of [`write_baltsv`]'s output (bodies + headers).
pub fn estimated_len(s: &Session) -> u64 {
    data_tabs(s).iter().map(|&id| s.tab(id).data.as_ref().unwrap().body_len + 512).sum::<u64>() + 64
}

fn signature(s: &Session) -> Vec<SigEntry> {
    data_tabs(s)
        .into_iter()
        .map(|id| {
            let tab = s.tab(id);
            SigEntry::new(&tab.uid, &tab.doc.net.name, tab.data.as_ref().unwrap())
        })
        .collect()
}

/// The data file at `path` already holds exactly the session's data.
pub(crate) fn data_file_current(s: &Session, path: &Path) -> bool {
    s.sidecar.as_ref().is_some_and(|st| st.path == path && st.sig == signature(s))
        && path.is_file()
        && !has_stale(s)
}

/// Remember that the data file at `path` now holds the session's data.
pub(crate) fn record_data_file(s: &mut Session, path: &Path) {
    s.sidecar = any_data(s).then(|| SidecarState { path: path.to_path_buf(), sig: signature(s) });
}

/// Write the data to `path` via a temp sibling. Returns where bodies landed.
fn write_baltsv_file(s: &Session, path: &Path) -> Result<Vec<(TabId, Section)>, CmdError> {
    let tmp = tmp_path(path);
    let written = File::create(&tmp).map_err(CmdError::from).and_then(|f| {
        let mut out = BufWriter::new(f);
        let placed = write_baltsv(s, &mut out)?;
        out.flush()?;
        Ok(placed)
    });
    match written {
        Ok(placed) => {
            std::fs::rename(&tmp, path)?;
            Ok(placed)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// After saving `project` (a `.balina`): write, keep, or delete
/// `<stem>.baltsv`. Returns a line for the message log if anything changed.
pub fn save_sidecar(s: &mut Session, project: &Path) -> Result<Option<String>, CmdError> {
    let path = sidecar_path(project);
    if !any_data(s) {
        s.sidecar = None;
        // Only delete a sidecar that is provably this project's.
        let ours = path.is_file()
            && load_baltsv_file(&path).is_ok_and(|l| l.file.project_uid == s.uid);
        if ours {
            std::fs::remove_file(&path)?;
            return Ok(Some(format!("Removed {} (no tab has data).", path.display())));
        }
        return Ok(None);
    }
    if data_file_current(s, &path) {
        return Ok(None);
    }
    let placed = write_baltsv_file(s, &path)?;
    // Read from the sidecar from now on, so temp snapshots can go.
    repoint(s, &path, &placed)?;
    record_data_file(s, &path);
    Ok(Some(format!("Saved data of {} tab(s) to {}.", placed.len(), path.display())))
}

/// Make the tabs read their data from the file just written at `path`
/// (same bodies, new offsets).
fn repoint(s: &mut Session, path: &Path, placed: &[(TabId, Section)]) -> Result<(), CmdError> {
    let file = Arc::new(DataFile::in_place(path.to_path_buf())?);
    for (id, sec) in placed {
        if let Some(d) = s.tab_mut(*id).data.as_mut() {
            let mut moved = TabData::new(
                DataStore::File(file.clone()),
                sec.body_offset,
                sec.body_len,
                d.meta.clone(),
                d.rows,
                d.cols,
            );
            moved.columns = d.columns.clone();
            moved.index = d.index.clone();
            *d = moved;
        }
    }
    Ok(())
}

/// Some tab reads its data in place from the file at `path`.
fn reads_from(s: &Session, path: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let target = canon(path);
    s.tab_order().iter().any(|&id| {
        matches!(&s.tab(id).data, Some(TabData { store: DataStore::File(f), .. }) if canon(&f.path) == target)
    })
}

/// File > Save Data As: a copy of all data anywhere. Returns the section
/// count.
pub fn save_data_as(s: &mut Session, path: &Path) -> Result<usize, CmdError> {
    if !any_data(s) {
        return Err(CmdError::BadRequest("no tab has data".into()));
    }
    // Overwriting a file tabs read in place moves their bodies: follow
    // them, or the next read would find the file "changed on disk".
    let in_use = reads_from(s, path);
    let placed = write_baltsv_file(s, path)?;
    if in_use {
        repoint(s, path, &placed)?;
        if s.path.as_deref().map(sidecar_path).as_deref() == Some(path) {
            record_data_file(s, path);
        }
    }
    Ok(placed.len())
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// A parsed `.baltsv`, ready to attach.
pub struct LoadedData {
    pub file: BaltsvFile,
    store: DataStore,
}

/// Desktop: open a `.baltsv` to read in place.
pub fn load_baltsv_file(path: &Path) -> Result<LoadedData, CmdError> {
    let store = DataStore::File(Arc::new(DataFile::in_place(path.to_path_buf())?));
    let file = baltsv::open(&mut BufReader::new(File::open(path)?))?;
    Ok(LoadedData { file, store })
}

/// Web: a `.baltsv` held in memory.
pub fn load_baltsv_bytes(bytes: Vec<u8>) -> Result<LoadedData, CmdError> {
    let bytes: Arc<[u8]> = bytes.into();
    let file = baltsv::open(&mut Cursor::new(bytes.clone()))?;
    Ok(LoadedData { file, store: DataStore::Memory(bytes) })
}

fn section_data(loaded: &LoadedData, sec: &Section) -> TabData {
    let meta = SectionMeta { tab_uid: String::new(), name: String::new(), ..sec.meta.clone() };
    TabData::new(loaded.store.clone(), sec.body_offset, sec.body_len, meta, sec.rows, sec.cols)
}

#[derive(Clone, Debug, Default)]
pub struct AttachReport {
    pub attached: usize,
    /// Sections whose tab no longer exists.
    pub orphans: usize,
    /// Tabs that already had data and kept it.
    pub kept: usize,
    /// The file belongs to another project; nothing was attached.
    pub foreign: bool,
    /// The file's header did not match its body (hand-edited); its
    /// sections were found by scanning.
    pub rebuilt: bool,
    /// The data could not be read at all (the project still opened).
    pub data_error: Option<String>,
}

impl AttachReport {
    /// Anything worth a message-log warning.
    pub fn warnings(&self, file: &str) -> Vec<String> {
        let mut out = Vec::new();
        if self.foreign {
            out.push(format!("{file} belongs to another project; its data was not attached."));
        }
        if self.rebuilt {
            out.push(format!("{file} was edited outside Balina; its index was rebuilt."));
        }
        if self.orphans > 0 {
            out.push(format!("{file} has data for {} tab(s) that no longer exist.", self.orphans));
        }
        if let Some(e) = &self.data_error {
            out.push(format!("the data in {file} could not be read: {e}"));
        }
        out
    }
}

/// Attach every section to the tab with the same uid; nothing when the file
/// belongs to another project. `explicit` (Cases > Attach Data) replaces
/// existing data and marks the project modified; otherwise (discovery,
/// opening a pack) tabs keep data attached meanwhile and the project stays
/// unmodified.
pub fn attach_loaded(s: &mut Session, loaded: &LoadedData, explicit: bool) -> AttachReport {
    let mut rep = AttachReport { rebuilt: loaded.file.rebuilt, ..Default::default() };
    if loaded.file.project_uid != s.uid {
        rep.foreign = true;
        return rep;
    }
    for sec in &loaded.file.sections {
        match s.tab_order().iter().copied().find(|&id| s.tab(id).uid == sec.meta.tab_uid) {
            None => rep.orphans += 1,
            Some(id) if !explicit && s.tab(id).data.is_some() => rep.kept += 1,
            Some(id) => {
                s.tab_mut(id).data = Some(section_data(loaded, sec));
                rep.attached += 1;
            }
        }
    }
    if explicit && rep.attached > 0 {
        s.modified = true;
    }
    rep
}

/// Attach one section of a (foreign) `.baltsv` to a tab.
pub fn attach_section(s: &mut Session, tab: TabId, loaded: &LoadedData, section: usize) -> bool {
    let Some(sec) = loaded.file.sections.get(section) else { return false };
    if !s.tabs().contains_key(tab) {
        return false;
    }
    s.tab_mut(tab).data = Some(section_data(loaded, sec));
    s.modified = true;
    true
}

/// After opening `project` (a `.balina`): attach `<stem>.baltsv` when it
/// exists. `Ok(None)` when there is none.
pub fn discover_sidecar(s: &mut Session, project: &Path) -> Result<Option<AttachReport>, CmdError> {
    let path = sidecar_path(project);
    if !path.is_file() {
        return Ok(None);
    }
    let loaded = load_baltsv_file(&path)?;
    let rep = attach_loaded(s, &loaded, false);
    if !rep.foreign && !rep.rebuilt && rep.orphans == 0 && rep.kept == 0 {
        record_data_file(s, &path);
    }
    Ok(Some(rep))
}

/// Uncompressed size of a pack's data, if it has any.
pub fn pack_data_len(pack: impl Read + std::io::Seek) -> Result<Option<u64>, CmdError> {
    Ok(PackReader::open(pack)?.data_len())
}

/// Desktop: extract a pack's data into the cache dir. Off the session.
pub fn extract_pack_file(pack: &Path, dir: &Path) -> Result<Option<LoadedData>, CmdError> {
    let mut reader = PackReader::open(BufReader::new(File::open(pack)?))?;
    if reader.data_len().is_none() {
        return Ok(None);
    }
    std::fs::create_dir_all(dir)?;
    let file = DataFile::temp(dir.join(format!("{}.baltsv", new_uid())));
    let mut out = BufWriter::new(File::create(&file.path)?);
    reader.extract_data(&mut out)?;
    out.flush()?;
    drop(out);
    let parsed = baltsv::open(&mut BufReader::new(File::open(&file.path)?))?;
    Ok(Some(LoadedData { file: parsed, store: DataStore::File(Arc::new(file)) }))
}

/// Web: a pack's data, decompressed into memory.
pub(crate) fn extract_pack_bytes<R: Read + std::io::Seek>(
    reader: &mut PackReader<R>,
) -> Result<Option<LoadedData>, CmdError> {
    if reader.data_len().is_none() {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    reader.extract_data(&mut bytes)?;
    load_baltsv_bytes(bytes).map(Some)
}

/// Attach data extracted from the pack the session was opened from.
/// Ignored if another file has been opened since.
pub fn attach_pack_data(s: &mut Session, pack: &Path, loaded: &LoadedData) -> AttachReport {
    if s.path.as_deref() != Some(pack) {
        return AttachReport::default();
    }
    let rep = attach_loaded(s, loaded, false);
    if !rep.foreign && !rep.rebuilt && rep.orphans == 0 && rep.kept == 0 {
        record_data_file(s, pack);
    }
    rep
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn now_rfc3339() -> String {
    let secs = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339(secs)
}

/// UTC timestamp from Unix seconds (Hinnant's civil-from-days).
fn rfc3339(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    #[test]
    fn rfc3339_dates() {
        assert_eq!(super::rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(super::rfc3339(1_795_430_655), "2026-11-23T10:44:15Z");
    }
}
