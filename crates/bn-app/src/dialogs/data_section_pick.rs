//! Attach one section of a `.baltsv` from another project: its tab uids
//! mean nothing here, so the user picks which data goes to the tab.

use bn_session::ops::data as data_ops;
use bn_session::TabId;
use dioxus::prelude::*;

use crate::state::{close_dialog, exec, log_message, SECTION_PICK, SESSION, SHOW_DATA_PANEL};
use crate::ui::{Modal, BTN, BTN_PRIMARY};

#[component]
pub fn DataSectionPickDialog(tab: TabId) -> Element {
    let mut choice = use_signal(|| 0usize);
    let Some(loaded) = SECTION_PICK.read().clone() else {
        return rsx! {};
    };
    let tab_name = match SESSION.read().tabs().get(tab) {
        Some(t) => t.doc.net.name.clone(),
        None => return rsx! {},
    };
    let close = move || {
        *SECTION_PICK.write() = None;
        close_dialog();
    };
    let attach = {
        let loaded = loaded.clone();
        move |_| {
            let idx = *choice.read();
            if exec(|s| Ok(data_ops::attach_section(s, tab, &loaded, idx))) == Some(true) {
                let name = &loaded.file.sections[idx].meta.name;
                log_message(format!("Attached the data of `{name}` to `{tab_name}`."));
                *SHOW_DATA_PANEL.write() = true;
            }
            close();
        }
    };

    rsx! {
        Modal {
            title: "Attach data",
            width: "max-w-md",
            draggable: false, // configuration dialog
            on_close: move |_| close(),
            footer: rsx! {
                button { class: BTN, onclick: move |_| close(), "Cancel" }
                button { class: BTN_PRIMARY, onclick: attach, "Attach" }
            },
            div { class: "space-y-2 text-sm",
                p { class: "text-muted-foreground",
                    "This data file belongs to another project. Choose the data to attach to this tab:"
                }
                for (i, sec) in loaded.file.sections.iter().enumerate() {
                    {
                        let id = format!("dsp-{i}");
                        let mut detail = vec![format!("{} rows", sec.rows)];
                        if !sec.meta.origin.is_empty() {
                            detail.push(sec.meta.origin.clone());
                        }
                        let detail = detail.join(" · ");
                        let name = if sec.meta.name.is_empty() { "(unnamed)".to_string() } else { sec.meta.name.clone() };
                        rsx! {
                            div { class: "flex items-center gap-1.5",
                                input {
                                    id: "{id}",
                                    r#type: "radio",
                                    name: "dsp",
                                    checked: *choice.read() == i,
                                    onchange: move |_| choice.set(i),
                                }
                                label { r#for: "{id}",
                                    span { class: "font-medium", "{name}" }
                                    span { class: "text-muted-foreground", " — {detail}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
