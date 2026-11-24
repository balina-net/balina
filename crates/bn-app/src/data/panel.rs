//! Right-docked data panel: the active tab's case data as a read-only,
//! virtualized table. Only the rows in view (plus a margin) are read,
//! through the body's sparse row index, so memory stays flat however large
//! the data is. Lives outside the dialog slot: Escape and other dialogs
//! leave it alone, and it follows tab switches.

use std::collections::HashSet;
use std::rc::Rc;

use bn_session::ops::data as data_ops;
use bn_session::TabId;
use dioxus::html::geometry::PixelsVector2D;
use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;

use crate::platform;
use crate::state::{
    log_message, DataJobKind, DATA_ERRORS, DATA_JOBS, DATA_PANEL_WIDTH, SESSION, SHOW_DATA_PANEL,
};
use crate::ui::{NumberInput, BTN_SM};

const ROW_H: f64 = 24.0;
/// Rows read per fetch, starting a little above the visible range.
const WINDOW: u64 = 200;
/// Browsers cap element heights (Firefox ≈ 17.9M px); beyond this the
/// scroll range is compressed and rows are positioned proportionally.
const MAX_SCROLL_PX: f64 = 10_000_000.0;
const MIN_WIDTH: f64 = 280.0;
const MAX_WIDTH_FRAC: f64 = 0.7;
const COL_W: &str = "8rem";
const NUM_COL_W: &str = "4.5rem";

/// Rows read for one body, starting at row `start`.
#[derive(Clone, PartialEq)]
struct RowCache {
    body: u64,
    start: u64,
    rows: Vec<Vec<String>>,
}

#[component]
pub fn DataPanel() -> Element {
    if !*SHOW_DATA_PANEL.read() {
        return rsx! {};
    }
    rsx! { PanelFrame {} }
}

/// First data row in view and one past the last, for `rows` rows.
fn visible_range(rows: u64, scroll_top: f64, view_h: f64) -> (u64, u64) {
    let scale = (rows as f64 * ROW_H / MAX_SCROLL_PX).max(1.0);
    let first = ((scroll_top * scale / ROW_H).floor().max(0.0) as u64).min(rows);
    let count = (view_h / ROW_H).ceil() as u64 + 2;
    (first, (first + count).min(rows))
}

