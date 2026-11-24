//! Tab data glue: where learning reads cases from ([`CaseSource`]), the
//! background jobs that convert, extract and index data (tracked in
//! `DATA_JOBS` so the status bar and panel show progress and saves wait for
//! them), and the Cases-menu flows. The session side lives in
//! `bn_session::ops::data`.

pub mod panel;

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use bn_session::ops::data::{self as data_ops, AttachReport};
use bn_session::{TabData, TabId};
use dioxus::core::spawn_forever;
use dioxus::prelude::*;

use crate::platform::{self, pick_case_file, CaseFile};
use crate::state::{
    exec, log_message, open_dialog, DataJob, DataJobKind, DialogDesc, DATA_BANNER,
    DATA_ERRORS, DATA_JOBS, LAST_CASE_FILE, SECTION_PICK, SESSION, SHOW_DATA_PANEL,
};
use crate::ui::BTN_SM;

/// Web keeps data in browser memory; confirm before holding more than this.
#[cfg(target_arch = "wasm32")]
pub const WEB_MEMORY_WARN: u64 = 200 * 1024 * 1024;

/// Where learning reads its cases from.
#[derive(Clone)]
pub enum CaseSource {
    File(CaseFile),
    /// The tab's attached data.
    Tab(TabData),
}

impl CaseSource {
    pub fn forced_delim(&self) -> Option<u8> {
        match self {
            CaseSource::File(f) => f.forced_delim(),
            CaseSource::Tab(_) => Some(data_ops::CASE_BYTES_DELIM),
        }
    }

    pub async fn read(&self) -> Result<Vec<u8>, String> {
        match self {
            CaseSource::File(f) => f.read().await,
            CaseSource::Tab(d) => {
                let d = d.clone();
                platform::run_blocking(move || data_ops::case_bytes(&d))
                    .await?
                    .map_err(|e| e.to_string())
            }
        }
    }
}

/// Initial source of the learn dialogs: the active tab's data when it has
/// some, else the last picked file.
pub fn default_source() -> Option<CaseSource> {
    let tab_data = SESSION.read().active_tab().data.clone();
    tab_data
        .map(CaseSource::Tab)
        .or_else(|| LAST_CASE_FILE.peek().clone().map(CaseSource::File))
}

/// A dialog's chosen source as it applies now. "This tab's data" always
/// means the ACTIVE tab's current data: a dialog can outlive a tab change
/// (Cmd+T, closing a tab), and must never learn from another tab's cases.
/// `None` when the active tab has no data.
pub fn effective_source(chosen: &Option<CaseSource>) -> Option<CaseSource> {
    match chosen {
        Some(CaseSource::Tab(_)) => SESSION.read().active_tab().data.clone().map(CaseSource::Tab),
        other => other.clone(),
    }
}

