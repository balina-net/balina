//! Native menu bar (muda — the same crate the Tauri shell used underneath).
//! Every item routes through [`route`] by id string.
//!
//! Accelerator ownership rule: a shortcut is either a native accelerator
//! (Cmd+N/O/S, Cmd+Shift+S, F5 — safe while typing) or a frontend hotkey
//! (Cmd+Z, Cmd+A, Delete — need the not-while-typing guard), never both.
//! The predefined clipboard items are required on macOS: without them
//! Cmd+C/V/X do not work inside webview text inputs at all.

use dioxus::prelude::ReadableExt;

use crate::state::{self, exec, open_dialog, DialogDesc, SESSION};

#[cfg(not(target_arch = "wasm32"))]
use dioxus::desktop::muda::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

#[cfg(not(target_arch = "wasm32"))]
thread_local! {
    /// Handle to the Auto Update check item so `sync_menu_state` can flip it.
    static AUTO_UPDATE_ITEM: std::cell::RefCell<Option<CheckMenuItem>> =
        const { std::cell::RefCell::new(None) };
    /// Handle to the Open Recent submenu so the list can be rebuilt live.
    static RECENT_SUBMENU: std::cell::RefCell<Option<Submenu>> =
        const { std::cell::RefCell::new(None) };
    /// Data items whose enabled/checked state follows the session.
    static DATA_ITEMS: std::cell::RefCell<Option<DataItems>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(not(target_arch = "wasm32"))]
struct DataItems {
    remove: MenuItem,
    save_as: MenuItem,
    panel: CheckMenuItem,
}

#[cfg(not(target_arch = "wasm32"))]
fn item(id: &str, text: &str) -> MenuItem {
    MenuItem::with_id(id, text, true, None)
}

