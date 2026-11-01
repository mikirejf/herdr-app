//! Browser panes: a web page in a block (`{ "mod": "browser-prototype", "props": { "browser": "b1", "url": … } }`),
//! as a pane in a tab or a block in the shown layout.
//!
//! macOS builds with the `browser` feature show each page as a native child
//! view over GPUI's Metal view, kept on its block's bounds every frame
//! (native.rs, view.rs). The engine is CEF in windowed mode (runtime.rs, `engine`),
//! Chromium shipped in the bundle; the rest is the registry, key routing,
//! overlay cover, the agents' token proxy and allowlist.
//! Every other build has the same API with no browser behind it: blocks show
//! a placeholder and the commands say what's missing (off.rs). The window
//! only talks to `Browsers`.
//!
//! What the shared code calls on `engine`:
//! `set_events`, `start`, `open`, `close`, `navigate`, `history`, `reload`,
//! `devtools`, `zoom`, `set_zoom`, `set_viewport`, `page_view`, `fit`,
//! `set_focus`, `pressed`, `agent_event`, `clicked_herdr`, `covers_changed`, `wake`,
//! `send_devtools` (an allowed CDP message from proxy.rs) and `agent_gone`.
//! Page events and answers go back through `proxy::from_page`.

#[cfg(any(test, all(target_os = "macos", feature = "browser")))]
pub mod agent;
#[cfg(any(test, all(target_os = "macos", feature = "browser")))]
pub mod emulate;
#[cfg(any(test, all(target_os = "macos", feature = "browser")))]
pub mod keys;
#[cfg(any(test, all(target_os = "macos", feature = "browser")))]
pub mod registry;

#[cfg(all(target_os = "macos", feature = "browser"))]
mod native;
#[cfg(all(target_os = "macos", feature = "browser"))]
mod proxy;
#[cfg(all(target_os = "macos", feature = "browser"))]
mod runtime;
#[cfg(all(target_os = "macos", feature = "browser"))]
use runtime as engine;
#[cfg(all(target_os = "macos", feature = "browser"))]
mod view;
#[cfg(all(target_os = "macos", feature = "browser"))]
pub use native::{Browsers, Event};

#[cfg(not(all(target_os = "macos", feature = "browser")))]
mod off;
#[cfg(not(all(target_os = "macos", feature = "browser")))]
pub use off::{Browsers, Event};

/// What the window does after a browser reported something.
#[derive(Debug, Default)]
pub struct Update {
    pub browser: String,
    /// The page moved here: the block's saved url follows.
    pub url: Option<String>,
    /// The page took the keyboard (the person clicked it): its pane is the focused one now.
    pub focused: bool,
    /// The page asked to close (`window.close()`): its pane goes.
    pub close: bool,
    /// The person pressed the mouse on Herdr, outside every page.
    pub clicked_herdr: bool,
    /// A page hid under an overlay or came back: who has the keyboard may change.
    pub covers: bool,
}

/// The theme colors a browser block's placeholder uses.
#[derive(Clone, Copy)]
pub struct Look {
    pub surface: gpui::Hsla,
    pub text: gpui::Hsla,
    pub dim: gpui::Hsla,
}

/// Which page has the keyboard: none while a dialog or popup is open (it
/// takes typing) or while a Herdr control the person pressed has GPUI focus
/// (a layout mod's input, a floating terminal); else the layout block's page
/// the person clicked into, while it shows; else the focused pane's page,
/// while its tab shows. A page hidden under an overlay (`covered`) can't
/// have it: Herdr holds it until the overlay goes, and the page gets it back
/// only if it still wins then. `None`: Herdr (GPUI) has it.
pub fn keyboard_page<'a>(dialog: bool, control: bool, layout_page: Option<&'a str>, pane_page: Option<&'a str>, covered: &[String]) -> Option<&'a str> {
    if dialog || control {
        return None;
    }
    layout_page.or(pane_page).filter(|p| !covered.iter().any(|c| c == p))
}

#[cfg(any(test, all(target_os = "macos", feature = "browser")))]
/// A rectangle in window points, top-left origin (as GPUI paints).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[cfg(any(test, all(target_os = "macos", feature = "browser")))]
impl Rect {
    /// Shares some area with `other`: edges that only touch, and an empty
    /// rectangle (an overlay not measured yet), don't count.
    pub fn overlaps(&self, other: &Rect) -> bool {
        self.w > 0.0
            && self.h > 0.0
            && other.w > 0.0
            && other.h > 0.0
            && self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }
}