/// Source chooser shared by the learn dialogs: the tab's data (when it has
/// some) or a CSV/TSV file.
#[component]
pub fn CaseSourcePicker(source: Signal<Option<CaseSource>>, disabled: bool) -> Element {
    let mut source = source;
    let tab_data = SESSION.read().active_tab().data.clone();
    let is_tab = matches!(*source.read(), Some(CaseSource::Tab(_)));
    let file_label = match &*source.read() {
        Some(CaseSource::File(f)) => f.label(),
        // The file row is the alternative to the tab's data: offer the
        // last file. Without tab data nothing is chosen yet.
        _ if tab_data.is_some() => LAST_CASE_FILE
            .read()
            .as_ref()
            .map(|f| f.label())
            .unwrap_or_else(|| "no file selected".into()),
        _ => "no file selected".into(),
    };
    let pick = move |_| {
        spawn(async move {
            if let Some(f) = pick_case_file().await {
                source.set(Some(CaseSource::File(f)));
            }
        });
    };
    rsx! {
        if let Some(d) = tab_data {
            div { class: "flex items-center gap-1.5",
                input {
                    id: "cs-tab",
                    r#type: "radio",
                    name: "cs-source",
                    checked: is_tab,
                    disabled,
                    onchange: move |_| source.set(Some(CaseSource::Tab(d.clone()))),
                }
                label { r#for: "cs-tab", "This tab's data ({d.rows} rows)" }
            }
            div { class: "flex items-center gap-1.5",
                input {
                    id: "cs-file",
                    r#type: "radio",
                    name: "cs-source",
                    checked: !is_tab,
                    disabled,
                    onchange: move |_| source.set(LAST_CASE_FILE.peek().clone().map(CaseSource::File)),
                }
                button { class: BTN_SM, disabled, onclick: pick, "Choose CSV/TSV…" }
                span { class: "truncate font-mono text-xs text-muted-foreground", "{file_label}" }
            }
        } else {
            div { class: "flex items-center gap-2",
                button { class: BTN_SM, disabled, onclick: pick, "Choose CSV/TSV…" }
                span { class: "truncate font-mono text-xs text-muted-foreground", "{file_label}" }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Jobs
// ---------------------------------------------------------------------------

struct JobHandle {
    id: u64,
    /// Fraction done as `f32` bits, written by the job body.
    progress: Arc<AtomicU32>,
}

/// Register a job; a detached loop mirrors its progress into `DATA_JOBS`.
fn start_job(kind: DataJobKind, tab: Option<TabId>, body: Option<u64>) -> JobHandle {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let progress = Arc::new(AtomicU32::new(0f32.to_bits()));
    DATA_JOBS.write().push(DataJob { id, kind, tab, body, frac: None });
    let p = progress.clone();
    spawn_forever(async move {
        loop {
            platform::sleep_ms(150).await;
            let frac = f32::from_bits(p.load(Ordering::Relaxed));
            let current = DATA_JOBS.peek().iter().find(|j| j.id == id).map(|j| j.frac);
            match current {
                None => break,
                Some(f) if f != Some(frac) && frac > 0.0 => {
                    if let Some(j) = DATA_JOBS.write().iter_mut().find(|j| j.id == id) {
                        j.frac = Some(frac);
                    }
                }
                Some(_) => {}
            }
        }
    });
    JobHandle { id, progress }
}

fn finish_job(id: u64) {
    DATA_JOBS.write().retain(|j| j.id != id);
}

/// Wait until no data is being converted or extracted (Save must not miss
/// it). Indexing does not change data, so it is not waited for. (Web jobs
/// run to completion without yielding, so only desktop can overlap.)
#[cfg(not(target_arch = "wasm32"))]
pub async fn wait_for_data_jobs() {
    while DATA_JOBS.peek().iter().any(|j| j.kind != DataJobKind::Index) {
        platform::sleep_ms(100).await;
    }
}

/// Reading `body` failed. If a sidecar read in place changed on disk, drop
/// the data that depended on it; otherwise remember the failure so the panel
/// shows it instead of retrying. Only the stale case writes the session —
/// any session write re-runs the panel's effects, which would read again.
pub fn report_data_error(body: u64, e: impl std::fmt::Display) {
    if data_ops::has_stale(&SESSION.read()) {
        let dropped = exec(|s| Ok(data_ops::drop_stale(s))).unwrap_or_default();
        for name in dropped {
            log_message(format!(
                "Warning: the data file of `{name}` changed on disk; its data was dropped."
            ));
        }
        return;
    }
    log_message(format!("Data error: {e}"));
    DATA_ERRORS.write().insert(body, e.to_string());
}

/// The read error recorded for `body`, if any (no subscription).
pub fn data_error(body: u64) -> Option<String> {
    DATA_ERRORS.peek().get(&body).cloned()
}

// ---------------------------------------------------------------------------
// Attach
// ---------------------------------------------------------------------------

/// Convert `bytes` (a case file) in the background and attach the result to
/// `tab`. Detached, so closing the dialog that started it does not lose it.
pub fn attach_bytes(
    tab: TabId,
    bytes: Vec<u8>,
    delim: Option<u8>,
    source: String,
    origin: &'static str,
) {
    spawn_forever(async move {
        #[cfg(target_arch = "wasm32")]
        if bytes.len() as u64 > WEB_MEMORY_WARN {
            let mb = bytes.len() / (1024 * 1024);
            let msg = format!("This data is {mb} MB and will be held in browser memory. Continue?");
            if !platform::confirm("Large data", &msg).await {
                return;
            }
        }
        let job = start_job(DataJobKind::Prepare, Some(tab), None);
        let progress = job.progress.clone();
        let dir = platform::data_cache_dir();
        let result = platform::run_blocking(move || {
            let target = match &dir {
                Some(d) => data_ops::DataTarget::Dir(d),
                None => data_ops::DataTarget::Memory,
            };
            let mut report = |f: f32| progress.store(f.to_bits(), Ordering::Relaxed);
            data_ops::prepare(&bytes, delim, target, origin, &source, &mut report)
        })
        .await;
        finish_job(job.id);
        match result {
            Ok(Ok(prepared)) => {
                let rows = prepared.rows();
                if exec(|s| Ok(data_ops::attach(s, tab, prepared))) == Some(true) {
                    let name = SESSION.read().tab_label(tab).to_string();
                    log_message(format!("Attached {rows} case(s) to `{name}`."));
                }
            }
            Ok(Err(e)) => log_message(format!("Could not attach the data: {e}")),
            Err(e) => log_message(format!("Preparing the data crashed: {e}")),
        }
    });
}

/// Attach whatever `source` reads to `tab`; nothing to do when the source
/// already is the tab's data.
pub fn attach_source(tab: TabId, source: CaseSource, origin: &'static str) {
    let CaseSource::File(file) = source else { return };
    spawn_forever(async move {
        match file.read().await {
            Ok(bytes) => attach_bytes(tab, bytes, file.forced_delim(), file.label(), origin),
            Err(e) => log_message(format!("Could not attach the data: {e}")),
        }
    });
}

/// Log what attaching a `.baltsv`'s sections did; `banner` offers to view
/// the data (it arrived with an opened project).
pub fn report_attach(rep: &AttachReport, file: &str, banner: bool) {
    for w in rep.warnings(file) {
        log_message(format!("Warning: {w}"));
    }
    if rep.attached > 0 {
        log_message(format!("Loaded case data of {} tab(s) from {file}.", rep.attached));
        if banner {
            *DATA_BANNER.write() = Some(format!("Case data found for {} tab(s).", rep.attached));
        }
    }
}

/// Desktop, after opening `path`: bring its data along — discover the
/// `.baltsv` next to a `.balina`, or extract a `.balpck`'s data.
#[cfg(not(target_arch = "wasm32"))]
pub fn after_open(path: &std::path::Path) {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "balina" | "json" => {
            let sidecar = data_ops::sidecar_path(path);
            let name = sidecar.file_name().unwrap_or_default().to_string_lossy().into_owned();
            match crate::state::exec_res(|s| data_ops::discover_sidecar(s, path)) {
                Ok(Some(rep)) => report_attach(&rep, &name, true),
                Ok(None) => {}
                Err(e) => log_message(format!("Warning: {name}: {e}")),
            }
        }
        "balpck" => {
            let Some(dir) = platform::data_cache_dir() else { return };
            let pack = path.to_path_buf();
            let name = pack.file_name().unwrap_or_default().to_string_lossy().into_owned();
            spawn_forever(async move {
                let job = start_job(DataJobKind::Extract, None, None);
                let src = pack.clone();
                let res = platform::run_blocking(move || data_ops::extract_pack_file(&src, &dir)).await;
                finish_job(job.id);
                match res {
                    Ok(Ok(Some(loaded))) => {
                        if let Some(rep) = exec(|s| Ok(data_ops::attach_pack_data(s, &pack, &loaded))) {
                            report_attach(&rep, &name, true);
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(e)) => log_message(format!("Warning: cannot read the data in {name}: {e}")),
                    Err(e) => log_message(format!("Reading the data in {name} crashed: {e}")),
                }
            });
        }
        _ => {}
    }
}

/// Build the row index of data read in place (lazily, for the panel).
pub fn ensure_index(data: TabData, tab: TabId) {
    let body = data.body_id();
    let running = DATA_JOBS.peek().iter().any(|j| j.kind == DataJobKind::Index && j.body == Some(body));
    if running || data_error(body).is_some() {
        return;
    }
    let job = start_job(DataJobKind::Index, Some(tab), Some(body));
    spawn_forever(async move {
        let d = data.clone();
        let res = platform::run_blocking(move || data_ops::index(&d)).await;
        finish_job(job.id);
        match res {
            Ok(Ok(stats)) => {
                exec(|s| {
                    data_ops::set_index(s, &data, stats);
                    Ok(())
                });
            }
            Ok(Err(e)) => report_data_error(body, e),
            Err(e) => {
                log_message(format!("Indexing the data crashed: {e}"));
                DATA_ERRORS.write().insert(body, e);
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Menu flows
// ---------------------------------------------------------------------------

/// Cases > Attach Data…
pub fn attach_data_flow() {
    let tab = SESSION.read().active_id();
    spawn_forever(async move {
        let Some(file) = platform::pick_data_file().await else { return };
        let label = file.label();
        if !file.is_baltsv() {
            match file.read().await {
                Ok(bytes) => {
                    attach_bytes(tab, bytes, file.forced_delim(), label, data_ops::ORIGIN_ATTACH);
                    *SHOW_DATA_PANEL.write() = true;
                }
                Err(e) => log_message(format!("Could not attach the data: {e}")),
            }
            return;
        }
        let loaded = match platform::load_baltsv(&file).await {
            Ok(l) => l,
            Err(e) => {
                log_message(format!("Cannot open {label}: {e}"));
                return;
            }
        };
        let same_project = loaded.file.project_uid == SESSION.read().uid;
        if same_project {
            if let Some(rep) = exec(|s| Ok(data_ops::attach_loaded(s, &loaded, true))) {
                report_attach(&rep, &label, false);
            }
        } else if loaded.file.sections.len() == 1 {
            exec(|s| Ok(data_ops::attach_section(s, tab, &loaded, 0)));
            log_message(format!("Attached the data in {label}."));
        } else if loaded.file.sections.is_empty() {
            log_message(format!("{label} holds no data."));
            return;
        } else {
            *SECTION_PICK.write() = Some(Arc::new(loaded));
            open_dialog(DialogDesc::DataSectionPick { tab });
            return;
        }
        *SHOW_DATA_PANEL.write() = true;
    });
}

/// Cases > Remove Data (confirmed: it is not undoable).
pub fn remove_data_flow() {
    let (tab, has_data, name) = {
        let s = SESSION.read();
        let id = s.active_id();
        (id, s.active_tab().data.is_some(), s.tab_label(id).to_string())
    };
    if !has_data {
        log_message("This tab has no data.");
        return;
    }
    spawn_forever(async move {
        let msg = format!("Remove the case data attached to `{name}`? This cannot be undone.");
        if !platform::confirm("Remove data", &msg).await {
            return;
        }
        exec(|s| {
            data_ops::remove(s, tab);
            Ok(())
        });
        log_message(format!("Removed the case data of `{name}`."));
    });
}

pub fn toggle_panel() {
    let open = *SHOW_DATA_PANEL.peek();
    *SHOW_DATA_PANEL.write() = !open;
}

/// File > Save Data As…: a copy of every tab's data as one `.baltsv`.
#[cfg(not(target_arch = "wasm32"))]
pub fn save_data_as_flow() {
    spawn(async move {
        let stem = {
            let s = SESSION.read();
            if !data_ops::any_data(&s) {
                drop(s);
                log_message("No tab has data to save.");
                return;
            }
            s.path
                .as_ref()
                .and_then(|p| p.file_stem())
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| s.doc().net.name.clone())
        };
        let picked = rfd::AsyncFileDialog::new()
            .add_filter("Balina data", &["baltsv"])
            .set_file_name(format!("{stem}.baltsv"))
            .save_file()
            .await;
        let Some(fh) = picked else { return };
        wait_for_data_jobs().await;
        let path = fh.path().to_path_buf();
        if let Some(n) = exec(|s| data_ops::save_data_as(s, &path)) {
            log_message(format!("Saved data of {n} tab(s) to {}.", path.display()));
        }
    });
}
