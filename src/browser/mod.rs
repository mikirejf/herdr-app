//! Browser panes: a web page in a block (`{ "mod": "browser-prototype", "props": { "browser": "b1", "url": … } }`),
//! as a pane in a tab or a block in the shown layout.
//!
//! macOS builds with an engine feature show each page as a native child view
//! over GPUI's Metal view, kept on its block's bounds every frame (native.rs,
//! view.rs). The engine is one module behind `engine`:
//! - `browser`: CEF in windowed mode (runtime.rs), Chromium shipped in the bundle.
//! - `browser-webkit`: the system's WKWebView (webkit/), nothing shipped.
//!
//! Both turn on `browser-pane`, the engine-neutral rest: registry, key
//! routing, overlay cover, consent, the agents' token proxy and allowlist.
//! Every other build has the same API with no browser behind it: blocks show
//! a placeholder and the commands say what's missing (off.rs). The window
//! only talks to `Browsers`.
//!
//! What the shared code calls on `engine` (both engines have each):
//! `set_events`, `start`, `open`, `close`, `navigate`, `history`, `reload`,
//! `devtools`, `zoom`, `set_zoom`, `set_viewport`, `page_view`, `fit`,
//! `set_focus`, `pressed`, `agent_event`, `clicked_herdr`, `covers_changed`, `wake`,
//! `send_devtools` (an allowed CDP message from proxy.rs) and `agent_gone`.
//! Page events and answers go back through `proxy::from_page`.

#[cfg(all(target_os = "macos", feature = "browser", feature = "browser-webkit"))]
compile_error!("pick one browser engine: the `browser` feature (CEF) or `browser-webkit` (WKWebView), not both");
#[cfg(all(target_os = "macos", feature = "browser-pane", not(any(feature = "browser", feature = "browser-webkit"))))]
compile_error!("browser-pane (or dev-agent-access) needs an engine: add the `browser` feature (CEF) or `browser-webkit` (WKWebView)");

#[cfg(any(test, all(target_os = "macos", feature = "browser-pane")))]
pub mod agent;
#[cfg(any(test, all(target_os = "macos", feature = "browser-pane")))]
pub mod emulate;
#[cfg(any(test, all(target_os = "macos", feature = "browser-pane")))]
pub mod keys;
#[cfg(any(test, all(target_os = "macos", feature = "browser-pane")))]
pub mod registry;
#[cfg(any(test, all(target_os = "macos", feature = "browser-webkit")))]
pub mod translate;

#[cfg(all(target_os = "macos", feature = "browser-pane"))]
mod confirm;
#[cfg(all(target_os = "macos", feature = "browser-pane"))]
mod native;
#[cfg(all(target_os = "macos", feature = "browser-pane"))]
mod proxy;
#[cfg(all(target_os = "macos", feature = "browser"))]
mod runtime;
#[cfg(all(target_os = "macos", feature = "browser"))]
use runtime as engine;
#[cfg(all(target_os = "macos", feature = "browser-webkit", not(feature = "browser")))]
mod webkit;
#[cfg(all(target_os = "macos", feature = "browser-webkit", not(feature = "browser")))]
use webkit as engine;
#[cfg(all(target_os = "macos", feature = "browser-pane"))]
mod view;
#[cfg(all(target_os = "macos", feature = "browser-pane"))]
pub use native::{Browsers, Event};

#[cfg(not(all(target_os = "macos", feature = "browser-pane")))]
mod off;
#[cfg(not(all(target_os = "macos", feature = "browser-pane")))]
pub use off::{Browsers, Event};

/// Shows `ask` as an app-modal macOS alert and waits for the person's
/// answer (`confirm::ask`). Call it from a foreground task, never while
/// the window or app is borrowed: AppKit runs a modal loop inside it.
#[cfg(all(target_os = "macos", feature = "browser-pane"))]
pub fn confirm(ask: &Ask) -> Reply {
    confirm::ask(ask)
}

