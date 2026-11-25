//! In-app menu bar for the web build (browsers have no native menu). Every
//! item routes through `chrome::menu::route` by the same id strings the
//! native muda menu uses, so the two menus can never drift apart. On desktop
//! the component renders nothing.

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use dioxus::prelude::*;

    /// Desktop has the native muda menu; render nothing.
    #[component]
    pub fn MenuBar() -> Element {
        rsx! {}
    }
}

pub use imp::MenuBar;

#[cfg(target_arch = "wasm32")]
mod imp {
    use dioxus::prelude::*;

    use crate::state::{SESSION, SHOW_DATA_PANEL};

    /// What a checkable item's checkmark reflects.
    #[derive(Clone, Copy, PartialEq)]
    enum Check {
        None,
        /// `doc.auto_update`.
        AutoUpdate,
        /// The data panel is open.
        DataPanel,
    }

    struct Item {
        id: &'static str,
        label: &'static str,
        /// Shown as a hint; the shortcut itself lives in `chrome::hotkeys`.
        accel: Option<&'static str>,
        sep_before: bool,
        check: Check,
    }

    const fn item(id: &'static str, label: &'static str) -> Item {
        Item { id, label, accel: None, sep_before: false, check: Check::None }
    }
    const fn item_accel(id: &'static str, label: &'static str, accel: &'static str) -> Item {
        Item { id, label, accel: Some(accel), sep_before: false, check: Check::None }
    }
    const fn item_sep(id: &'static str, label: &'static str) -> Item {
        Item { id, label, accel: None, sep_before: true, check: Check::None }
    }

    const MENUS: &[(&str, &[Item])] = &[
        (
            "File",
            &[
                item_accel("file.new", "New", "⌘N"),
                item_accel("file.open", "Open…", "⌘O"),
                item_accel("file.save", "Save…", "⌘S"),
                Item {
                    id: "file.saveAs",
                    label: "Save As…",
                    accel: Some("⇧⌘S"),
                    sep_before: false,
                    check: Check::None,
                },
                item_sep("file.exportSvg", "Export as SVG"),
                item("file.exportPng", "Export as PNG"),
            ],
        ),
        (
            "Edit",
            &[
                item_accel("edit.undo", "Undo", "⌘Z"),
                item_accel("edit.redo", "Redo", "⇧⌘Z"),
                item_sep("edit.selectAll", "Select All Nodes"),
                item_accel("edit.deleteSelection", "Delete Selection", "⌫"),
            ],
        ),
        (
            "Network",
            &[
                item_accel("tab.new", "New Tab", "⌘T"),
                item_accel("tab.close", "Close Tab", "⌘W"),
                item("tab.duplicate", "Duplicate Tab"),
                Item {
                    id: "network.compile",
                    label: "Compile Now",
                    accel: Some("F5"),
                    sep_before: true,
                    check: Check::None,
                },
                Item {
                    id: "network.autoUpdate",
                    label: "Auto Update Beliefs",
                    accel: None,
                    sep_before: false,
                    check: Check::AutoUpdate,
                },
                item_sep("network.removeFindings", "Remove All Findings"),
                item("network.rename", "Network Name…"),
            ],
        ),
        (
            "Cases",
            &[
                item("cases.learnCpts", "Learn CPTs from Cases…"),
                item("cases.learnStructure", "Learn Structure from Cases…"),
                item("cases.simulate", "Simulate Cases…"),
                item_sep("cases.attachData", "Attach Data…"),
                item("cases.removeData", "Remove Data"),
                Item {
                    id: "cases.showDataPanel",
                    label: "Show Data Panel",
                    accel: None,
                    sep_before: false,
                    check: Check::DataPanel,
                },
            ],
        ),
        (
            "Tools",
            &[
                item("tools.sensitivity", "Sensitivity to Findings…"),
                item("tools.arcStrength", "Arc Strengths…"),
                item("tools.solveId", "Solve Influence Diagram"),
                item_sep("help.about", "About Balina"),
            ],
        ),
    ];

    const MENU_BTN: &str = "rounded px-2 py-0.5 text-sm hover:bg-accent";
    const MENU_BTN_OPEN: &str = "rounded px-2 py-0.5 text-sm bg-accent";
    const MENU_ITEM: &str = "flex w-full cursor-default items-center justify-between gap-6 \
                             rounded-sm px-2 py-1.5 text-left text-sm hover:bg-accent \
                             disabled:pointer-events-none disabled:opacity-50";

    #[component]
    pub fn MenuBar() -> Element {
        let mut open: Signal<Option<usize>> = use_signal(|| None);
        let (auto_update, tab_has_data) = {
            let s = SESSION.read();
            (s.doc().auto_update, s.active_tab().data.is_some())
        };
        let panel_open = *SHOW_DATA_PANEL.read();
        // In a read-only view the menu shows only what `route` would still
        // accept — same predicate, so the two can never disagree. Submenus
        // left empty by the filter are dropped entirely.
        let ro = crate::ui_config::read_only();
        let menus: Vec<(&'static str, Vec<&'static Item>)> = MENUS
            .iter()
            .map(|(title, items)| {
                let keep: Vec<&'static Item> = items
                    .iter()
                    .filter(|it| !ro || crate::chrome::menu::allowed_in_read_only(it.id))
                    .collect();
                (*title, keep)
            })
            .filter(|(_, items)| !items.is_empty())
            .collect();
        let checked = move |c: Check| match c {
            Check::None => false,
            Check::AutoUpdate => auto_update,
            Check::DataPanel => panel_open,
        };
        let is_open = *open.read();

        rsx! {
            // z-50 keeps the whole bar (titles + dropdowns) above the z-40
            // click-catcher so hover-switching between open menus works.
            div { class: "relative z-50 flex shrink-0 items-center gap-0.5 border-b bg-background px-2 py-1",
                for (i, (title, items)) in menus.iter().enumerate() {
                    div { class: "relative",
                        button {
                            class: if is_open == Some(i) { MENU_BTN_OPEN } else { MENU_BTN },
                            onclick: move |_| {
                                let cur = *open.peek();
                                open.set(if cur == Some(i) { None } else { Some(i) });
                            },
                            onmouseenter: move |_| {
                                // Menu-bar convention: once one menu is open,
                                // hovering moves between menus.
                                if open.peek().is_some() {
                                    open.set(Some(i));
                                }
                            },
                            "{title}"
                        }
                        if is_open == Some(i) {
                            div {
                                class: "absolute top-full left-0 z-50 mt-1 w-64 rounded-md border \
                                        bg-popover p-1 text-popover-foreground shadow-md",
                                for (j, it) in items.iter().enumerate() {
                                    // `j > 0`: a filtered submenu must not
                                    // open with a dangling rule.
                                    if it.sep_before && j > 0 {
                                        div { class: "my-1 border-t" }
                                    }
                                    button {
                                        class: MENU_ITEM,
                                        disabled: it.id == "cases.removeData" && !tab_has_data,
                                        onclick: {
                                            let id = it.id;
                                            move |_| {
                                                open.set(None);
                                                crate::chrome::menu::route(id);
                                            }
                                        },
                                        span {
                                            if checked(it.check) {
                                                "✓ {it.label}"
                                            } else {
                                                "{it.label}"
                                            }
                                        }
                                        if let Some(a) = it.accel {
                                            span { class: "text-xs text-muted-foreground", "{a}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // Click-catcher: closes the open dropdown on any outside click.
            if is_open.is_some() {
                div {
                    class: "fixed inset-0 z-40",
                    onclick: move |_| open.set(None),
                }
            }
        }
    }
}
