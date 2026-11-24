//! Simulate cases from the current network: attach them to the tab as its
//! data and/or export them as a CSV file. The generation runs off the UI
//! thread on a network clone (no session borrow held).

use dioxus::prelude::*;

use crate::state::{close_dialog, log_message, SESSION};
use crate::ui::{Modal, NumberInput, BTN, BTN_PRIMARY};

#[component]
pub fn SimulateDialog() -> Element {
    let mut n = use_signal(|| 1000usize);
    let mut missing_pct = use_signal(|| 0.0f64);
    let mut running = use_signal(|| false);
    let mut attach = use_signal(|| true);
    let mut export = use_signal(|| false);

    #[cfg(not(target_arch = "wasm32"))]
    let generate = move |_| {
        spawn(async move {
            let (attach, export) = (*attach.read(), *export.read());
            let mut path = None;
            if export {
                let picked = rfd::AsyncFileDialog::new()
                    .add_filter("CSV", &["csv"])
                    .set_file_name("cases.csv")
                    .save_file()
                    .await;
                let Some(fh) = picked else { return };
                path = Some(fh.path().to_path_buf());
            }
            running.set(true);
            let (tab, net) = {
                let s = SESSION.read();
                (s.active_id(), s.doc().net.clone())
            };
            let (count, pct) = (*n.read(), *missing_pct.read());
            let out = path.clone();
            let result = tokio::task::spawn_blocking(move || {
                let csv = bn_session::ops::learn::simulate_cases_csv(&net, count, pct)?;
                if let Some(p) = &out {
                    std::fs::write(p, &csv)?;
                }
                Ok::<_, bn_session::CmdError>(csv)
            })
            .await;
            running.set(false);
            match result {
                Ok(Ok(csv)) => {
                    let source = path
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if let Some(p) = &path {
                        log_message(format!("Wrote {count} cases to {}", p.display()));
                    }
                    if attach {
                        crate::data::attach_bytes(
                            tab,
                            csv.into_bytes(),
                            Some(b','),
                            source,
                            bn_session::ops::data::ORIGIN_SIMULATE,
                        );
                    }
                    close_dialog();
                }
                Ok(Err(e)) => log_message(format!("Simulation failed: {e}")),
                Err(e) => log_message(format!("Simulation crashed: {e}")),
            }
        });
    };

    // Web: generate on the main thread (a brief stall at very large counts);
    // the CSV export is a browser download.
    #[cfg(target_arch = "wasm32")]
    let generate = move |_| {
        running.set(true);
        let (tab, net) = {
            let s = SESSION.read();
            (s.active_id(), s.doc().net.clone())
        };
        let (count, pct) = (*n.read(), *missing_pct.read());
        match bn_session::ops::learn::simulate_cases_csv(&net, count, pct) {
            Ok(csv) => {
                if *export.read() {
                    crate::platform::download("cases.csv", "text/csv", csv.as_bytes());
                    log_message(format!("Wrote {count} cases to cases.csv (downloaded)."));
                }
                if *attach.read() {
                    let source = if *export.read() { "cases.csv".to_string() } else { String::new() };
                    crate::data::attach_bytes(
                        tab,
                        csv.into_bytes(),
                        Some(b','),
                        source,
                        bn_session::ops::data::ORIGIN_SIMULATE,
                    );
                }
                close_dialog();
            }
            Err(e) => log_message(format!("Simulation failed: {e}")),
        }
        running.set(false);
    };
    let nothing_to_do = !*attach.read() && !*export.read();

    rsx! {
        Modal {
            title: "Simulate cases",
            width: "max-w-sm",
            draggable: false, // configuration dialog
            on_close: move |_| close_dialog(),
            footer: rsx! {
                button { class: BTN, onclick: move |_| close_dialog(), "Cancel" }
                button {
                    class: BTN_PRIMARY,
                    disabled: *running.read() || nothing_to_do,
                    onclick: generate,
                    if *export.read() { "Generate…" } else { "Generate" }
                }
            },
            div { class: "space-y-3 text-sm",
                div { class: "flex items-center gap-2",
                    label { class: "w-40", "Number of cases:" }
                    NumberInput {
                        class: "h-8 w-28".to_string(),
                        min: "1".to_string(),
                        max: "1000000".to_string(),
                        value: n.read().to_string(),
                        onchange: move |v: String| {
                            if let Ok(v) = v.parse::<usize>() {
                                n.set(v.clamp(1, 1_000_000));
                            }
                        },
                    }
                }
                div { class: "flex items-center gap-2",
                    label { class: "w-40", "Missing values (%):" }
                    NumberInput {
                        class: "h-8 w-28".to_string(),
                        min: "0".to_string(),
                        max: "90".to_string(),
                        value: missing_pct.read().to_string(),
                        onchange: move |v: String| {
                            if let Ok(v) = v.parse::<f64>() {
                                missing_pct.set(v.clamp(0.0, 90.0));
                            }
                        },
                    }
                }
                div { class: "flex items-center gap-1.5",
                    input {
                        id: "sim-attach",
                        r#type: "checkbox",
                        checked: *attach.read(),
                        onchange: move |ev| attach.set(ev.checked()),
                    }
                    label { r#for: "sim-attach", "Attach to this tab" }
                }
                div { class: "flex items-center gap-1.5",
                    input {
                        id: "sim-export",
                        r#type: "checkbox",
                        checked: *export.read(),
                        onchange: move |ev| export.set(ev.checked()),
                    }
                    label { r#for: "sim-export", "Also export CSV…" }
                }
            }
        }
    }
}
