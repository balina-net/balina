//! Desktop implementations: rfd native dialogs, std::fs, tokio blocking pool.

use std::path::PathBuf;
use std::sync::OnceLock;

use bn_session::jobs::{JobCtx, JobEvent};
use bn_session::ops::learn::{StructureJobInput, StructureOutcome};
use bn_session::views::StructureLearnResult;
use bn_session::{CmdError, Session};
use dioxus::prelude::*;

use crate::data::CaseSource;
use crate::state::{JOB_PROGRESS, LAST_CASE_FILE};

/// A picked case file: a plain path on desktop.
#[derive(Clone, Debug)]
pub struct CaseFile {
    path: PathBuf,
}

impl CaseFile {
    /// File name for dialog labels.
    pub fn label(&self) -> String {
        self.path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    pub fn forced_delim(&self) -> Option<u8> {
        bn_session::ops::learn::delim_for_name(&self.path.to_string_lossy())
    }

    pub async fn read(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.path).map_err(|e| format!("cannot open file: {e}"))
    }

    pub fn is_baltsv(&self) -> bool {
        self.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("baltsv"))
    }
}

/// Pick a file to attach as a tab's data (CSV/TSV, or a `.baltsv`). Does
/// not touch `LAST_CASE_FILE`: a `.baltsv` is no input for learning.
pub async fn pick_data_file() -> Option<CaseFile> {
    let mut d = rfd::AsyncFileDialog::new()
        .add_filter("Case data", &["csv", "tsv", "baltsv"])
        .add_filter("Balina data", &["baltsv"]);
    if let Some(last) = LAST_CASE_FILE.peek().as_ref().and_then(|f| f.path.parent()) {
        d = d.set_directory(last);
    }
    let fh = d.pick_file().await?;
    Some(CaseFile { path: fh.path().to_path_buf() })
}

/// Open a picked `.baltsv` to read in place.
pub async fn load_baltsv(file: &CaseFile) -> Result<bn_session::ops::data::LoadedData, String> {
    let path = file.path.clone();
    run_blocking(move || bn_session::ops::data::load_baltsv_file(&path))
        .await?
        .map_err(|e| e.to_string())
}

/// Run `f` on the blocking pool. `Err` = the task itself crashed.
pub async fn run_blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())
}

/// Parent of every launch's data folder.
fn data_cache_root() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("balina").join("data")
}

/// Lock file every launch folder holds an exclusive lock on while its
/// process runs, so other launches' sweeps can tell live folders from
/// crash leftovers.
const LIVE_LOCK: &str = ".live";