/// No browser here, so no dialog: nothing turns access on.
#[cfg(not(all(target_os = "macos", feature = "browser-pane")))]
pub fn confirm(_: &Ask) -> Reply {
    Reply { answer: Answer::Closed, after: std::time::Duration::ZERO, app_active: false }
}

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

#[cfg(any(test, all(target_os = "macos", feature = "browser-pane")))]
/// A rectangle in window points, top-left origin (as GPUI paints).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[cfg(any(test, all(target_os = "macos", feature = "browser-pane")))]
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

#[cfg(any(test, all(target_os = "macos", feature = "browser-pane")))]
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

#[cfg(any(test, all(target_os = "macos", feature = "browser-pane")))]
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

#[cfg(any(test, all(target_os = "macos", feature = "browser-pane")))]
impl std::fmt::Display for Responder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Responder::Herdr => f.write_str("Herdr"),
            Responder::Page(id) => f.write_str(&log_safe(id)),
            Responder::Other => f.write_str("the window (nobody)"),
        }
    }
}

#[cfg(any(test, all(target_os = "macos", feature = "browser-pane")))]
/// What to do so the page `wanted` (None: Herdr) has the keyboard, given
/// who really has it.
#[derive(Debug, PartialEq)]
pub enum Hand<'a> {
    Keep,
    Give(&'a str),
    TakeBack,
}

#[cfg(any(test, all(target_os = "macos", feature = "browser-pane")))]
pub fn hand<'a>(wanted: Option<&'a str>, actual: &Responder) -> Hand<'a> {
    match (wanted, actual) {
        (Some(w), Responder::Page(a)) if a == w => Hand::Keep,
        (Some(w), _) => Hand::Give(w),
        (None, Responder::Herdr) => Hand::Keep,
        (None, _) => Hand::TakeBack,
    }
}

#[cfg(any(test, all(target_os = "macos", feature = "browser-pane")))]
/// Whether a page may take focus when CEF asks (on creation, on a load, on
/// a click): only the page the window gave the keyboard (`wanted`; None
/// before the window first decided), or one the person just pressed.
#[cfg_attr(not(feature = "browser"), allow(dead_code))] // CEF only
pub fn may_focus(page: &str, wanted: Option<Option<&str>>, pressed: bool) -> bool {
    pressed || wanted.is_none_or(|w| w == Some(page))
}


// ---- agent access: what the window needs in every build (rules: agent.rs)

/// Where a change to agent access came from (and the audit log's `by=`).
#[cfg_attr(not(all(target_os = "macos", feature = "browser-pane")), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The person answered Allow in the native macOS dialog that the pane
    /// header's "Allow agent" opens: the one source that turns access on.
    NativeConfirm,
    /// Herdr's own Stop buttons (the pane header, the window's strip).
    PaneControl,
    /// `herdr-app do` over the control socket: agents can reach it.
    Socket,
    /// A mod's `run()` (and keys.json bindings, the native key monitor): agents write mods and keys.
    Mod,
    /// The dev-only switch (debug builds with the `dev-agent-access` feature,
    /// `HERDR_DEV_AGENT_ACCESS=1` at launch).
    DevSwitch,
    Close,
    Quit,
}

#[cfg_attr(not(all(target_os = "macos", feature = "browser-pane")), allow(dead_code))]
impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::NativeConfirm => "native confirm",
            Source::PaneControl => "pane control",
            Source::Socket => "socket",
            Source::Mod => "mod or key",
            #[cfg(all(debug_assertions, feature = "dev-agent-access"))]
            Source::DevSwitch => "socket (dev switch, dev-agent-access build)",
            // Never set in builds without the dev feature.
            #[cfg(not(all(debug_assertions, feature = "dev-agent-access")))]
            Source::DevSwitch => "socket",
            Source::Close => "close",
            Source::Quit => "quit",
        }
    }
}

/// The audit log line for an agent access change. A browser id can be
/// agent text (a layout block's `browser`), so it is escaped (`log_safe`):
/// an id can't add a line of its own.
#[cfg_attr(not(all(target_os = "macos", feature = "browser-pane")), allow(dead_code))]
pub fn access_line(on: bool, browser: &str, by: Source) -> String {
    format!("browser: agent access {} browser={} by={}", if on { "on" } else { "off" }, log_safe(browser), by.label())
}