#[cfg(any(test, all(target_os = "macos", feature = "browser")))]
/// Which overlay each page hides under: GPUI can't draw over a page's native
/// view, so a page an overlay overlaps hides while it's there and its block
/// shows a placeholder. `covers` are the overlays painted this frame (label,
/// bounds), in paint order; a page names the first that overlaps it. None:
/// the page shows.
pub fn covered<'a>(pages: &[(&'a str, Rect)], covers: &'a [(String, Rect)]) -> Vec<(&'a str, Option<&'a str>)> {
    pages
        .iter()
        .map(|(id, page)| (*id, covers.iter().find(|(_, c)| c.overlaps(page)).map(|(label, _)| label.as_str())))
        .collect()
}

#[cfg(any(test, all(target_os = "macos", feature = "browser")))]
/// What has AppKit's first responder in Herdr's window.
#[derive(Clone, Debug, PartialEq)]
pub enum Responder {
    /// GPUI's view: Herdr's own keys, terminals and inputs get typing.
    Herdr,
    Page(String),
    /// Anything else, usually the window itself: AppKit makes the window
    /// first responder when the view that had it leaves (a closed page),
    /// and then plain typing reaches nobody.
    Other,
}

#[cfg(any(test, all(target_os = "macos", feature = "browser")))]
impl std::fmt::Display for Responder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Responder::Herdr => f.write_str("Herdr"),
            Responder::Page(id) => f.write_str(&log_safe(id)),
            Responder::Other => f.write_str("the window (nobody)"),
        }
    }
}

#[cfg(any(test, all(target_os = "macos", feature = "browser")))]
/// What to do so the page `wanted` (None: Herdr) has the keyboard, given
/// who really has it.
#[derive(Debug, PartialEq)]
pub enum Hand<'a> {
    Keep,
    Give(&'a str),
    TakeBack,
}

#[cfg(any(test, all(target_os = "macos", feature = "browser")))]
pub fn hand<'a>(wanted: Option<&'a str>, actual: &Responder) -> Hand<'a> {
    match (wanted, actual) {
        (Some(w), Responder::Page(a)) if a == w => Hand::Keep,
        (Some(w), _) => Hand::Give(w),
        (None, Responder::Herdr) => Hand::Keep,
        (None, _) => Hand::TakeBack,
    }
}

#[cfg(any(test, all(target_os = "macos", feature = "browser")))]
/// Whether a page may take focus when CEF asks (on creation, on a load, on
/// a click): only the page the window gave the keyboard (`wanted`; None
/// before the window first decided), or one the person just pressed.
#[cfg_attr(not(feature = "browser"), allow(dead_code))] // CEF only
pub fn may_focus(page: &str, wanted: Option<Option<&str>>, pressed: bool) -> bool {
    pressed || wanted.is_none_or(|w| w == Some(page))
}


/// The url without its userinfo, query and fragment, for the status of a
/// block whose browser isn't open and for logs: OAuth codes, magic links and
/// passwords stay out of them. The tab document keeps the whole url
/// (restore needs it).
pub fn public_url(url: &str) -> String {
    let url = url.split(['?', '#']).next().unwrap_or_default();
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let (authority, path) = rest.split_at(rest.find(['/', '\\']).unwrap_or(rest.len()));
            let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
            format!("{scheme}://{host}{path}")
        }
        None => url.to_owned(),
    }
}

/// Text from outside Herdr (an agent's CDP method name, a url it asked for,
/// a key binding's action) as a log line may carry it: control characters, line and paragraph
/// separators, bidi controls and invisible format characters escaped
/// (`\n`, `\u{202e}`, `\u{200b}`), so it can't start a line of its own,
/// reorder one, or hide text in one; at most 200 characters.
#[cfg_attr(not(all(target_os = "macos", feature = "browser")), allow(dead_code))]
pub fn log_safe(text: &str) -> String {
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i == 200 {
            out.push('…');
            break;
        }
        if ch.is_control() || hidden(ch) {
            out.extend(ch.escape_default());
        } else {
            out.push(ch);
        }
    }
    out
}

