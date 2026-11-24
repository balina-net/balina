//! Web (wasm32) implementations: file handles instead of paths, blob
//! downloads instead of save dialogs, a Web Worker instead of the tokio
//! blocking pool.

use std::rc::Rc;

use base64::Engine as _;
use bn_session::ops::learn::StructureJobInput;
use bn_session::patch::StructurePatch;
use bn_session::views::StructureLearnResult;
use bn_session::{CmdError, Session};

use crate::data::CaseSource;
use crate::state::LAST_CASE_FILE;

/// A picked case file: a browser file handle (stays readable after the
/// picker closes, so "re-run with the same file" works like on desktop).
#[derive(Clone)]
pub struct CaseFile {
    handle: Rc<rfd::FileHandle>,
}

impl CaseFile {
    pub fn label(&self) -> String {
        self.handle.file_name()
    }

    pub fn forced_delim(&self) -> Option<u8> {
        bn_session::ops::learn::delim_for_name(&self.label())
    }

    pub async fn read(&self) -> Result<Vec<u8>, String> {
        Ok(self.handle.read().await)
    }

    pub fn is_baltsv(&self) -> bool {
        self.label().to_ascii_lowercase().ends_with(".baltsv")
    }
}

/// Pick a file to attach as a tab's data (CSV/TSV, or a `.baltsv`).
pub async fn pick_data_file() -> Option<CaseFile> {
    let fh = rfd::AsyncFileDialog::new()
        .set_title("Choose data file")
        .add_filter("Case data", &["csv", "tsv", "baltsv"])
        .pick_file()
        .await?;
    Some(CaseFile { handle: Rc::new(fh) })
}

/// Read a picked `.baltsv` into memory.
pub async fn load_baltsv(file: &CaseFile) -> Result<bn_session::ops::data::LoadedData, String> {
    bn_session::ops::data::load_baltsv_bytes(file.read().await?).map_err(|e| e.to_string())
}

/// No threads on the web: run `f` right here.
pub async fn run_blocking<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    Ok(f())
}

/// The web keeps data in memory.
pub fn data_cache_dir() -> Option<std::path::PathBuf> {
    None
}

/// Pick a CSV/TSV case file (browser file input).
pub async fn pick_case_file() -> Option<CaseFile> {
    let fh = rfd::AsyncFileDialog::new()
        .set_title("Choose case file")
        .add_filter("Case files", &["csv", "tsv"])
        .pick_file()
        .await?;
    let picked = CaseFile { handle: Rc::new(fh) };
    *LAST_CASE_FILE.write() = Some(picked.clone());
    Some(picked)
}

pub async fn sleep_ms(ms: u64) {
    gloo_timers::future::TimeoutFuture::new(ms as u32).await;
}

/// A query parameter of the page URL, if present and non-blank. `get`
/// percent-decodes, so both the raw and the encoded form of a value work.
fn query_param(name: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
    params.get(name).filter(|v| !v.trim().is_empty())
}

/// `…/?file={url}` asks the app to open that network on startup.
pub fn initial_file_url() -> Option<String> {
    query_param("file")
}

/// `…/?ui={json}` configures chrome visibility and the read-only gate; see
/// `crate::ui_config`.
pub fn ui_config_param() -> Option<String> {
    query_param("ui")
}

fn js_err(v: wasm_bindgen::JsValue) -> String {
    use wasm_bindgen::JsCast;
    match v.dyn_ref::<js_sys::Error>() {
        Some(e) => String::from(e.message()),
        None => v.as_string().unwrap_or_else(|| format!("{v:?}")),
    }
}

/// GET `url` with the browser's fetch and return the body as text. `Err` is
/// a readable message (network/CORS failure or non-2xx status).
pub async fn fetch_text(url: &str) -> Result<String, String> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    let window = web_sys::window().ok_or("no window")?;
    let resp = JsFuture::from(window.fetch_with_str(url)).await.map_err(js_err)?;
    let resp: web_sys::Response = resp.dyn_into().map_err(js_err)?;
    if !resp.ok() {
        return Err(format!("HTTP {} {}", resp.status(), resp.status_text()));
    }
    let text = JsFuture::from(resp.text().map_err(js_err)?).await.map_err(js_err)?;
    text.as_string().ok_or_else(|| "response body is not text".into())
}

