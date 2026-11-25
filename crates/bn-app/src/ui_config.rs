//! Startup view configuration from the page URL (`?ui={json}`, web only).
//!
//! Three independent concerns: which parts of the chrome are visible,
//! whether the document is read-only, and whether the view can be navigated.
//! Hiding is cosmetic; `ro` is the capability gate (see
//! `chrome::menu::allowed_in_read_only`, `state::open_dialog` and
//! `canvas::controller::arm`) and `scroll` the navigation gate (see
//! `Gesture::navigates`, `controller::zoom_center`/`zoom_reset` and the
//! canvas wheel handler).
//!
//! Process-constant: read once on first access, never written again, so it is
//! a plain `Copy` struct behind a `OnceLock` rather than a signal — components
//! branch on it ABOVE their hooks, which is only sound when the predicate
//! cannot change between renders. Most consumers are free functions outside
//! any reactive scope anyway. Mirrors `crate::INITIAL_FILE`.

/// What the page URL asked for. `Default` is today's behavior: everything
/// visible, fully editable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UiConfig {
    /// The in-app menu bar (`chrome::menu_bar`; web only).
    pub menu: bool,
    /// The toolbar (`chrome::toolbar`).
    pub bar: bool,
    /// The tab bar (`chrome::tab_bar`).
    pub tabs: bool,
    /// The status bar and the message log under it.
    pub footer: bool,
    /// The canvas minimap.
    pub map: bool,
    /// Read-only: entering findings still works, nothing else mutates.
    pub ro: bool,
    /// Pan and zoom. `false` pins the view where fit-view left it: the wheel,
    /// the middle-drag pan, the zoom hotkeys/buttons and the minimap all stop
    /// moving it.
    pub scroll: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self { menu: true, bar: true, tabs: true, footer: true, map: true, ro: false, scroll: true }
    }
}

static UI: std::sync::OnceLock<UiConfig> = std::sync::OnceLock::new();

/// The view configuration for this run.
pub fn get() -> UiConfig {
    *UI.get_or_init(read_config)
}

/// Shorthand for the most-used predicate.
pub fn read_only() -> bool {
    get().ro
}

/// True when the view is pinned: no pan, no zoom.
pub fn no_scroll() -> bool {
    !get().scroll
}

#[cfg(target_arch = "wasm32")]
fn read_config() -> UiConfig {
    crate::platform::ui_config_param().map(|s| parse(&s)).unwrap_or_default()
}

/// Desktop has no query string.
#[cfg(not(target_arch = "wasm32"))]
fn read_config() -> UiConfig {
    UiConfig::default()
}

/// Parse the `ui` query value.
///
/// Deliberately lenient and fail-open: malformed JSON, a non-object, unknown
/// keys, or a key of the wrong type all leave the corresponding default in
/// place. The URL is author-controlled, so `ro` is a presentation mode and
/// never a security boundary — degrading to the normal app is the right
/// failure for an embed with a typo.
///
/// Only the wasm build reaches this (plus the tests below); on desktop it is
/// compiled but unreachable.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub fn parse(json: &str) -> UiConfig {
    let mut cfg = UiConfig::default();
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(json)
    else {
        return cfg;
    };
    flag(&map, "menu", &mut cfg.menu);
    flag(&map, "bar", &mut cfg.bar);
    flag(&map, "tabs", &mut cfg.tabs);
    flag(&map, "footer", &mut cfg.footer);
    flag(&map, "map", &mut cfg.map);
    flag(&map, "ro", &mut cfg.ro);
    flag(&map, "scroll", &mut cfg.scroll);
    cfg
}

/// Read one boolean key, leaving `cur` alone unless the value reads as a
/// boolean. Hand-written embed URLs often quote everything, so `"true"`,
/// `"on"`, `"yes"` and `1` are accepted alongside `true` (and their
/// negatives alongside `false`).
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn flag(map: &serde_json::Map<String, serde_json::Value>, key: &str, cur: &mut bool) {
    match map.get(key) {
        Some(serde_json::Value::Bool(b)) => *cur = *b,
        Some(serde_json::Value::Number(n)) => {
            if let Some(f) = n.as_f64() {
                *cur = f != 0.0;
            }
        }
        Some(serde_json::Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => *cur = true,
            "false" | "0" | "no" | "off" => *cur = false,
            _ => {}
        },
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shows_everything_and_is_editable() {
        let d = UiConfig::default();
        assert!(d.menu && d.bar && d.tabs && d.footer && d.map && d.scroll);
        assert!(!d.ro);
    }

    #[test]
    fn full_example_parses() {
        let c = parse(
            r#"{"menu":false,"bar":false,"tabs":false,"footer":false,"map":false,"ro":true,"scroll":false}"#,
        );
        assert_eq!(
            c,
            UiConfig {
                menu: false,
                bar: false,
                tabs: false,
                footer: false,
                map: false,
                ro: true,
                scroll: false,
            }
        );
    }

    #[test]
    fn scroll_is_independent_of_read_only() {
        assert_eq!(parse(r#"{"scroll":false}"#), UiConfig { scroll: false, ..Default::default() });
        assert!(parse(r#"{"ro":true}"#).scroll);
        assert!(!parse(r#"{"scroll":"off"}"#).scroll);
        assert!(!parse(r#"{"scroll":"no"}"#).scroll);
        assert!(!parse(r#"{"scroll":0}"#).scroll);
    }

    #[test]
    fn partial_object_keeps_defaults() {
        let c = parse(r#"{"ro":true}"#);
        assert_eq!(c, UiConfig { ro: true, ..Default::default() });
    }

    #[test]
    fn unknown_keys_are_ignored() {
        assert_eq!(parse(r#"{"ro":true,"wat":1,"nested":{"a":2}}"#).ro, true);
        assert_eq!(parse(r#"{"wat":1}"#), UiConfig::default());
    }

    #[test]
    fn malformed_input_falls_back_to_default() {
        for bad in ["garbage", "", "[]", "null", "{", "\"ro\"", "123", "true"] {
            assert_eq!(parse(bad), UiConfig::default(), "input: {bad:?}");
        }
    }

    #[test]
    fn wrong_type_keeps_that_key_default() {
        // `menu` stays true; the valid sibling still applies.
        let c = parse(r#"{"menu":"maybe","ro":true}"#);
        assert!(c.menu);
        assert!(c.ro);
        assert_eq!(parse(r#"{"menu":null}"#).menu, true);
        assert_eq!(parse(r#"{"menu":[]}"#).menu, true);
    }

    #[test]
    fn string_and_number_coercion() {
        assert!(parse(r#"{"ro":"true"}"#).ro);
        assert!(parse(r#"{"ro":"TRUE"}"#).ro);
        assert!(parse(r#"{"ro":1}"#).ro);
        assert!(!parse(r#"{"menu":"false"}"#).menu);
        assert!(!parse(r#"{"menu":0}"#).menu);
        assert!(!parse(r#"{"menu":"no"}"#).menu);
        assert!(!parse(r#"{"menu":"off"}"#).menu);
        assert!(parse(r#"{"menu":"on"}"#).menu);
    }

    #[test]
    fn whitespace_tolerated() {
        assert!(parse("  {\"ro\" : true}  ").ro);
        assert!(parse(r#"{"ro":"  true  "}"#).ro);
    }
}
