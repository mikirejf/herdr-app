//! Herdr's own keys while a page has the keyboard. A native page view sits
//! before GPUI in AppKit's responder chain, so keys.json never sees a key
//! pressed in a page; an `NSEvent` local monitor (view.rs) asks `find` first
//! and runs the bound action instead. Pure: no AppKit here.

use gpui::Keystroke;

/// A key press as AppKit reports it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Press {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub cmd: bool,
    /// The key without modifiers, lowercase (`]`, `t`), or GPUI's name for
    /// a named key (`escape`, `left`, `tab`, `f2`).
    pub key: String,
    /// What the key types with shift (`}` for shift-]), for bindings written
    /// with the shifted character (`cmd-+`).
    pub shifted: Option<String>,
}

/// The action bound to `press`, if any. The escape hatch comes first,
/// whatever else is bound to the same keys (as GPUI ranks it). `bindings`:
/// `page_bindings`. Sequences (`cmd-k cmd-s`) are skipped: a page keeps
/// keys one at a time. A `browser` key bound to `terminal` is the page's.
pub fn find<'a>(bindings: &'a [(String, String)], press: &Press) -> Option<&'a str> {
    if matches(crate::keys::ESCAPE_HATCH, press) {
        return Some(HATCH_ACTION);
    }
    bindings.iter().find(|(keys, _)| matches(keys, press)).map(|(_, action)| action.as_str()).filter(|a| *a != "terminal")
}

const HATCH_ACTION: &str = "app.reset";

/// The keys Herdr keeps while a page has the keyboard, as (keys, action),
/// from `crate::keys::listing()` of a keys.json that loaded: the `browser`
/// section first (it applies only here: `cmd-=` zooms the page, not
/// Herdr), then the shown layout's (GPUI ranks it above `global`), then
/// `global`. Left to the page: `send:` (it types into a terminal, and a page
/// isn't one) and `terminal`, which in `browser` also hides the same keys'
/// layout and global bindings from the page (`find`). The escape hatch is
/// `find`'s own.
pub fn page_bindings(listing: &[(String, String, String)], layout: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for section in ["browser".to_owned(), format!("layout:{layout}"), "global".to_owned()] {
        for (_, keys, action) in listing.iter().filter(|(s, _, _)| *s == section) {
            let passes = action == "terminal" && section == "browser";
            if keys != crate::keys::ESCAPE_HATCH && (passes || action != "terminal") && !action.starts_with("send:") {
                out.push((keys.clone(), action.clone()));
            }
        }
    }
    out
}

/// An action macOS does itself (Herdr's command for it does nothing): the
/// key monitor calls AppKit for these instead of running the command.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Native {
    Hide,
    HideOthers,
    ShowAll,
}

pub fn native(action: &str) -> Option<Native> {
    match action {
        "app.hide" => Some(Native::Hide),
        "app.hide_others" => Some(Native::HideOthers),
        "app.show_all" => Some(Native::ShowAll),
        _ => None,
    }
}

fn matches(keys: &str, press: &Press) -> bool {
    if keys.contains(' ') {
        return false;
    }
    let Ok(k) = Keystroke::parse(keys) else { return false };
    let m = k.modifiers;
    if m.function || m.control != press.ctrl || m.alt != press.alt || m.platform != press.cmd {
        return false;
    }
    let key = k.key.to_lowercase();
    if m.shift == press.shift && key == press.key {
        return true;
    }
    // `cmd-+` is pressed as cmd-shift-=: shift made another character (not
    // just a capital letter, which would make cmd-shift-t match cmd-t).
    let shifted = press.shifted.as_deref().map(str::to_lowercase).filter(|s| *s != press.key);
    !m.shift && press.shift && shifted.is_some_and(|s| s == key)
}

