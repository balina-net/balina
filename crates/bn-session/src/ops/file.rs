//! File ops. Native file pickers live in the UI layer (rfd); these take
//! plain paths so they stay headless.
//!
//! Case data rides along: saving a `.balina` keeps its `.baltsv` sidecar in
//! step ([`data::save_sidecar`]) and a `.balpck` carries both. Reading a
//! pack's data and discovering a sidecar are separate steps the UI runs
//! after opening (see [`data`]).

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor, Write};
use std::path::{Path, PathBuf};

use bn_core::io;
use bn_core::io::pack::{PackReader, PackWriter};

use crate::doc::{Dirt, Document};
use crate::error::CmdError;
use crate::ops::data;
use crate::session::{new_uid, Session, Tab};
use crate::views::SaveResult;

pub fn doc_new(s: &mut Session) {
    s.reset();
}

/// Open a project or network file. For a `.balpck` only the project is
/// loaded here; its data follows via [`data::extract_pack_file`].
pub fn doc_open(s: &mut Session, path: PathBuf) -> Result<(), CmdError> {
    let fmt = io::Format::from_path(&path)?;
    if fmt == io::Format::Pack {
        let mut pack = PackReader::open(BufReader::new(File::open(&path)?))?;
        load_project(s, io::load_project_str(&pack.project_json()?)?)?;
    } else {
        let text = std::fs::read_to_string(&path)?;
        doc_open_str(s, &text, fmt)?;
    }
    s.path = Some(path);
    Ok(())
}

/// Open from already-loaded text (web builds — there is no path to keep).
pub fn doc_open_str(s: &mut Session, text: &str, fmt: io::Format) -> Result<(), CmdError> {
    match fmt {
        io::Format::NativeJson => load_project(s, io::load_project_str(text)?)?,
        io::Format::Pack => return Err(CmdError::BadRequest("a .balpck is binary".into())),
        _ => {
            // XMLBIF / XDSL: single-network import → one tab
            let iodoc = io::load_str(text, fmt)?;
            let mut tab = Tab::new();
            tab.doc = Document::from_io_document(iodoc, None);
            tab.bridge.invalidate();
            tab.bridge.recompute(&tab.doc);
            s.load_tabs(vec![tab], 0);
            s.uid = new_uid();
        }
    }
    s.path = None;
    s.modified = false;
    Ok(())
}

/// Open a `.balpck` from bytes (web builds), data included (in memory).
/// Unreadable data does not fail the open: the project loads and the
/// report carries the error (as on desktop).
pub fn doc_open_pack_bytes(s: &mut Session, bytes: &[u8]) -> Result<data::AttachReport, CmdError> {
    let mut pack = PackReader::open(Cursor::new(bytes))?;
    let project = io::load_project_str(&pack.project_json()?)?;
    let loaded = data::extract_pack_bytes(&mut pack);
    load_project(s, project)?;
    s.path = None;
    s.modified = false;
    Ok(match loaded {
        Ok(Some(l)) => data::attach_loaded(s, &l, false),
        Ok(None) => data::AttachReport::default(),
        Err(e) => data::AttachReport { data_error: Some(e.to_string()), ..Default::default() },
    })
}

/// Serialize for a platform-side save (web builds: the caller downloads the
/// text). Clears `modified` like a successful save; `s.path` is untouched.
pub fn doc_save_str(s: &mut Session, fmt: io::Format) -> Result<(String, Vec<String>), CmdError> {
    let label = match fmt {
        io::Format::NativeJson => ".balina",
        io::Format::Xmlbif => ".xmlbif",
        io::Format::Xdsl => ".xdsl",
        io::Format::Pack => ".balpck",
    };
    let mut warnings = data_not_kept(s, label);
    match fmt {
        io::Format::NativeJson => {
            let project = session_to_project(s);
            let text = io::save_project_str(&project)?;
            clear_modified(s);
            Ok((text, warnings))
        }
        io::Format::Pack => Err(CmdError::BadRequest("a .balpck is binary".into())),
        _ => {
            // XMLBIF / XDSL: export active tab only
            if s.tab_count() > 1 {
                warnings.push("only the active tab was exported (format does not support multiple networks)".into());
            }
            let (text, io_warnings) = io::save_str(&s.doc().to_io_document(), fmt)?;
            warnings.extend(io_warnings.into_iter().map(|io::Warning::Lossy(m)| m));
            clear_modified(s);
            Ok((text, warnings))
        }
    }
}

/// Serialize the project and its data as a `.balpck` (web builds: the
/// caller downloads the bytes).
pub fn doc_save_pack_bytes(s: &mut Session) -> Result<Vec<u8>, CmdError> {
    let json = io::save_project_str(&session_to_project(s))?;
    let mut w = PackWriter::new(Cursor::new(Vec::new()), &json)?;
    if data::any_data(s) {
        data::write_baltsv(s, w.data_writer(data::estimated_len(s))?)?;
    }
    let bytes = w.finish()?.into_inner();
    clear_modified(s);
    Ok(bytes)
}