/// GET `url` and return the body as bytes (binary files: `.balpck`).
pub async fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    let window = web_sys::window().ok_or("no window")?;
    let resp = JsFuture::from(window.fetch_with_str(url)).await.map_err(js_err)?;
    let resp: web_sys::Response = resp.dyn_into().map_err(js_err)?;
    if !resp.ok() {
        return Err(format!("HTTP {} {}", resp.status(), resp.status_text()));
    }
    let buf = JsFuture::from(resp.array_buffer().map_err(js_err)?).await.map_err(js_err)?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

/// The browser tab title.
pub fn set_window_title(title: &str) {
    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
        doc.set_title(title);
    }
}

/// Browser confirm dialog; true = OK.
pub async fn confirm(title: &str, message: &str) -> bool {
    web_sys::window()
        .and_then(|w| w.confirm_with_message(&format!("{title}\n\n{message}")).ok())
        .unwrap_or(false)
}

/// Trigger a browser download of `bytes` as `filename`.
pub fn download(filename: &str, mime: &str, bytes: &[u8]) {
    let eval = dioxus::document::eval(
        r#"
        const [name, mime, b64] = await dioxus.recv();
        const bin = atob(b64);
        const arr = new Uint8Array(bin.length);
        for (let i = 0; i < bin.length; i++) arr[i] = bin.charCodeAt(i);
        const url = URL.createObjectURL(new Blob([arr], { type: mime }));
        const a = document.createElement("a");
        a.href = url;
        a.download = name;
        a.click();
        setTimeout(() => URL.revokeObjectURL(url), 10_000);
        "#,
    );
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    let _ = eval.send((filename, mime, b64));
}

/// Rasterize an SVG with the browser's own canvas and download it as a PNG.
/// The exported SVG carries explicit width/height, so `naturalWidth` is
/// defined in every engine (including Safari).
pub fn download_png_from_svg(filename: &str, svg: &str, scale: f64) {
    let eval = dioxus::document::eval(
        r#"
        const [name, svg, scale] = await dioxus.recv();
        const url = URL.createObjectURL(new Blob([svg], { type: "image/svg+xml" }));
        try {
            const img = new Image();
            await new Promise((ok, err) => { img.onload = ok; img.onerror = err; img.src = url; });
            const c = document.createElement("canvas");
            c.width = Math.max(1, Math.ceil(img.naturalWidth * scale));
            c.height = Math.max(1, Math.ceil(img.naturalHeight * scale));
            const ctx = c.getContext("2d");
            ctx.scale(scale, scale);
            ctx.drawImage(img, 0, 0);
            c.toBlob((b) => {
                const a = document.createElement("a");
                a.href = URL.createObjectURL(b);
                a.download = name;
                a.click();
                setTimeout(() => URL.revokeObjectURL(a.href), 10_000);
            }, "image/png");
        } finally {
            URL.revokeObjectURL(url);
        }
        "#,
    );
    let _ = eval.send((filename, svg, scale));
}

/// What the web worker sends back: a by-name patch (NodeIds don't survive
/// the serialization boundary — see `bn_session::patch`).
pub enum StructureOutcome {
    Done { patch: StructurePatch, report: String, summary: String, warnings: Vec<String> },
    Cancelled,
    Failed(String),
}

/// Run the structure-learning job in the `bn-worker` Web Worker, streaming
/// progress into `JOB_PROGRESS`. `Err` = the worker itself failed to start.
pub async fn run_structure_job(
    input: StructureJobInput,
    cases: CaseSource,
) -> Result<StructureOutcome, String> {
    crate::platform::worker::run(input, cases).await
}

/// Web applies the outcome by name-based patch. Mirrors
/// `ops::learn::apply_structure_outcome` (clears the busy slot, same errors).
pub fn apply_structure_outcome(
    s: &mut Session,
    tab_id: bn_session::TabId,
    outcome: StructureOutcome,
    started_seq: u64,
) -> Result<StructureLearnResult, CmdError> {
    match outcome {
        StructureOutcome::Done { patch, report, summary, warnings } => {
            bn_session::patch::apply_structure_patch(s, tab_id, &patch, report, summary, warnings, started_seq)
        }
        StructureOutcome::Cancelled => {
            s.active_tab_mut().job_cancel = None;
            Err(CmdError::Cancelled)
        }
        StructureOutcome::Failed(e) => {
            s.active_tab_mut().job_cancel = None;
            Err(CmdError::Learn(e))
        }
    }
}