/// This launch's folder for data snapshots and extracted packs. Always
/// `Some` on desktop (`None` = keep data in memory, the web's choice).
pub fn data_cache_dir() -> Option<PathBuf> {
    // The lock file stays open (and locked) for the life of the process.
    static DIR: OnceLock<(PathBuf, Option<std::fs::File>)> = OnceLock::new();
    let (dir, _lock) = DIR.get_or_init(|| {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let dir = data_cache_root().join(format!("{secs}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let lock = std::fs::File::create(dir.join(LIVE_LOCK)).ok().filter(|f| f.lock().is_ok());
        (dir, lock)
    });
    Some(dir.clone())
}

/// Startup: remove launch folders a crash left behind.
pub fn sweep_data_cache() {
    sweep_launch_dirs(&data_cache_root(), std::time::Duration::from_secs(7 * 24 * 3600));
}

/// Remove every launch folder under `root` whose process is gone: its live
/// lock can be taken. Folders without a usable lock (created by an older
/// build, or caught mid-creation) fall back to an age rule: older than
/// `max_age`.
fn sweep_launch_dirs(root: &std::path::Path, max_age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let old = || {
            entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > max_age)
        };
        // Scoped so the probe's handle is closed before deleting.
        let dead = match std::fs::File::open(path.join(LIVE_LOCK)) {
            Ok(f) => match f.try_lock() {
                Ok(()) => true,
                Err(std::fs::TryLockError::WouldBlock) => false,
                Err(std::fs::TryLockError::Error(_)) => old(),
            },
            Err(_) => old(),
        };
        if dead {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// Exit: this launch's snapshots are never needed again.
pub fn clear_data_cache() {
    if let Some(dir) = data_cache_dir() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Pick a CSV/TSV case file; remembers the pick for the next dialog.
pub async fn pick_case_file() -> Option<CaseFile> {
    let mut d = rfd::AsyncFileDialog::new().add_filter("Case files", &["csv", "tsv"]);
    if let Some(last) = LAST_CASE_FILE.peek().as_ref().and_then(|f| f.path.parent()) {
        d = d.set_directory(last);
    }
    let fh = d.pick_file().await?;
    let picked = CaseFile { path: fh.path().to_path_buf() };
    *LAST_CASE_FILE.write() = Some(picked.clone());
    Some(picked)
}

pub async fn sleep_ms(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

pub fn set_window_title(title: &str) {
    dioxus::desktop::window().set_title(title);
}

/// Native warning dialog with OK/Cancel; true = OK.
pub async fn confirm(title: &str, message: &str) -> bool {
    let choice = rfd::AsyncMessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(title)
        .set_description(message)
        .set_buttons(rfd::MessageButtons::OkCancel)
        .show()
        .await;
    matches!(choice, rfd::MessageDialogResult::Ok)
}

/// Run a prepared structure-learning job off the UI thread, streaming
/// progress into `JOB_PROGRESS`. `Err` = the blocking task itself crashed.
pub async fn run_structure_job(
    input: StructureJobInput,
    cases: CaseSource,
) -> Result<StructureOutcome, String> {
    let cancel = input.cancel.clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<JobEvent>();
    let jc = JobCtx::new(cancel, move |e| {
        let _ = tx.send(e);
    });
    let handle = tokio::task::spawn_blocking(move || {
        // Read inside the blocking task; same failure text as when the job
        // body did the open itself.
        let bytes = match &cases {
            CaseSource::File(f) => std::fs::read(&f.path).map_err(|e| format!("cannot open file: {e}")),
            CaseSource::Tab(d) => bn_session::ops::data::case_bytes(d).map_err(|e| e.to_string()),
        };
        let bytes = match bytes {
            Ok(b) => b,
            Err(e) => return StructureOutcome::Failed(e),
        };
        bn_session::ops::learn::run_structure_job(input, &bytes, cases.forced_delim(), &jc)
    });
    // Drain progress on the UI scheduler.
    spawn(async move {
        while let Some(e) = rx.recv().await {
            *JOB_PROGRESS.write() = Some(e);
        }
    });
    handle.await.map_err(|e| e.to_string())
}

/// Desktop applies the outcome by clone-swap (NodeIds are shared).
pub fn apply_structure_outcome(
    s: &mut Session,
    tab_id: bn_session::TabId,
    outcome: StructureOutcome,
    started_seq: u64,
) -> Result<StructureLearnResult, CmdError> {
    bn_session::ops::learn::apply_structure_outcome(s, tab_id, outcome, started_seq)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_keeps_locked_and_young_launch_dirs() {
        let root = std::env::temp_dir().join(format!("balina-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (live, dead, young) = (root.join("live"), root.join("dead"), root.join("young"));
        for d in [&live, &dead, &young] {
            std::fs::create_dir_all(d).unwrap();
        }
        // A running launch holds its lock; a crashed one left an unlocked
        // file; a folder still being created has no lock file yet.
        let held = std::fs::File::create(live.join(LIVE_LOCK)).unwrap();
        held.lock().unwrap();
        std::fs::File::create(dead.join(LIVE_LOCK)).unwrap();

        sweep_launch_dirs(&root, std::time::Duration::from_secs(3600));
        assert!(live.exists(), "a live launch's folder survives, however old");
        assert!(!dead.exists(), "a crashed launch's folder goes right away");
        assert!(young.exists(), "no lock yet: only removed once old");

        drop(held);
        let _ = std::fs::remove_dir_all(&root);
    }
}