/// The access button in an agent browser's header: its label and what a
/// click asks for, both from the state it was painted with. The click acts
/// on what the person saw: a "Stop agent" still on screen after a change it
/// never showed (the socket turned access off) means off. Toggling the live
/// state instead would turn access back on. "Allow agent…" only opens the
/// native dialog (`Browsers::ask`); access turns on only from its answer.
pub fn access_button(shown_on: bool) -> (&'static str, bool) {
    if shown_on { ("Stop agent", false) } else { ("Allow agent…", true) }
}

#[cfg(any(test, all(target_os = "macos", feature = "browser-pane")))]
/// How long the native dialog's Allow stays disabled after it shows, and
/// the least time between showing and an Allow that counts: a double click
/// whose first click opened the dialog can't land its second on Allow.
/// Chromium waits the same 500 ms before its permission prompts take a click
/// (`InputEventActivationProtector`).
pub const ALLOW_AFTER: std::time::Duration = std::time::Duration::from_millis(500);

/// A native "Let agents drive this browser?" dialog about to show
/// (`Browsers::ask`), and what it names.
#[cfg_attr(not(all(target_os = "macos", feature = "browser-pane")), allow(dead_code))]
#[derive(Clone, Debug)]
pub struct Ask {
    pub browser: String,
    /// `scheme://host[:port]` of the page when the dialog opened.
    pub site: String,
    pub title: String,
    pub body: String,
}

/// Which button closed the dialog.
#[cfg_attr(not(all(target_os = "macos", feature = "browser-pane")), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// "Allow" (no key equivalent).
    Allow,
    /// "Cancel": the default button, so Return and Esc too.
    Cancel,
    /// Anything else: the dialog went away without a button.
    Closed,
}

/// What came back from the dialog.
#[cfg_attr(not(all(target_os = "macos", feature = "browser-pane")), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
pub struct Reply {
    pub answer: Answer,
    /// How long it was up.
    pub after: std::time::Duration,
    /// Herdr was the active app when it was answered.
    pub app_active: bool,
}

#[cfg(all(debug_assertions, feature = "dev-agent-access"))]
static DEV_SWITCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What every agent header and the window's agent strip say while the dev
/// switch is on: the person's click is no longer the only way in. Builds
/// without the dev feature carry no such text.
pub fn dev_switch_note() -> Option<&'static str> {
    #[cfg(all(debug_assertions, feature = "dev-agent-access"))]
    if dev_switch() {
        return Some("DEV SWITCH: the control socket can turn agent access on");
    }
    None
}

/// Debug builds with the `dev-agent-access` feature only:
/// `HERDR_DEV_AGENT_ACCESS=1` in the app's own environment at launch lets
/// the control socket turn agent access on, so an agent can test the path
/// end to end without a person's click. That drops the consent property, so
/// the dev bundle people run (`just browser-dev`) is built without it. Read
/// once at startup and removed from the environment, so terminals never
/// inherit it. Other builds compile none of this: the variable means nothing.
#[cfg_attr(not(all(target_os = "macos", feature = "browser-pane")), allow(dead_code))]
pub fn read_dev_switch() {
    #[cfg(all(debug_assertions, feature = "dev-agent-access"))]
    {
        const NAME: &str = "HERDR_DEV_AGENT_ACCESS";
        let on = std::env::var(NAME).is_ok_and(|v| v == "1");
        // SAFETY: called first thing in main, before any other thread exists.
        unsafe { std::env::remove_var(NAME) };
        DEV_SWITCH.store(on, std::sync::atomic::Ordering::SeqCst);
    }
}

pub fn dev_switch() -> bool {
    #[cfg(all(debug_assertions, feature = "dev-agent-access"))]
    return DEV_SWITCH.load(std::sync::atomic::Ordering::SeqCst);
    #[cfg(not(all(debug_assertions, feature = "dev-agent-access")))]
    false
}