#[component]
fn PanelFrame() -> Element {
    let mut scroll_top = use_signal(|| 0.0f64);
    let mut view_h = use_signal(|| 600.0f64);
    let mut cache: Signal<Option<RowCache>> = use_signal(|| None);
    // The read in flight, as (body, start): scroll events must not pile up
    // duplicate reads of the same window.
    let mut pending: Signal<Option<(u64, u64)>> = use_signal(|| None);
    let mut scroller: Signal<Option<Rc<MountedData>>> = use_signal(|| None);
    // In-flight resize: (client x at mousedown, width then).
    let mut resize: Signal<Option<(f64, f64)>> = use_signal(|| None);
    let mut goto = use_signal(String::new);
    let mut shown_tab: Signal<Option<TabId>> = use_signal(|| None);

    let (tab, tab_name, data, node_names) = {
        let s = SESSION.read();
        let id = s.active_id();
        let names: HashSet<String> =
            s.doc().net.nodes().map(|(_, n)| n.name.to_lowercase()).collect();
        (id, s.tab_label(id).to_string(), s.active_tab().data.clone(), names)
    };

    // Each tab's data starts at the top.
    use_effect(move || {
        let id = SESSION.read().active_id();
        if *shown_tab.peek() == Some(id) {
            return;
        }
        let first_show = shown_tab.peek().is_none();
        shown_tab.set(Some(id));
        if first_show {
            return;
        }
        scroll_top.set(0.0);
        goto.set(String::new());
        if let Some(el) = scroller.peek().clone() {
            spawn(async move {
                let _ = el.scroll(PixelsVector2D::new(0.0, 0.0), ScrollBehavior::Instant).await;
            });
        }
    });

    // Data read in place arrives without an index: build it once.
    use_effect(move || {
        let (id, d) = {
            let s = SESSION.read();
            (s.active_id(), s.active_tab().data.clone())
        };
        if let Some(d) = d
            && d.index.is_none()
        {
            crate::data::ensure_index(d, id);
        }
    });

    // Read the rows in view when they are not cached yet.
    use_effect(move || {
        let (st, vh) = (*scroll_top.read(), *view_h.read());
        let Some(d) = SESSION.read().active_tab().data.clone() else { return };
        let Some(index) = d.index.clone() else { return };
        let (first, last) = visible_range(index.rows, st, vh);
        if first >= last {
            return;
        }
        let start = first.saturating_sub(WINDOW / 4);
        let body = d.body_id();
        if crate::data::data_error(body).is_some() {
            return;
        }
        if let Some(c) = cache.read().as_ref()
            && c.body == body
            && ((c.start <= first && c.start + c.rows.len() as u64 >= last) || c.start == start)
        {
            return;
        }
        if *pending.peek() == Some((body, start)) {
            return;
        }
        pending.set(Some((body, start)));
        spawn(async move {
            let res = platform::run_blocking(move || data_ops::read_rows(&d, start, WINDOW as usize)).await;
            if *pending.peek() == Some((body, start)) {
                pending.set(None);
            }
            match res {
                Ok(Ok(rows)) => cache.set(Some(RowCache { body, start, rows })),
                Ok(Err(e)) => crate::data::report_data_error(body, e),
                Err(e) => {
                    log_message(format!("Reading the data crashed: {e}"));
                    DATA_ERRORS.write().insert(body, e);
                }
            }
        });
    });

    let measure = move || {
        let Some(el) = scroller.peek().clone() else { return };
        spawn(async move {
            if let Ok(rect) = el.get_client_rect().await {
                view_h.set(rect.height());
            }
        });
    };

    let width = *DATA_PANEL_WIDTH.read();
    let jobs = DATA_JOBS.read().clone();
    let preparing = jobs.iter().find(|j| {
        (j.kind == DataJobKind::Prepare && j.tab == Some(tab)) || j.kind == DataJobKind::Extract
    });
    let progress_line = preparing.map(|j| {
        let what = if j.kind == DataJobKind::Extract { "Loading the pack's data" } else { "Preparing data" };
        match j.frac {
            Some(f) => format!("{what}… {:.0}%", f * 100.0),
            None => format!("{what}…"),
        }
    });

    let read_error = data.as_ref().and_then(|d| DATA_ERRORS.read().get(&d.body_id()).cloned());
    let body = match &data {
        Some(_) if read_error.is_some() => rsx! {
            div { class: "flex flex-1 flex-col items-center justify-center gap-2 p-4 text-center text-sm",
                p { class: "font-medium text-red-700", "This data cannot be read." }
                p { class: "text-xs text-muted-foreground", "{read_error.clone().unwrap_or_default()}" }
            }
        },
        None => rsx! {
            div { class: "flex flex-1 flex-col items-center justify-center gap-3 p-4 text-center text-sm text-muted-foreground",
                p { "No data attached to this tab." }
                button { class: BTN_SM, onclick: move |_| crate::data::attach_data_flow(), "Attach data…" }
            }
        },
        Some(d) if d.index.is_none() => rsx! {
            div { class: "flex flex-1 items-center justify-center p-4 text-sm text-muted-foreground",
                "Indexing {d.rows} rows…"
            }
        },
        Some(d) => {
            let index = d.index.clone().unwrap();
            let rows = index.rows;
            let scale = (rows as f64 * ROW_H / MAX_SCROLL_PX).max(1.0);
            let (first, last) = visible_range(rows, *scroll_top.read(), *view_h.read());
            let top_pad = first as f64 * ROW_H / scale;
            let bottom_pad = (rows - last) as f64 * ROW_H / scale;
            let cached = cache.read().clone().filter(|c| c.body == d.body_id());
            let headers: Vec<(String, bool)> = d
                .columns
                .iter()
                .map(|c| (c.clone(), node_names.contains(&c.to_lowercase())))
                .collect();
            let n_cols = headers.len();
            let table_w = format!("calc({NUM_COL_W} + {n_cols} * {COL_W})");
            rsx! {
                div {
                    class: "min-h-0 flex-1 overflow-auto",
                    onmounted: move |ev| {
                        scroller.set(Some(ev.data()));
                        measure();
                    },
                    onresize: move |_| measure(),
                    onscroll: move |ev| scroll_top.set(ev.data().scroll_top()),
                    table {
                        class: "table-fixed border-collapse text-xs",
                        style: "width: {table_w};",
                        colgroup {
                            col { style: "width: {NUM_COL_W};" }
                            for _ in 0..n_cols {
                                col { style: "width: {COL_W};" }
                            }
                        }
                        thead { class: "sticky top-0 z-10 bg-muted",
                            tr { style: "height: {ROW_H}px;",
                                th { class: "px-2 text-right font-normal text-muted-foreground", "#" }
                                for (c, matched) in headers.iter() {
                                    if *matched {
                                        th { class: "truncate px-2 text-left font-semibold", title: "{c}", "{c}" }
                                    } else {
                                        th {
                                            class: "truncate px-2 text-left font-normal text-muted-foreground/70",
                                            title: "{c} (matches no node)",
                                            "{c}"
                                        }
                                    }
                                }
                            }
                        }
                        tbody {
                            tr { style: "height: {top_pad}px;" }
                            for r in first..last {
                                {
                                    let cells: Vec<String> = cached
                                        .as_ref()
                                        .and_then(|c| r.checked_sub(c.start).and_then(|i| c.rows.get(i as usize)))
                                        .cloned()
                                        .unwrap_or_default();
                                    // Striped by row number: DOM parity shifts as
                                    // the window scrolls.
                                    let stripe = if r % 2 == 1 { "border-t bg-muted/30" } else { "border-t" };
                                    rsx! {
                                        tr { key: "{r}", class: stripe, style: "height: {ROW_H}px;",
                                            td { class: "px-2 text-right tabular-nums text-muted-foreground", "{r + 1}" }
                                            for (i, v) in cells.iter().enumerate() {
                                                td { key: "{i}", class: "truncate px-2 font-mono", title: "{v}", "{v}" }
                                            }
                                        }
                                    }
                                }
                            }
                            tr { style: "height: {bottom_pad}px;" }
                        }
                    }
                }
            }
        }
    };

    let footer = data.as_ref().map(|d| {
        let mut parts = vec![format!("{} rows × {} cols", d.rows, d.cols)];
        for v in [&d.meta.origin, &d.meta.source] {
            if !v.is_empty() {
                parts.push(v.clone());
            }
        }
        if d.meta.created.len() >= 10 {
            parts.push(d.meta.created[..10].to_string());
        }
        parts.join(" · ")
    });
    let rows_total = data.as_ref().and_then(|d| d.index.as_ref()).map(|i| i.rows).unwrap_or(0);

    let go_to = move |v: String| {
        goto.set(v.clone());
        let Ok(row) = v.trim().parse::<u64>() else { return };
        if rows_total == 0 {
            return;
        }
        let row = row.clamp(1, rows_total);
        let scale = (rows_total as f64 * ROW_H / MAX_SCROLL_PX).max(1.0);
        let y = (row - 1) as f64 * ROW_H / scale;
        let Some(el) = scroller.peek().clone() else { return };
        spawn(async move {
            let x = el.get_scroll_offset().await.map(|o| o.x).unwrap_or(0.0);
            let _ = el.scroll(PixelsVector2D::new(x, y), ScrollBehavior::Instant).await;
        });
    };

    rsx! {
        div {
            class: "relative flex shrink-0 flex-col border-l bg-background",
            style: "width: {width}px;",
            // Resize handle on the left edge.
            div {
                class: "absolute inset-y-0 -left-1 z-20 w-2 cursor-col-resize hover:bg-ring/30",
                onmousedown: move |ev| {
                    if ev.data().trigger_button() != Some(MouseButton::Primary) {
                        return;
                    }
                    ev.prevent_default();
                    let c = ev.data().client_coordinates();
                    resize.set(Some((c.x, *DATA_PANEL_WIDTH.peek())));
                },
            }
            div { class: "flex h-8 shrink-0 items-center justify-between gap-2 border-b px-2",
                span { class: "truncate text-sm font-semibold", "Data — {tab_name}" }
                button {
                    class: "rounded-xs opacity-70 hover:opacity-100",
                    title: "Close the data panel",
                    onclick: move |_| *SHOW_DATA_PANEL.write() = false,
                    "✕"
                }
            }
            if let Some(line) = progress_line {
                div { class: "shrink-0 border-b bg-muted/40 px-2 py-1 text-xs text-muted-foreground", "{line}" }
            }
            {body}
            if let Some(f) = footer {
                div { class: "flex shrink-0 items-center gap-2 border-t px-2 py-1 text-xs",
                    span { class: "min-w-0 flex-1 truncate text-muted-foreground", title: "{f}", "{f}" }
                    if rows_total > 0 {
                        label { class: "shrink-0 text-muted-foreground", "Go to row" }
                        NumberInput {
                            class: "h-6 w-20".to_string(),
                            min: "1".to_string(),
                            max: rows_total.to_string(),
                            value: goto.read().clone(),
                            onchange: go_to,
                        }
                    }
                }
            }
        }
        // Capture layer while resizing (a sibling, like ui::DragCapture, so
        // fast drags keep delivering moves).
        if resize.read().is_some() {
            div {
                class: "fixed inset-0 z-[70] cursor-col-resize",
                onmousemove: move |ev| {
                    let Some((x0, w0)) = *resize.peek() else { return };
                    if ev.data().held_buttons().is_empty() {
                        resize.set(None);
                        return;
                    }
                    // The panel is docked right: the window ends at x0 + w0.
                    let max = (x0 + w0) * MAX_WIDTH_FRAC;
                    let w = (w0 + x0 - ev.data().client_coordinates().x).clamp(MIN_WIDTH, max.max(MIN_WIDTH));
                    *DATA_PANEL_WIDTH.write() = w;
                },
                onmouseup: move |_| resize.set(None),
            }
        }
    }
}
