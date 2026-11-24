//! Web file flows: open through the browser file input (rfd wasm), save and
//! export as blob downloads. Same public surface as the desktop `file_ops`.
//! Case data only travels in `.balpck` files here (no sidecars: there are
//! no paths to find them by).

use bn_core::io;
use bn_session::ops;
use dioxus::prelude::*;

use crate::state::{exec, log_message, open_dialog, DialogDesc, DATA_BANNER, SESSION};

const OPEN_FILTERS: &[(&str, &[&str])] = &[
    ("All networks", &["balina", "balpck", "json", "xml", "bif", "xmlbif", "xdsl"]),
    ("Balina", &["balina", "json"]),
    ("Balina pack (project + data)", &["balpck"]),
    ("XMLBIF", &["xml", "bif", "xmlbif"]),
    ("XDSL (GeNIe)", &["xdsl"]),
];

/// True to proceed (document unmodified, or the user confirmed discarding).
async fn confirm_discard(action: &str) -> bool {
    if !SESSION.read().modified {
        return true;
    }
    crate::platform::confirm(
        "Unsaved changes",
        &format!("The network has unsaved changes. {action} anyway?"),
    )
    .await
}

pub fn file_new() {
    spawn(async move {
        if !confirm_discard("Discard them and start a new network").await {
            return;
        }
        exec(|s| {
            ops::file::doc_new(s);
            Ok(())
        });
        *DATA_BANNER.write() = None;
        crate::state::clear_selection();
        log_message("New network.");
    });
}

/// Replace the document with parsed `text`, then reset selection, fit the
/// view and report — shared tail of the picker and URL open flows.
fn open_text(text: &str, fmt: io::Format) {
    if exec(|s| ops::file::doc_open_str(s, text, fmt)).is_some() {
        opened();
    }
}

/// Open a `.balpck` (project + data, the data held in memory). Large data
/// asks first: it all lives in browser memory.
async fn open_pack(bytes: Vec<u8>, name: &str) {
    let data_len = ops::data::pack_data_len(std::io::Cursor::new(&bytes)).ok().flatten().unwrap_or(0);
    if data_len > crate::data::WEB_MEMORY_WARN {
        let mb = data_len / (1024 * 1024);
        let msg = format!("This pack holds {mb} MB of data, which will be held in browser memory. Continue?");
        if !crate::platform::confirm("Large data", &msg).await {
            return;
        }
    }
    if let Some(rep) = exec(|s| ops::file::doc_open_pack_bytes(s, &bytes)) {
        opened();
        crate::data::report_attach(&rep, name, true);
    }
}

/// Shared tail of every successful open: reset selection, fit, report.
fn opened() {
    *DATA_BANNER.write() = None;
    crate::state::clear_selection();
    *crate::canvas::controller::FIT_REQUEST.write() += 1;
    let (name, n) = {
        let s = SESSION.read();
        (s.doc().net.name.clone(), s.doc().net.len())
    };
    log_message(format!("Loaded `{name}` ({n} nodes)."));
}

pub fn file_open() {
    spawn(async move {
        if !confirm_discard("Discard them and open another file").await {
            return;
        }
        let mut d = rfd::AsyncFileDialog::new().set_title("Open network");
        for (name, exts) in OPEN_FILTERS {
            d = d.add_filter(*name, exts);
        }
        let Some(fh) = d.pick_file().await else { return };
        let name = fh.file_name();
        let fmt = match io::Format::from_path(std::path::Path::new(&name)) {
            Ok(f) => f,
            Err(e) => {
                log_message(format!("Open failed: {e}"));
                return;
            }
        };
        if fmt == io::Format::Pack {
            open_pack(fh.read().await, &name).await;
            return;
        }
        let text = match String::from_utf8(fh.read().await) {
            Ok(t) => t,
            Err(e) => {
                log_message(format!("Open failed: {e}"));
                return;
            }
        };
        open_text(&text, fmt);
    });
}

/// Fetch `url` and open it (the `?file={url}` startup path; a `.balpck`
/// is fetched as bytes). Format comes from the URL's extension, defaulting to the native format when there is
/// none — a wrong guess just surfaces as a parse error in the message log.
pub fn open_url(url: String) {
    spawn(async move {
        let path = url.split(['?', '#']).next().unwrap_or("");
        if io::Format::from_path(std::path::Path::new(path)).ok() == Some(io::Format::Pack) {
            match crate::platform::fetch_bytes(&url).await {
                Ok(bytes) => {
                    let name = path.rsplit('/').next().unwrap_or(path).to_string();
                    open_pack(bytes, &name).await;
                }
                Err(e) => log_message(format!("Open failed: {e} ({url})")),
            }
            return;
        }
        let text = match crate::platform::fetch_text(&url).await {
            Ok(t) => t,
            Err(e) => {
                log_message(format!("Open failed: {e} ({url})"));
                return;
            }
        };
        let fmt = io::Format::from_path(std::path::Path::new(path))
            .unwrap_or(io::Format::NativeJson);
        open_text(&text, fmt);
    });
}

/// Save and Save As are the same on the web: pick a name + format, download.
pub fn file_save(_save_as: bool) {
    open_dialog(DialogDesc::SaveAsWeb);
}