/// `path: None` saves to the project's current path (error if it has none —
/// the UI runs a save dialog first in that case).
pub fn doc_save(s: &mut Session, path: Option<PathBuf>) -> Result<SaveResult, CmdError> {
    let path = match path {
        Some(p) => p,
        None => s
            .path
            .clone()
            .ok_or_else(|| CmdError::BadRequest("project has no path; use Save As".into()))?,
    };
    let fmt = io::Format::from_path(&path)?;
    let mut warnings = Vec::new();
    let mut notes = Vec::new();
    let mut data_unsaved = false;
    match fmt {
        io::Format::NativeJson => {
            let project = session_to_project(s);
            io::save_project(&project, &path)?;
            // The project is saved either way; a data problem is a warning,
            // and the project stays modified so quitting still asks.
            match data::save_sidecar(s, &path) {
                Ok(note) => notes.extend(note),
                Err(e) => {
                    warnings.push(format!("case data was not saved: {e}"));
                    data_unsaved = true;
                }
            }
        }
        io::Format::Pack => save_pack_file(s, &path)?,
        _ => {
            warnings.extend(data_not_kept(s, &format!(".{}", ext(&path))));
            let warnings_io = io::save(&s.doc().to_io_document(), &path)?;
            if !warnings_io.is_empty() {
                // warnings are surfaced by the caller
            }
        }
    }
    s.path = Some(path.clone());
    clear_modified(s);
    s.modified = data_unsaved;
    Ok(SaveResult { path, warnings, notes })
}

/// Initial mount: make sure beliefs exist if possible.
pub fn refresh(s: &mut Session) {
    s.finish(Dirt::None);
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Replace the session with a loaded project, filling in uids that files
/// saved before uids existed lack (silently: not a modification).
fn load_project(s: &mut Session, project: io::Project) -> Result<(), CmdError> {
    let mut seen = HashSet::new();
    let tabs: Vec<Tab> = project
        .sheets
        .into_iter()
        .map(|sheet| {
            let mut tab = Tab::new();
            if !sheet.uid.is_empty() && seen.insert(sheet.uid.clone()) {
                tab.uid = sheet.uid;
            }
            tab.doc = Document::from_io_document(sheet.doc, None);
            tab.bridge.invalidate();
            tab.bridge.recompute(&tab.doc);
            tab
        })
        .collect();
    if tabs.is_empty() {
        return Err(CmdError::BadRequest("project contains no tabs".into()));
    }
    s.load_tabs(tabs, project.active);
    s.uid = if project.uid.is_empty() { new_uid() } else { project.uid };
    Ok(())
}

/// Desktop `.balpck` save: unchanged data is copied from the previous pack
/// without recompressing.
fn save_pack_file(s: &mut Session, path: &Path) -> Result<(), CmdError> {
    let json = io::save_project_str(&session_to_project(s))?;
    let tmp = data::tmp_path(path);
    let written = (|| -> Result<(), CmdError> {
        let mut w = PackWriter::new(BufWriter::new(File::create(&tmp)?), &json)?;
        if data::any_data(s) {
            if data::data_file_current(s, path) {
                let mut old = PackReader::open(BufReader::new(File::open(path)?))?;
                w.copy_data_from(&mut old)?;
            } else {
                data::write_baltsv(s, w.data_writer(data::estimated_len(s))?)?;
            }
        }
        w.finish()?.flush()?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path)?;
    data::record_data_file(s, path);
    Ok(())
}

fn ext(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// Warning for formats that cannot carry case data.
fn data_not_kept(s: &Session, format: &str) -> Vec<String> {
    let n = s.tab_order().iter().filter(|&&id| s.tab(id).data.is_some()).count();
    if n == 0 {
        return vec![];
    }
    vec![format!(
        "case data of {n} tab(s) is not part of a {format} file; save as .balpck to keep it"
    )]
}

fn session_to_project(s: &Session) -> io::Project {
    let active = s.tab_order().iter().position(|&id| id == s.active_id()).unwrap_or(0);
    let sheets = s
        .tab_order()
        .iter()
        .map(|&id| {
            let tab = s.tab(id);
            io::ProjectSheet {
                label: tab.doc.net.name.clone(),
                uid: tab.uid.clone(),
                doc: tab.doc.to_io_document(),
            }
        })
        .collect();
    io::Project { uid: s.uid.clone(), sheets, active }
}

fn clear_modified(s: &mut Session) {
    let ids: Vec<_> = s.tab_order().to_vec();
    for id in ids {
        s.tab_mut(id).doc.modified = false;
    }
    s.modified = false;
}