/// GPUI's name for a key AppKit reports by its virtual key code, for keys
/// that type no character worth matching.
pub fn named(key_code: u16) -> Option<&'static str> {
    Some(match key_code {
        36 | 76 => "enter",
        48 => "tab",
        49 => "space",
        51 => "backspace",
        53 => "escape",
        115 => "home",
        116 => "pageup",
        117 => "delete",
        119 => "end",
        121 => "pagedown",
        123 => "left",
        124 => "right",
        125 => "down",
        126 => "up",
        122 => "f1",
        120 => "f2",
        99 => "f3",
        118 => "f4",
        96 => "f5",
        97 => "f6",
        98 => "f7",
        100 => "f8",
        101 => "f9",
        109 => "f10",
        103 => "f11",
        111 => "f12",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bindings() -> Vec<(String, String)> {
        [
            ("cmd-t", "tab.new"),
            ("cmd-shift-]", "tab.next"),
            ("cmd-alt-left", "pane.go:left"),
            ("ctrl-1", "space.select:1"),
            ("cmd-+", "app.zoom_in"),
            ("f2", "tab.rename"),
            ("cmd-k cmd-s", "app.help"),
            ("ctrl-alt-shift-escape", "app.reset"),
        ]
        .into_iter()
        .map(|(k, a)| (k.to_owned(), a.to_owned()))
        .collect()
    }

    fn press(mods: &str, key: &str, shifted: Option<&str>) -> Press {
        Press {
            ctrl: mods.contains('c'),
            alt: mods.contains('a'),
            shift: mods.contains('s'),
            cmd: mods.contains('m'),
            key: key.into(),
            shifted: shifted.map(Into::into),
        }
    }

    #[test]
    fn bound_global_keys_are_found_with_their_exact_modifiers() {
        let b = bindings();
        assert_eq!(find(&b, &press("m", "t", Some("t"))), Some("tab.new"));
        assert_eq!(find(&b, &press("ms", "]", Some("}"))), Some("tab.next"));
        assert_eq!(find(&b, &press("ma", "left", None)), Some("pane.go:left"));
        assert_eq!(find(&b, &press("c", "1", Some("1"))), Some("space.select:1"));
        assert_eq!(find(&b, &press("", "f2", None)), Some("tab.rename"));
        // The escape hatch works with a page focused.
        assert_eq!(find(&b, &press("cas", "escape", None)), Some("app.reset"));
    }

    #[test]
    fn a_shifted_character_binding_matches_its_shifted_press() {
        assert_eq!(find(&bindings(), &press("ms", "=", Some("+"))), Some("app.zoom_in"));
    }

    #[test]
    fn page_keys_stay_with_the_page() {
        let b = bindings();
        // Typing, copy and paste, and near misses go to the page.
        assert_eq!(find(&b, &press("", "t", Some("t"))), None);
        assert_eq!(find(&b, &press("m", "c", Some("c"))), None);
        assert_eq!(find(&b, &press("ms", "t", Some("T"))), None);
        assert_eq!(find(&b, &press("m", "]", Some("]"))), None);
        assert_eq!(find(&b, &press("cs", "escape", None)), None);
        // Sequences are never matched from a page.
        assert_eq!(find(&b, &press("m", "k", Some("k"))), None);
    }

    #[test]
    fn the_escape_hatch_wins_over_any_binding_on_the_same_keys() {
        // keys.json can't bind the hatch's exact text, but a reordered one
        // parses and lands earlier in the list.
        let b: Vec<(String, String)> = [("shift-ctrl-alt-escape", "tab.new"), ("alt-ctrl-shift-escape", "tab.close")]
            .into_iter()
            .map(|(k, a)| (k.to_owned(), a.to_owned()))
            .collect();
        assert_eq!(find(&b, &press("cas", "escape", None)), Some("app.reset"));
        assert_eq!(find(&[], &press("cas", "escape", None)), Some("app.reset"));
    }

    #[test]
    fn a_page_keeps_terminal_and_send_keys_and_the_layout_ranks_first() {
        let listing: Vec<(String, String, String)> = [
            ("global", "cmd-t", "tab.new"),
            ("global", "cmd-left", "send:\u{1}"),
            ("global", "cmd-c", "terminal"),
            ("layout:main", "cmd-t", "app.palette"),
            ("layout:other", "cmd-j", "tab.next"),
            ("terminal", "cmd-v", "terminal.paste"),
            ("global", "ctrl-alt-shift-escape", "app.reset"),
        ]
        .into_iter()
        .map(|(s, k, a)| (s.to_owned(), k.to_owned(), a.to_owned()))
        .collect();
        let b = page_bindings(&listing, "main");
        assert_eq!(b, vec![("cmd-t".to_owned(), "app.palette".to_owned()), ("cmd-t".to_owned(), "tab.new".to_owned())]);
        assert_eq!(find(&b, &press("m", "t", Some("t"))), Some("app.palette"));
        // The page gets cmd-left (send:) and cmd-c (terminal).
        assert_eq!(find(&b, &press("m", "left", None)), None);
        assert_eq!(find(&b, &press("m", "c", Some("c"))), None);
    }

    #[test]
    fn the_browser_section_ranks_first_and_can_give_a_key_to_the_page() {
        let listing: Vec<(String, String, String)> = [
            ("global", "cmd-=", "app.zoom_in"),
            ("global", "cmd-0", "app.zoom_reset"),
            ("global", "cmd-1", "tab.select:1"),
            ("global", "cmd-2", "tab.select:2"),
            ("layout:main", "cmd-0", "app.palette"),
            ("browser", "cmd-=", "browser.zoom:in"),
            ("browser", "cmd-0", "browser.zoom:reset"),
            ("browser", "cmd-2", "terminal"),
            ("browser", "cmd-left", "send:\u{1}"),
        ]
        .into_iter()
        .map(|(s, k, a)| (s.to_owned(), k.to_owned(), a.to_owned()))
        .collect();
        let b = page_bindings(&listing, "main");
        // A page's cmd-= and cmd-0 zoom the page, above the layout and global.
        assert_eq!(find(&b, &press("m", "=", Some("+"))), Some("browser.zoom:in"));
        assert_eq!(find(&b, &press("m", "0", Some(")"))), Some("browser.zoom:reset"));
        // Tab switching stays global; `terminal` gives cmd-2 to the page.
        assert_eq!(find(&b, &press("m", "1", Some("!"))), Some("tab.select:1"));
        assert_eq!(find(&b, &press("m", "2", Some("@"))), None);
        assert_eq!(find(&b, &press("m", "left", None)), None);
        // `terminal` outside `browser` still just leaves the key to the page.
        let global_only = page_bindings(&listing[..4], "main");
        assert_eq!(find(&global_only, &press("m", "0", Some(")"))), Some("app.zoom_reset"));
    }

    #[test]
    fn the_shipped_mac_keys_zoom_a_page_and_keep_tab_switching() {
        let sections: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>> =
            serde_json::from_str(include_str!("../../defaults/keys.macos.json")).unwrap();
        let listing: Vec<(String, String, String)> =
            sections.into_iter().flat_map(|(s, keys)| keys.into_iter().map(move |(k, a)| (s.clone(), k, a))).collect();
        let b = page_bindings(&listing, "main");
        assert_eq!(find(&b, &press("m", "=", Some("+"))), Some("browser.zoom:in"));
        assert_eq!(find(&b, &press("ms", "=", Some("+"))), Some("browser.zoom:in"));
        assert_eq!(find(&b, &press("m", "-", Some("_"))), Some("browser.zoom:out"));
        assert_eq!(find(&b, &press("m", "0", Some(")"))), Some("browser.zoom:reset"));
        for n in 1..=9 {
            let key = n.to_string();
            assert_eq!(find(&b, &press("m", &key, None)), Some(format!("tab.select:{n}").as_str()));
        }
    }

    #[test]
    fn hiding_the_app_is_macoss_own() {
        assert_eq!(native("app.hide"), Some(Native::Hide));
        assert_eq!(native("app.hide_others"), Some(Native::HideOthers));
        assert_eq!(native("app.show_all"), Some(Native::ShowAll));
        assert_eq!(native("app.quit"), None);
        assert_eq!(native("tab.new"), None);
    }

    #[test]
    fn named_keys_use_gpuis_names() {
        assert_eq!(named(53), Some("escape"));
        assert_eq!(named(123), Some("left"));
        assert_eq!(named(120), Some("f2"));
        assert_eq!(named(0), None);
    }
}