/// The url without its userinfo, query and fragment, wherever a url leaves
/// the app for a browser agents can't drive (`herdr-app ls`, mods'
/// `browsers()`, the status of a block whose browser isn't open), and in
/// logs: OAuth codes, magic links and passwords stay out of reach. The tab
/// document keeps the whole url (restore needs it).
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

/// A page's title wherever its url goes out as `public_url`. Chromium titles
/// a page that has none with its url ("127.0.0.1:8765/cb?code=…", no scheme
/// or userinfo, maybe unescaped, the host maybe in Unicode), so a title
/// that starts with the page's host, ends its head with the page's path
/// (as is or unescaped), or reads as a url (a host with a `.` or `:`, then
/// `/`, then `?` or `#`, and no space before those) is cut the same way.
/// Then, whatever the title looks like, the url's query, fragment and
/// userinfo come out of it, matched as they are and unescaped: the shape
/// tests miss a Unicode host with a space in the path.
/// Other titles pass.
#[cfg_attr(not(all(target_os = "macos", feature = "browser-pane")), allow(dead_code))]
pub fn public_title(title: &str, url: &str) -> String {
    without_url_secrets(&url_title_cut(title, url), url)
}

fn url_title_cut(title: &str, url: &str) -> String {
    let (before, query) = url.split_once('?').map_or((url, ""), |(b, q)| (b, q.split('#').next().unwrap_or_default()));
    let public = public_url(before);
    let (host, path) = public
        .split_once("://")
        .map(|(_, rest)| {
            let (host, path) = rest.split_at(rest.find(['/', '\\']).unwrap_or(rest.len()));
            (host.to_ascii_lowercase(), path)
        })
        .unwrap_or_default();
    let shown = title.trim_start();
    let shown = shown.split_once("://").filter(|(scheme, _)| !scheme.contains([' ', '/'])).map_or(shown, |(_, rest)| rest);
    let shown = shown.split_once('@').filter(|(user, _)| !user.contains(['/', ' '])).map_or(shown, |(_, rest)| rest).to_ascii_lowercase();
    let hosts = [host.as_str(), host.strip_prefix("www.").unwrap_or_default()];
    let carries_query = !query.is_empty() && title.contains(query);
    let head = shown.split(['?', '#']).next().unwrap_or_default();
    let hostish = head.split_once('/').is_some_and(|(host, _)| host.contains(['.', ':']));
    let reads_as_url = head.len() < shown.len() && hostish && !head.contains(char::is_whitespace);
    // The page's path after a host-shaped head, as Chromium shows it (a
    // Unicode host, `%20` as a space): the url, whatever its host looks like.
    let has_path = path.len() > 1 && hostish && [path.to_ascii_lowercase(), unescape(path).to_lowercase()].iter().any(|p| head.ends_with(p.as_str()));
    if !carries_query && !reads_as_url && !has_path && !hosts.iter().any(|h| !h.is_empty() && shown.starts_with(h)) {
        return title.to_owned();
    }
    let title = if carries_query { title.replace(query, "") } else { title.to_owned() };
    let cut = title.split(['?', '#']).next().unwrap_or_default();
    match cut.split_once('@') {
        Some((user, rest)) if !user.contains(['/', ' ']) => rest.to_owned(),
        _ => cut.to_owned(),
    }
}

