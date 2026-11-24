//! Platform facade: the few places where desktop (native pickers, fs, tokio
//! blocking pool, muda window) and web (file handles, downloads, web worker)
//! genuinely differ. Everything else in the app is shared. Both files expose
//! the same surface:
//!
//! - `CaseFile` + `pick_case_file` — case-file picking/reading for learning
//! - `pick_data_file`, `load_baltsv`, `data_cache_dir`, `run_blocking` —
//!   tab data (desktop: cache-dir snapshots + blocking pool; web: memory)
//! - `sleep_ms`, `set_window_title`, `confirm`
//! - `run_structure_job` / `apply_structure_outcome` — the long-job engine
//!   (desktop: `spawn_blocking`; web: a Web Worker running `bn-worker`)

#[cfg(not(target_arch = "wasm32"))]
mod desktop;
#[cfg(not(target_arch = "wasm32"))]
pub use desktop::*;

#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
pub(crate) mod worker;
#[cfg(target_arch = "wasm32")]
pub use web::*;