#[cfg(not(target_arch = "wasm32"))]
fn item_accel(id: &str, text: &str, accel: &str) -> MenuItem {
    MenuItem::with_id(id, text, true, Some(accel.parse().expect("valid accelerator")))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn build() -> Menu {
    let menu = Menu::new();

    #[cfg(target_os = "macos")]
    {
        let app_menu = Submenu::new("Balina", true);
        app_menu
            .append_items(&[
                &item("help.about", "About Balina"),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::quit(None),
            ])
            .unwrap();
        menu.append(&app_menu).unwrap();
    }

    let save_data_as = MenuItem::with_id("file.saveDataAs", "Save Data As…", false, None);
    let remove_data = MenuItem::with_id("cases.removeData", "Remove Data", false, None);
    let data_panel =
        CheckMenuItem::with_id("cases.showDataPanel", "Show Data Panel", true, false, None);

    let recent = Submenu::new("Open Recent", true);
    RECENT_SUBMENU.with(|c| *c.borrow_mut() = Some(recent.clone()));
    rebuild_recent_items(&crate::chrome::recent::load());

    let file = Submenu::new("&File", true);
    file.append_items(&[
        &item_accel("file.new", "New", "CmdOrCtrl+N"),
        &item_accel("file.open", "Open…", "CmdOrCtrl+O"),
        &recent,
        &PredefinedMenuItem::separator(),
        &item_accel("file.save", "Save", "CmdOrCtrl+S"),
        &item_accel("file.saveAs", "Save As… (balina / balpck / xmlbif / xdsl)", "CmdOrCtrl+Shift+S"),
        &save_data_as,
        &PredefinedMenuItem::separator(),
        &item("file.exportSvg", "Export as SVG…"),
        &item("file.exportPng", "Export as PNG…"),
    ])
    .unwrap();
    #[cfg(not(target_os = "macos"))]
    file.append_items(&[&PredefinedMenuItem::separator(), &PredefinedMenuItem::quit(None)])
        .unwrap();

    // Undo/Redo/Select All/Delete get NO accelerators: the frontend owns
    // Cmd+Z/Cmd+A/Delete with a not-while-typing guard, so text inputs keep
    // their native editing shortcuts.
    let edit = Submenu::new("&Edit", true);
    edit.append_items(&[
        &item("edit.undo", "Undo"),
        &item("edit.redo", "Redo"),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::cut(None),
        &PredefinedMenuItem::copy(None),
        &PredefinedMenuItem::paste(None),
        &PredefinedMenuItem::separator(),
        &item("edit.selectAll", "Select All Nodes"),
        &item("edit.deleteSelection", "Delete Selection"),
    ])
    .unwrap();

    let auto_update = CheckMenuItem::with_id(
        "network.autoUpdate",
        "Auto Update Beliefs",
        true,
        true,
        None,
    );
    let network = Submenu::new("&Network", true);
    network
        .append_items(&[
            &item_accel("tab.new", "New Tab", "CmdOrCtrl+T"),
            &item_accel("tab.close", "Close Tab", "CmdOrCtrl+W"),
            &item("tab.duplicate", "Duplicate Tab"),
            &PredefinedMenuItem::separator(),
            &item_accel("network.compile", "Compile Now", "F5"),
            &auto_update,
            &PredefinedMenuItem::separator(),
            &item("network.removeFindings", "Remove All Findings"),
            &item("network.rename", "Network Name…"),
        ])
        .unwrap();
    AUTO_UPDATE_ITEM.with(|c| *c.borrow_mut() = Some(auto_update));

    let cases = Submenu::new("&Cases", true);
    cases
        .append_items(&[
            &item("cases.learnCpts", "Learn CPTs from Cases…"),
            &item("cases.learnStructure", "Learn Structure from Cases…"),
            &item("cases.simulate", "Simulate Cases…"),
            &PredefinedMenuItem::separator(),
            &item("cases.attachData", "Attach Data…"),
            &remove_data,
            &data_panel,
        ])
        .unwrap();
    DATA_ITEMS.with(|c| {
        *c.borrow_mut() =
            Some(DataItems { remove: remove_data, save_as: save_data_as, panel: data_panel })
    });

    let tools = Submenu::new("&Tools", true);
    tools
        .append_items(&[
            &item("tools.sensitivity", "Sensitivity to Findings…"),
            &item("tools.arcStrength", "Arc Strengths…"),
            &item("tools.solveId", "Solve Influence Diagram"),
        ])
        .unwrap();

    // Experimental AI features live under one menu so they stay visibly
    // fenced; grows an item per feature phase.
    let ai = Submenu::new("A&I", true);
    ai.append_items(&[
        &item("ai.draftNetwork", "Draft Network from Text…"),
        &item("ai.enterFindings", "Enter Findings from Text…"),
        &PredefinedMenuItem::separator(),
        &item("ai.settings", "AI Settings…"),
    ])
    .unwrap();

    menu.append_items(&[&file, &edit, &network, &cases, &tools, &ai]).unwrap();

    #[cfg(not(target_os = "macos"))]
    {
        let help = Submenu::new("&Help", true);
        help.append(&item("help.about", "About Balina")).unwrap();
        menu.append(&help).unwrap();
    }

    menu
}

/// Rebuild the Open Recent submenu from `list`. Item ids are
/// `file.recent.<index>`; duplicate file names get their parent dir appended.
#[cfg(not(target_arch = "wasm32"))]
pub fn rebuild_recent_items(list: &[std::path::PathBuf]) {
    RECENT_SUBMENU.with(|c| {
        let borrowed = c.borrow();
        let Some(sub) = borrowed.as_ref() else { return };
        while sub.remove_at(0).is_some() {}
        if list.is_empty() {
            let none = MenuItem::with_id("file.recent.none", "No Recent Files", false, None);
            let _ = sub.append(&none);
            return;
        }
        let name_of = |p: &std::path::Path| {
            p.file_name().map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string())
        };
        for (i, p) in list.iter().enumerate() {
            let name = name_of(p);
            let duplicated = list.iter().filter(|o| name_of(o) == name).count() > 1;
            let label = match (duplicated, p.parent()) {
                (true, Some(dir)) => format!("{name} — {}", dir.display()),
                _ => name,
            };
            let _ = sub.append(&item(&format!("file.recent.{i}"), &label));
        }
        let _ = sub.append(&PredefinedMenuItem::separator());
        let _ = sub.append(&item("file.recent.clear", "Clear Menu"));
    });
}

