//! Learn CPTs from a case file or the tab's data (counting or EM).
//! Synchronous — fast at editor scale. A file that was learned from becomes
//! the tab's data.

use bn_session::ops::learn::{LearnCptsOpts, LearnMethod};
use dioxus::prelude::*;

use crate::data::{default_source, effective_source, CaseSource, CaseSourcePicker};
use crate::state::{close_dialog, exec_res, log_message, show_toast, SESSION};
use crate::ui::{Modal, NumberInput, BTN, BTN_PRIMARY};

#[component]
pub fn LearnCptsDialog() -> Element {
    let source: Signal<Option<CaseSource>> = use_signal(default_source);
    let mut method = use_signal(|| LearnMethod::Counting);
    let mut em_iters = use_signal(|| 50usize);
    let mut create_nodes = use_signal(|| true);
    let mut bins = use_signal(|| 4usize);
    let mut report: Signal<Option<String>> = use_signal(|| None);

    let run = move |_| {
        let Some(src) = effective_source(&source.read()) else { return };
        let tab = SESSION.read().active_id();
        let opts = LearnCptsOpts {
            method: *method.read(),
            em_iters: *em_iters.read(),
            create_nodes: *create_nodes.read(),
            bins: *bins.read(),
        };
        spawn(async move {
            let bytes = match src.read().await {
                Ok(b) => b,
                Err(e) => {
                    report.set(Some(format!("Learning failed: {e}")));
                    return;
                }
            };
            let delim = src.forced_delim();
            match exec_res(|s| bn_session::ops::learn::learn_cpts(s, &bytes, delim, opts)) {
                Ok(res) => {
                    log_message(res.summary);
                    report.set(Some(res.report));
                    *crate::canvas::controller::FIT_REQUEST.write() += 1;
                    show_toast("Cases loaded!");
                    if let CaseSource::File(f) = &src {
                        crate::data::attach_bytes(
                            tab,
                            bytes,
                            delim,
                            f.label(),
                            bn_session::ops::data::ORIGIN_LEARN_CPTS,
                        );
                    }
                }
                Err(e) => report.set(Some(format!("Learning failed: {e}"))),
            }
        });
    };

    rsx! {
        Modal {
            title: "Learn CPTs from cases",
            draggable: false, // configuration dialog
            on_close: move |_| close_dialog(),
            footer: rsx! {
                button { class: BTN, onclick: move |_| close_dialog(), "Close" }
                button {
                    class: BTN_PRIMARY,
                    disabled: effective_source(&source.read()).is_none(),
                    onclick: run,
                    "Learn"
                }
            },
            div { class: "space-y-3 text-sm",
                CaseSourcePicker { source, disabled: false }
                div { class: "flex items-center gap-1.5",
                    input {
                        id: "lc-create",
                        r#type: "checkbox",
                        checked: *create_nodes.read(),
                        onchange: move |ev| create_nodes.set(ev.checked()),
                    }
                    label { r#for: "lc-create", "Add missing nodes from columns" }
                }
                if *create_nodes.read() {
                    div { class: "flex items-center gap-2",
                        label { "Bins for continuous variables:" }
                        NumberInput {
                            min: "2".to_string(),
                            max: "20".to_string(),
                            value: bins.read().to_string(),
                            onchange: move |v: String| {
                                if let Ok(v) = v.parse::<usize>() {
                                    bins.set(v.clamp(2, 20));
                                }
                            },
                        }
                    }
                }
                div { class: "flex gap-4",
                    for (m, mid, label) in [
                        (LearnMethod::Counting, "lc-counting", "Counting"),
                        (LearnMethod::Em, "lc-em", "EM (missing data)"),
                    ] {
                        div { class: "flex items-center gap-1.5",
                            input {
                                id: mid,
                                r#type: "radio",
                                name: "lc-method",
                                checked: *method.read() == m,
                                onchange: move |_| method.set(m),
                            }
                            label { r#for: mid, "{label}" }
                        }
                    }
                }
                if *method.read() == LearnMethod::Em {
                    div { class: "flex items-center gap-2",
                        label { "Max iterations:" }
                        NumberInput {
                            class: "h-7 w-20".to_string(),
                            min: "1".to_string(),
                            max: "500".to_string(),
                            value: em_iters.read().to_string(),
                            onchange: move |v: String| {
                                if let Ok(v) = v.parse::<usize>() {
                                    em_iters.set(v.clamp(1, 500));
                                }
                            },
                        }
                    }
                }
                if let Some(r) = report.read().clone() {
                    pre { class: "max-h-40 overflow-auto rounded border bg-muted/30 p-2 \
                                  font-mono text-[11px] whitespace-pre-wrap select-text",
                        "{r}"
                    }
                }
            }
        }
    }
}