/// A failed command's error as its warning line carries it: escaped like
/// [`log_safe`], but whole, since errors quote ids and urls an agent wrote.
pub fn log_escaped(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch.is_control() || hidden(ch) {
            out.extend(ch.escape_default());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Line and paragraph separators, bidi controls, and format characters
/// that draw nothing (soft hyphen, zero-width spaces and joiners, word
/// joiner and invisible operators, the byte order mark, the tag block).
fn hidden(ch: char) -> bool {
    matches!(ch,
        '\u{00ad}' | '\u{061c}' | '\u{115f}' | '\u{1160}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
        | '\u{2060}'..='\u{206f}' | '\u{3164}' | '\u{feff}' | '\u{ffa0}' | '\u{fff9}'..='\u{fffb}' | '\u{1d173}'..='\u{1d17a}' | '\u{e0000}'..='\u{e007f}')
}

// Errors an agent sees (pinned in agent.rs's tests).

pub fn other_core(terminal: &str) -> String {
    format!("terminal {terminal} is on another Herdr core than this Herdr's: run agent-browser from a terminal in this Herdr")
}
pub fn unknown_core(terminal: &str) -> String {
    format!("Herdr could not tell which core terminal {terminal} is on (hc did not answer), so it isn't attached")
}
/// `browser.agent_browser_setup`: adds Herdr's provider to agent-browser's
/// user config (`~/.agent-browser/config.json`), so a terminal that started
/// before this Herdr (with none of Herdr's `AGENT_BROWSER_*`) can drive a
/// pane with `-p herdr`. It only declares the plugin: agent-browser outside
/// Herdr keeps its own default provider.
pub fn agent_browser_setup() -> Result<serde_json::Value, String> {
    #[cfg(all(target_os = "macos", feature = "browser"))]
    {
        let home = std::env::var_os("HOME").ok_or("HOME is unset")?;
        let path = std::path::Path::new(&home).join(".agent-browser").join("config.json");
        let exe = std::env::current_exe().map_err(|e| format!("cannot find Herdr's own binary: {e}"))?;
        let socket = crate::mods::app_dir().join("herdr-app.sock");
        let existing = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        let text = agent::with_herdr_plugin(&existing, &exe.to_string_lossy(), &socket.to_string_lossy())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        tracing::info!("browser: added Herdr's agent-browser plugin to {}", path.display());
        Ok(serde_json::json!({ "config": path.to_string_lossy(), "use": "agent-browser -p herdr --session herdr-$HC_TERMINAL_ID <command>" }))
    }
    #[cfg(not(all(target_os = "macos", feature = "browser")))]
    {
        Err("this Herdr build has no browser panes".into())
    }
}

/// Whether a caller's core (from the provider) is this app's core. A caller
/// whose core couldn't be read isn't attached: its terminal id could name a
/// terminal of this core that isn't it.
pub fn same_core(terminal: &str, caller: Option<&str>, own: Option<&str>) -> Result<(), String> {
    match (caller, own) {
        (Some(c), Some(o)) if c == o => Ok(()),
        (Some(_), Some(_)) => Err(other_core(terminal)),
        _ => Err(unknown_core(terminal)),
    }
}

/// `result.core.id` of a core's hello.
pub fn core_id(hello: &serde_json::Value) -> Option<String> {
    hello.pointer("/result/core/id").or_else(|| hello.pointer("/core/id")).and_then(serde_json::Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned)
}

pub fn not_in_herdr() -> String {
    "this terminal is not in Herdr (HC_TERMINAL_ID is unset): agent-browser can only drive a Herdr browser pane from a Herdr terminal".into()
}
pub fn parked(terminal: &str) -> String {
    format!("terminal {terminal} has no pane in Herdr (parked): give it one with herdr-app do terminal.keep --terminal {terminal}")
}
pub fn remote(terminal: &str) -> String {
    format!("terminal {terminal} runs on another machine: browser panes are only for this machine's terminals")
}

/// Whether browser panes work in this build.
pub const AVAILABLE: bool = cfg!(all(target_os = "macos", feature = "browser"));

#[cfg(test)]
mod tests {
    use super::{Hand, Rect, Responder, covered, hand, keyboard_page, log_escaped, log_safe, may_focus, public_url};

    #[test]
    fn public_urls_have_no_userinfo_query_or_fragment() {
        assert_eq!(public_url("https://user:pw@example.test/cb?code=SECRET#frag"), "https://example.test/cb");
        assert_eq!(public_url("http://me@127.0.0.1:8765/x"), "http://127.0.0.1:8765/x");
        assert_eq!(public_url("http://127.0.0.1:8765/x.html"), "http://127.0.0.1:8765/x.html");
        assert_eq!(public_url("https://a.test/p@q?x=1"), "https://a.test/p@q");
        assert_eq!(public_url("about:blank"), "about:blank");
    }

    #[test]
    fn logged_text_cannot_start_a_line_of_its_own() {
        assert_eq!(log_safe("Page.navigate"), "Page.navigate");
        assert_eq!(log_safe("Evil.x\nFORGED\r\u{1b}[2J\u{2028}\u{202e}"), "Evil.x\\nFORGED\\r\\u{1b}[2J\\u{2028}\\u{202e}");
        let long = "a".repeat(500);
        assert_eq!(log_safe(&long).chars().count(), 201);
        // Invisible format characters can't hide text either.
        for ch in ['\u{00ad}', '\u{061c}', '\u{200b}', '\u{200d}', '\u{2060}', '\u{2064}', '\u{feff}', '\u{e0041}', '\u{e007f}'] {
            let out = log_safe(&format!("a{ch}b"));
            assert!(!out.contains(ch) && out.starts_with("a\\u{"), "{ch:?} → {out}");
        }
        assert_eq!(log_safe("bücher.localhost"), "bücher.localhost");
    }

    #[test]
    fn a_failed_command_quoting_an_agent_id_is_logged_on_one_line() {
        // `browser.close` on a layout block quotes the block's id in its error.
        let id = format!("x\u{2028}{}\nbrowser: agent attach browser=b1 terminal=t1", "y".repeat(300));
        let error = format!("browser {id} is a block of the shown layout");
        let line = log_escaped(&error);
        assert!(!line.contains('\n') && !line.contains('\u{2028}'), "{line}");
        assert!(line.ends_with("is a block of the shown layout"), "kept whole: {line}");
    }

    #[test]
    fn a_page_holding_the_keyboard_is_logged_escaped() {
        // The keyboard lines log the holder through `Display`.
        let forged = Responder::Page("x\nbrowser: agent attach browser=b1 terminal=t1".into());
        let shown = forged.to_string();
        assert!(!shown.contains('\n'), "{shown}");
        assert_eq!(Responder::Page("b1".into()).to_string(), "b1");
    }

    fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    /// A 1280x800 window: b1 the right half below the tab bar, b2 a side column.
    fn pages() -> Vec<(&'static str, Rect)> {
        vec![("b1", r(640.0, 80.0, 640.0, 720.0)), ("b2", r(0.0, 80.0, 300.0, 720.0))]
    }

    #[test]
    fn no_overlay_hides_no_page() {
        assert_eq!(covered(&pages(), &[]), vec![("b1", None), ("b2", None)]);
    }

    #[test]
    fn an_overlay_hides_only_the_pages_it_overlaps() {
        // The palette hangs from the top, centered: over b1's left edge, clear of b2.
        let covers = vec![("popup frame-palette".to_owned(), r(330.0, 60.0, 620.0, 500.0))];
        assert_eq!(covered(&pages(), &covers), vec![("b1", Some("popup frame-palette")), ("b2", None)]);
        // A toast in the bottom right corner: b1 only.
        let covers = vec![("toast toast".to_owned(), r(924.0, 712.0, 340.0, 72.0))];
        assert_eq!(covered(&pages(), &covers), vec![("b1", Some("toast toast")), ("b2", None)]);
    }

    #[test]
    fn a_dialogs_backdrop_hides_every_page() {
        let covers = vec![("dialog prompt".to_owned(), r(0.0, 0.0, 1280.0, 800.0))];
        assert_eq!(covered(&pages(), &covers), vec![("b1", Some("dialog prompt")), ("b2", Some("dialog prompt"))]);
    }

    #[test]
    fn the_first_overlay_painted_over_a_page_names_it() {
        let covers = vec![("float notes".to_owned(), r(700.0, 100.0, 100.0, 100.0)), ("toast toast".to_owned(), r(924.0, 712.0, 340.0, 72.0))];
        assert_eq!(covered(&pages(), &covers)[0], ("b1", Some("float notes")));
    }

    #[test]
    fn touching_edges_and_empty_overlays_hide_nothing() {
        // Ends exactly where b1 starts; a float not measured yet (0x0) inside b1.
        let covers = vec![("float side".to_owned(), r(300.0, 80.0, 340.0, 720.0)), ("float new".to_owned(), r(700.0, 100.0, 0.0, 0.0))];
        assert_eq!(covered(&pages(), &covers), vec![("b1", None), ("b2", None)]);
    }

    #[test]
    fn a_page_under_an_overlay_hands_the_keyboard_to_herdr() {
        let hidden = ["b1".to_owned()];
        // The focused pane's page is under a toast: Herdr types meanwhile.
        assert_eq!(keyboard_page(false, false, None, Some("b1"), &hidden), None);
        // A clicked layout page under an overlay doesn't pass it to the pane's page.
        assert_eq!(keyboard_page(false, false, Some("b1"), Some("b2"), &hidden), None);
        // Another page isn't affected.
        assert_eq!(keyboard_page(false, false, None, Some("b2"), &hidden), Some("b2"));
        // The overlay went: the page that still should have it gets it back.
        assert_eq!(keyboard_page(false, false, None, Some("b1"), &[]), Some("b1"));
    }

    #[test]
    fn closing_the_page_with_the_keyboard_hands_it_back_to_herdr() {
        // The page's view left the window, AppKit made the window first
        // responder: Herdr must take the keyboard back, or typing goes nowhere.
        assert_eq!(hand(None, &Responder::Other), Hand::TakeBack);
        assert_eq!(hand(None, &Responder::Page("b1".into())), Hand::TakeBack);
        assert_eq!(hand(None, &Responder::Herdr), Hand::Keep);
    }

    #[test]
    fn a_page_gets_the_keyboard_from_anyone_else() {
        assert_eq!(hand(Some("b2"), &Responder::Page("b2".into())), Hand::Keep);
        assert_eq!(hand(Some("b2"), &Responder::Page("b1".into())), Hand::Give("b2"));
        assert_eq!(hand(Some("b2"), &Responder::Herdr), Hand::Give("b2"));
        assert_eq!(hand(Some("b2"), &Responder::Other), Hand::Give("b2"));
    }

    #[test]
    fn only_the_wanted_or_pressed_page_takes_focus() {
        // CEF's creation focus on a page that shouldn't have the keyboard.
        assert!(!may_focus("b1", Some(Some("b2")), false));
        assert!(!may_focus("b1", Some(None), false));
        // The window gave it the keyboard, or the person clicked it.
        assert!(may_focus("b2", Some(Some("b2")), false));
        assert!(may_focus("b1", Some(None), true));
        // Before the window decided anything.
        assert!(may_focus("b1", None, false));
    }

    #[test]
    fn a_dialog_takes_the_keyboard_from_every_page() {
        assert_eq!(keyboard_page(true, false, Some("b5"), Some("b1"), &[]), None);
        assert_eq!(keyboard_page(true, false, None, Some("b1"), &[]), None);
    }

    #[test]
    fn a_control_the_person_pressed_keeps_the_keyboard_from_every_page() {
        // The Changes panel's commit box, a floating terminal: typing goes there.
        assert_eq!(keyboard_page(false, true, None, Some("b1"), &[]), None);
        assert_eq!(keyboard_page(false, true, Some("b5"), Some("b1"), &[]), None);
    }

    #[test]
    fn a_clicked_layout_page_wins_over_the_focused_pane() {
        assert_eq!(keyboard_page(false, false, Some("b5"), Some("b1"), &[]), Some("b5"));
        assert_eq!(keyboard_page(false, false, Some("b5"), None, &[]), Some("b5"));
    }

    #[test]
    fn otherwise_the_focused_panes_page_or_herdr() {
        assert_eq!(keyboard_page(false, false, None, Some("b1"), &[]), Some("b1"));
        assert_eq!(keyboard_page(false, false, None, None, &[]), None);
    }
}