/// Best-effort sync of the Auto Update check item with the document state.
#[cfg(not(target_arch = "wasm32"))]
pub fn sync_auto_update_item(on: bool) {
    AUTO_UPDATE_ITEM.with(|c| {
        if let Some(item) = c.borrow().as_ref() {
            item.set_checked(on);
        }
    });
}

/// Web has no native menu handle; the in-app menu bar reads the document
/// state reactively instead.
#[cfg(target_arch = "wasm32")]
pub fn sync_auto_update_item(_on: bool) {}

/// Sync the data items: Remove needs data on the active tab, Save Data As
/// on any tab; the panel item mirrors the panel.
#[cfg(not(target_arch = "wasm32"))]
pub fn sync_data_items(tab_has_data: bool, any_data: bool, panel_open: bool) {
    DATA_ITEMS.with(|c| {
        if let Some(items) = c.borrow().as_ref() {
            items.remove.set_enabled(tab_has_data);
            items.save_as.set_enabled(any_data);
            items.panel.set_checked(panel_open);
        }
    });
}

#[cfg(target_arch = "wasm32")]
pub fn sync_data_items(_tab_has_data: bool, _any_data: bool, _panel_open: bool) {}

/// Menu ids that stay available in a read-only view (`?ui={"ro":true}`).
///
/// An ALLOWLIST on purpose: a newly added menu item is blocked under `ro`
/// until it is deliberately listed here. Everything here either never touches
/// the document (export, analysis, view toggles, navigation) or is an evidence
/// op, which read-only deliberately keeps live.
pub fn allowed_in_read_only(id: &str) -> bool {
    matches!(
        id,
        "file.exportSvg"
            | "file.exportPng"
            | "edit.undo"
            | "edit.redo"
            | "edit.selectAll"
            | "network.compile"
            | "network.removeFindings"
            | "cases.showDataPanel"
            | "tab.next"
            | "tab.prev"
            | "tools.sensitivity"
            | "tools.arcStrength"
            | "tools.solveId"
            | "help.about"
    )
}