/// `title` with the url's query, fragment, `userinfo@` and password taken
/// out, each matched as it is in the url and unescaped (Chromium shows a
/// url's `%20` as a space), then any `?`, `#`, `&` or `=` left dangling
/// at its end. A title that carries none of them passes as it is.
fn without_url_secrets(title: &str, url: &str) -> String {
    let (pre, fragment) = url.split_once('#').unwrap_or((url, ""));
    let query = pre.split_once('?').map_or("", |(_, q)| q);
    let authority = pre.split_once("://").map_or("", |(_, rest)| rest.split(['/', '\\', '?']).next().unwrap_or_default());
    let userinfo = authority.rsplit_once('@').map_or(String::new(), |(u, _)| format!("{u}@"));
    let password = userinfo.trim_end_matches('@').split_once(':').map_or("", |(_, p)| p);
    let secrets: Vec<&str> = [query, fragment, userinfo.as_str(), password].into_iter().filter(|s| !s.is_empty()).collect();
    let mut out = title.to_owned();
    let mut found = false;
    for s in &secrets {
        if out.contains(s) {
            out = out.replace(s, "");
            found = true;
        }
    }
    let unescaped: Vec<String> = secrets.iter().map(|s| unescape(s)).filter(|s| !s.is_empty()).collect();
    if unescaped.iter().any(|s| unescape(&out).contains(s.as_str())) {
        out = unescape(&out);
        for s in &unescaped {
            out = out.replace(s.as_str(), "");
        }
        found = true;
    }
    if found { out.trim_end_matches(['?', '#', '&', '=']).to_owned() } else { out }
}

/// `%XX` escapes decoded (invalid UTF-8 as U+FFFD); `+` stays a `+`, as
/// in Chromium's display of a url.
fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (bytes[i], bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Text from outside Herdr (an agent's CDP method name, a url it asked for,
/// a key binding's action, a site the consent dialog names) as a log line or
/// the dialog may carry it: control characters, line and paragraph
/// separators, bidi controls and invisible format characters escaped
/// (`\n`, `\u{202e}`, `\u{200b}`), so it can't start a line of its own,
/// reorder one, or hide text in one; at most 200 characters.
#[cfg_attr(not(all(target_os = "macos", feature = "browser-pane")), allow(dead_code))]
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
    #[cfg(all(target_os = "macos", feature = "browser-pane"))]
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
    #[cfg(not(all(target_os = "macos", feature = "browser-pane")))]
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
pub const AVAILABLE: bool = cfg!(all(target_os = "macos", feature = "browser-pane"));

#[cfg(test)]
mod tests {
    use super::{Hand, Rect, Responder, Source, access_line, covered, hand, keyboard_page, log_escaped, log_safe, may_focus, public_title, public_url};

    #[test]
    fn public_urls_have_no_userinfo_query_or_fragment() {
        assert_eq!(public_url("https://user:pw@example.test/cb?code=SECRET#frag"), "https://example.test/cb");
        assert_eq!(public_url("http://me@127.0.0.1:8765/x"), "http://127.0.0.1:8765/x");
        assert_eq!(public_url("http://127.0.0.1:8765/x.html"), "http://127.0.0.1:8765/x.html");
        assert_eq!(public_url("https://a.test/p@q?x=1"), "https://a.test/p@q");
        assert_eq!(public_url("about:blank"), "about:blank");
    }

    #[test]
    fn a_title_that_is_the_url_goes_out_as_the_url_does() {
        // Chromium's title for a page with no <title>: the url with no scheme.
        let url = "http://127.0.0.1:18766/notitle.html?code=SECRETQ2#frag";
        assert_eq!(public_title("127.0.0.1:18766/notitle.html?code=SECRETQ2", url), "127.0.0.1:18766/notitle.html");
        assert_eq!(public_title("http://127.0.0.1:18766/notitle.html?code=SECRETQ2", url), "http://127.0.0.1:18766/notitle.html");
        assert_eq!(public_title("user:pw@127.0.0.1:18766/notitle.html?code=SECRETQ2", "http://user:pw@127.0.0.1:18766/notitle.html?code=SECRETQ2"), "127.0.0.1:18766/notitle.html");
        assert_eq!(public_title("example.test/cb?code=1", "https://www.example.test/cb?code=1"), "example.test/cb");
        assert_eq!(public_title("EXAMPLE.test/cb#t=1", "https://example.test/cb#t=1"), "EXAMPLE.test/cb");
        // A title that carries the query, however it starts.
        assert_eq!(public_title("Signed in: code=SECRETQ3", "https://a.test/cb?code=SECRETQ3"), "Signed in: ");
        assert!(!public_title("x ?code=SECRETQ3", "https://a.test/cb?code=SECRETQ3").contains("SECRET"));
        // A Unicode host and an unescaped query: the host and query differ
        // from the url's, but the title still reads as a url.
        let idn = "http://xn--bcher-kva.localhost:18791/notitle.html?code=SECRET%20C1#fragSECRET";
        assert_eq!(public_title("bücher.localhost:18791/notitle.html?code=SECRET C1", idn), "bücher.localhost:18791/notitle.html");
        assert_eq!(public_title(" bücher.localhost:18791/notitle.html?code=SECRETH1é", idn), " bücher.localhost:18791/notitle.html");
        assert_eq!(public_title("bücher.localhost:18791/notitle.html#fragSECRET", idn), "bücher.localhost:18791/notitle.html");
        // Seen live: a Unicode host and a space in the path.
        // The host prefix (punycode), the raw query (`%20`) and the
        // no-whitespace url shape all missed at once.
        let b4 = concat!("http://me:", "p", "w", "@xn--bcher-kva.localhost:18795/no%20title.html?code=SECRET%20Q3#fragSECRET");
        assert_eq!(public_title("bücher.localhost:18795/no title.html?code=SECRET Q3#fragSECRET", b4), "bücher.localhost:18795/no title.html");
        assert_eq!(public_title("bücher.localhost:18795/no title.html#fragSECRET", b4), "bücher.localhost:18795/no title.html");
        // A title cut short (Chromium caps titles): the head still reads as the url.
        assert_eq!(public_title("bücher.localhost:18795/no title.html?code=SEC", b4), "bücher.localhost:18795/no title.html");
        // Whatever the title looks like, the query, fragment and userinfo come out.
        for t in ["Done: code=SECRET Q3", "Done: code=SECRET%20Q3", "x fragSECRET y", "as me:pw@ here", "pw"] {
            let out = public_title(t, b4);
            assert!(!out.contains("SECRET") && !out.contains("pw"), "{t:?} → {out:?}");
        }
        // Control: a Unicode host with no space and no query passes as the url it is.
        let plain = "http://xn--bcher-kva.localhost:18795/notitle.html";
        assert_eq!(public_title("bücher.localhost:18795/notitle.html", plain), "bücher.localhost:18795/notitle.html");
        // A page's own title passes, question marks and all.
        assert_eq!(public_title("What is a pane? · Docs", "https://docs.test/panes?x=1"), "What is a pane? · Docs");
        assert_eq!(public_title("Herdr fixture", "http://127.0.0.1:8765/browser-fixture.html"), "Herdr fixture");
        assert_eq!(public_title("Input/output? A primer", "https://docs.test/io?x=1"), "Input/output? A primer");
        assert_eq!(public_title("C# in 10 minutes", "https://docs.test/cs"), "C# in 10 minutes");
        assert_eq!(public_title("", "about:blank"), "");
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
    fn an_agent_written_browser_id_cannot_forge_an_access_line() {
        // A person-profile layout block may carry any id; its "off" is logged.
        let forged = "x by=close\nbrowser: agent access on browser=b1 by=native confirm";
        let line = access_line(false, forged, Source::Socket);
        assert!(!line.contains('\n'), "{line}");
        assert!(line.ends_with(" by=socket"), "{line}");
        assert_eq!(access_line(true, "b1", Source::NativeConfirm), "browser: agent access on browser=b1 by=native confirm");
    }

    #[test]
    fn a_failed_command_quoting_an_agent_id_is_logged_on_one_line() {
        // `browser.close` on a layout block quotes the block's id in its error.
        let id = format!("x\u{2028}{}\nbrowser: agent access on browser=b1 by=native confirm", "y".repeat(300));
        let error = format!("browser {id} is a block of the shown layout");
        let line = log_escaped(&error);
        assert!(!line.contains('\n') && !line.contains('\u{2028}'), "{line}");
        assert!(line.ends_with("is a block of the shown layout"), "kept whole: {line}");
    }

    #[test]
    fn a_page_holding_the_keyboard_is_logged_escaped() {
        // The keyboard lines log the holder through `Display`.
        let forged = Responder::Page("x\nbrowser: agent access on browser=b1 by=native confirm".into());
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