/// Route a menu event by id. Runs on the UI thread; long flows spawn.
pub fn route(id: &str) {
    // The single gate for every menu command: the native muda menu, the web
    // menu bar, and every hotkey that routes through here. Affordances are
    // hidden too, so a blocked id normally only arrives by key press.
    if crate::ui_config::read_only() && !allowed_in_read_only(id) {
        return;
    }
    match id {
        "file.new" => crate::chrome::file_ops::file_new(),
        "file.open" => crate::chrome::file_ops::file_open(),
        "file.save" => crate::chrome::file_ops::file_save(false),
        "file.saveAs" => crate::chrome::file_ops::file_save(true),
        "file.exportSvg" => crate::export::export_svg(),
        "file.exportPng" => crate::export::export_png(),
        #[cfg(not(target_arch = "wasm32"))]
        "file.saveDataAs" => crate::data::save_data_as_flow(),
        #[cfg(not(target_arch = "wasm32"))]
        "file.recent.clear" => crate::chrome::recent::clear(),
        #[cfg(not(target_arch = "wasm32"))]
        _ if id.starts_with("file.recent.") => {
            if let Some(path) = id
                .strip_prefix("file.recent.")
                .and_then(|i| i.parse::<usize>().ok())
                .and_then(crate::chrome::recent::get)
            {
                crate::chrome::file_ops::open_recent(path);
            }
        }
        "edit.undo" => {
            exec(|s| {
                bn_session::ops::edit::undo(s);
                Ok(())
            });
        }
        "edit.redo" => {
            exec(|s| {
                bn_session::ops::edit::redo(s);
                Ok(())
            });
        }
        "edit.selectAll" => crate::state::select_all(),
        "edit.deleteSelection" => crate::state::delete_selection(),
        "network.compile" => {
            exec(|s| {
                bn_session::ops::edit::recompute(s);
                Ok(())
            });
        }
        "network.autoUpdate" => {
            let on = !SESSION.read().doc().auto_update;
            exec(|s| {
                bn_session::ops::edit::set_auto_update(s, on);
                Ok(())
            });
            sync_auto_update_item(on);
        }
        "network.removeFindings" => {
            exec(|s| {
                bn_session::ops::evidence::retract_all_findings(s);
                Ok(())
            });
        }
        "network.rename" => open_dialog(DialogDesc::RenameNetwork),
        "cases.learnCpts" => open_dialog(DialogDesc::LearnCpts),
        "cases.learnStructure" => open_dialog(DialogDesc::StructureLearn),
        "cases.simulate" => open_dialog(DialogDesc::Simulate),
        "cases.attachData" => crate::data::attach_data_flow(),
        "cases.removeData" => crate::data::remove_data_flow(),
        "cases.showDataPanel" => crate::data::toggle_panel(),
        "tools.sensitivity" => open_dialog(DialogDesc::Sensitivity),
        "tools.arcStrength" => open_dialog(DialogDesc::ArcStrength),
        "tools.solveId" => {
            if let Some(sol) = exec(|s| bn_session::ops::tools::solve_influence_diagram(s)) {
                open_dialog(DialogDesc::IdSolution { text: sol.text });
            }
        }
        "tab.new" => {
            crate::chrome::tab_bar::add_new_tab();
        }
        "tab.close" => {
            let id = SESSION.read().active_id();
            crate::chrome::tab_bar::close_tab(id);
        }
        "tab.next" | "tab.prev" => {
            let s = SESSION.read();
            let order = s.tab_order();
            let active = s.active_id();
            if order.len() > 1 {
                let pos = order.iter().position(|&id| id == active).unwrap_or(0);
                let next = if id == "tab.next" {
                    order[(pos + 1) % order.len()]
                } else {
                    order[(pos + order.len() - 1) % order.len()]
                };
                drop(s);
                crate::chrome::tab_bar::switch_to_tab(next);
            }
        }
        "tab.duplicate" => {
            let id = SESSION.read().active_id();
            crate::chrome::tab_bar::duplicate_tab(id);
        }
        #[cfg(not(target_arch = "wasm32"))]
        "ai.settings" => open_dialog(DialogDesc::AiSettings),
        #[cfg(not(target_arch = "wasm32"))]
        "ai.draftNetwork" => open_dialog(DialogDesc::AiDraft),
        #[cfg(not(target_arch = "wasm32"))]
        "ai.enterFindings" => open_dialog(DialogDesc::AiEvidence),
        "help.about" => open_dialog(DialogDesc::About),
        _ => {
            state::log_message(format!("Unhandled menu id: {id}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::allowed_in_read_only;

    /// The read-only menu gate. This table is where a new menu item is most
    /// likely to be added, so pin both directions: everything allowed is
    /// non-mutating (or evidence), and nothing that edits the document,
    /// the files or the tabs slips through.
    #[test]
    fn read_only_menu_allowlist() {
        for id in [
            "file.exportSvg",
            "file.exportPng",
            "edit.undo",
            "edit.redo",
            "edit.selectAll",
            "network.compile",
            "network.removeFindings",
            "cases.showDataPanel",
            "tab.next",
            "tab.prev",
            "tools.sensitivity",
            "tools.arcStrength",
            "tools.solveId",
            "help.about",
        ] {
            assert!(allowed_in_read_only(id), "{id} should survive read-only");
        }

        for id in [
            "file.new",
            "file.open",
            "file.save",
            "file.saveAs",
            "file.saveDataAs",
            "file.recent.0",
            "file.recent.clear",
            "edit.deleteSelection",
            "network.autoUpdate",
            "network.rename",
            "cases.learnCpts",
            "cases.learnStructure",
            "cases.simulate",
            "cases.attachData",
            "cases.removeData",
            "tab.new",
            "tab.close",
            "tab.duplicate",
            "ai.settings",
            "ai.draftNetwork",
            "ai.enterFindings",
        ] {
            assert!(!allowed_in_read_only(id), "{id} must be blocked in read-only");
        }
    }
}
