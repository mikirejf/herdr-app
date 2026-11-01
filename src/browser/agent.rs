//! Agents drive a browser pane: the rules, with no CEF and no
//! sockets, so every security control here is unit-tested.
//!
//! The path: `agent-browser` in a Herdr terminal runs `herdr-app
//! __agent-browser-provider` (its plugin), which asks the window over the
//! control socket to `attach`. The window picks the caller's browser and hands back
//! `ws://127.0.0.1:<port>/<token>`: one token per browser pane. proxy.rs serves
//! that socket as the pane's browser endpoint: the Target domain is answered
//! here from the pane's tabs, and every other message goes to one tab's
//! in-process DevTools session, through the allowlist below. CEF opens no
//! remote-debugging port.

// Default builds compile these rules only for their tests; the browser feature uses them.
#![cfg_attr(not(all(target_os = "macos", feature = "browser")), allow(dead_code))]

use serde_json::{Map, Value, json};

pub use super::{not_in_herdr, public_url};
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};

pub const PROTOCOL: &str = "agent-browser.plugin.v1";
pub const PLUGIN: &str = "herdr";
/// The argv verb agent-browser runs (`herdr-app __agent-browser-provider --socket <path>`).
pub const VERB: &str = "__agent-browser-provider";
/// `agent-browser plugin run herdr herdr.check`: why a launch would fail.
/// agent-browser hides a provider's error text on launch (it prints only
/// "Plugin 'herdr' returned success=false"); `plugin run` shows it.
pub const CHECK: &str = "herdr.check";
/// Largest websocket frame from an agent, and largest provider request.
pub const MAX_FRAME: usize = 1 << 20;
/// Requests an agent may have waiting on the page (DevTools runs on the main thread).
pub const MAX_IN_FLIGHT: usize = 256;

// ---- the URL gate ---------------------------------------------------------

/// The one URL gate for every navigation sink (`browser.open`/`navigate`,
/// layout restore, popups, CDP `Page.navigate`): `http`, `https`, or exactly
/// `about:blank`. Everything else (`file:`, `chrome:`, `devtools:`,
/// `javascript:`, `data:`, ...) is refused: a privileged or local page under
/// a trusted DevTools session reads files and the browser's internals.
pub fn allowed_url(url: &str) -> bool {
    if url == "about:blank" {
        return true;
    }
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://")) && url.len() > lower.find("//").unwrap_or(0) + 2
}

/// What the logs may say about a url: scheme, host and path (no userinfo,
/// query or fragment, where tokens and codes ride), with control characters
/// escaped (`super::log_safe`: a url an agent sends can't forge a log line),
/// at most 80 characters before escaping. It is cut first, then escaped, so
/// the cut never splits an escape (`\u{202e}` into `\u{20`).
pub fn loggable(url: &str) -> String {
    let url = public_url(url);
    if url.chars().count() <= 80 { super::log_safe(&url) } else { format!("{}…", super::log_safe(&url.chars().take(77).collect::<String>())) }
}

/// Whether a popup (`window.open`, a target link) may navigate the pane: its
/// url passes the gate, and a frame from another origin than the page needs
/// the person's gesture (an ad iframe can't move the top page on its own).
pub fn popup_allowed(target: &str, frame_url: &str, main_url: &str, is_main_frame: bool, user_gesture: bool) -> bool {
    allowed_url(target) && (is_main_frame || user_gesture || origin(frame_url) == origin(main_url))
}

/// `scheme://host[:port]`, lowercased.
fn origin(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
            let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
            format!("{scheme}://{host}").to_ascii_lowercase()
        }
        None => url.to_ascii_lowercase(),
    }
}

/// Chromium switches that would open a DevTools port or let pages reach one.
/// Herdr refuses to start CEF when its command line carries any of them.
#[cfg_attr(not(feature = "browser"), allow(dead_code))] // CEF only
pub const FORBIDDEN_SWITCHES: [&str; 4] = ["remote-debugging-port", "remote-debugging-pipe", "remote-debugging-address", "remote-allow-origins"];

#[cfg_attr(not(feature = "browser"), allow(dead_code))] // CEF only
pub fn forbidden_switch<'a>(args: impl IntoIterator<Item = &'a str>) -> Option<&'static str> {
    args.into_iter().find_map(|a| {
        let name = a.trim_start_matches('-').split('=').next().unwrap_or_default().to_ascii_lowercase();
        (a.starts_with('-')).then_some(()).and_then(|()| FORBIDDEN_SWITCHES.iter().copied().find(|s| *s == name))
    })
}

// ---- errors agents see (each pinned by a test) -----------------------------

pub fn no_app(socket: &str) -> String {
    format!("Herdr is not running (no socket at {socket})")
}
/// The agent-browser session a terminal without Herdr's environment uses.
pub fn own_session(terminal: &str) -> String {
    format!("herdr-{terminal}")
}
pub fn needs_own_session(terminal: &str) -> String {
    format!(
        "terminal {terminal} started before Herdr set up agent-browser for it: run agent-browser -p herdr --session {} <command>",
        own_session(terminal)
    )
}
pub fn no_browser() -> String {
    "no browser in this space: open one with: herdr-app do browser.open --url <url>".into()
}
pub fn ambiguous(browsers: &[&str]) -> String {
    // agent-browser's daemon reads the provider's environment once, when it
    // starts (often on the command that got this error), so a new
    // HERDR_BROWSER needs a new session too.
    format!(
        "two or more browsers here ({}): run `export HERDR_BROWSER=<id> AGENT_BROWSER_SESSION=$AGENT_BROWSER_SESSION-<id>`, then agent-browser again (a running agent-browser session keeps the environment it started with)",
        browsers.join(", ")
    )
}
// ---- which browser the caller means -----------------------------------------

/// One open browser, as the selection sees it.
#[derive(Clone, Debug)]
pub struct Candidate<'a> {
    pub browser: &'a str,
    /// None: a block of the shown layout (on every space).
    pub space: Option<&'a str>,
    pub tab: Option<&'a str>,
    pub focused_at: u64,
}

/// The terminal asking (`HC_TERMINAL_ID`, a routing hint, not an identity) and where it is.
pub struct Caller<'a> {
    pub space: &'a str,
    pub tab: &'a str,
    /// `HERDR_BROWSER`.
    pub wanted: Option<&'a str>,
}

/// The browser an agent in `caller` means, and how it was chosen:
/// `HERDR_BROWSER` (within the caller's space, or a layout block), else the
/// only browser in the caller's tab, else the one in its space the person
/// focused last, else the only one in its space. DevTools windows are never
/// browsers here.
pub fn select<'a>(caller: &Caller, all: &[Candidate<'a>]) -> Result<(&'a str, &'static str), String> {
    if let Some(w) = caller.wanted {
        return all
            .iter()
            .find(|c| c.browser == w && c.space.is_none_or(|s| s == caller.space))
            .map(|c| (c.browser, "HERDR_BROWSER"))
            .ok_or_else(|| format!("HERDR_BROWSER={w} is not a browser in this terminal's space: see `herdr-app ls`"));
    }
    let in_space: Vec<&Candidate> = all.iter().filter(|c| c.space == Some(caller.space)).collect();
    let in_tab: Vec<&Candidate> = in_space.iter().copied().filter(|c| c.tab == Some(caller.tab)).collect();
    match in_tab.as_slice() {
        [one] => return Ok((one.browser, "tab")),
        [] => {}
        many => return Err(ambiguous(&many.iter().map(|c| c.browser).collect::<Vec<_>>())),
    }
    match in_space.as_slice() {
        [] => Err(no_browser()),
        [one] => Ok((one.browser, "space")),
        many => {
            let top = many.iter().map(|c| c.focused_at).max().unwrap_or(0);
            let recent: Vec<&&Candidate> = many.iter().filter(|c| c.focused_at == top).collect();
            match recent.as_slice() {
                [one] if top > 0 => Ok((one.browser, "recent")),
                _ => Err(ambiguous(&many.iter().map(|c| c.browser).collect::<Vec<_>>())),
            }
        }
    }
}

// ---- tokens -------------------------------------------------------------------

/// The live tokens: 32 random bytes each, one per browser, in memory only
/// (an app restart forgets them). A new attach for a browser replaces its
/// token; browser close and quit revoke it.
#[derive(Default)]
pub struct Tokens {
    grants: Vec<([u8; 32], String)>,
}

impl Tokens {
    /// Gives `browser` a new token from `random` (the OS CSPRNG's bytes) and
    /// returns it as 64 hex characters, the path of the agent's url.
    pub fn issue(&mut self, browser: &str, random: [u8; 32]) -> String {
        self.revoke(browser);
        self.grants.push((random, browser.to_owned()));
        random.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The browser a token names. Every live token is compared in full, in
    /// constant time, so timing says nothing about how much of it matched.
    pub fn check(&self, token: &str) -> Option<&str> {
        let bytes = from_hex(token)?;
        let mut found = None;
        for (t, browser) in &self.grants {
            let mut diff = 0u8;
            for (a, b) in t.iter().zip(bytes.iter()) {
                diff |= a ^ b;
            }
            if diff == 0 {
                found = Some(browser.as_str());
            }
        }
        found
    }

    pub fn revoke(&mut self, browser: &str) -> bool {
        let before = self.grants.len();
        self.grants.retain(|(_, b)| b != browser);
        self.grants.len() != before
    }

    pub fn browsers(&self) -> Vec<String> {
        self.grants.iter().map(|(_, b)| b.clone()).collect()
    }
}

fn from_hex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

// ---- the websocket upgrade ------------------------------------------------------

/// What the listener read from a request before answering it.
pub struct Upgrade<'a> {
    /// A websocket upgrade (`Upgrade: websocket`); anything else gets a bare 404.
    pub websocket: bool,
    pub path: &'a str,
    pub host: Option<&'a str>,
    pub origin: Option<&'a str>,
    /// A head tungstenite might read differently than this parser: a
    /// second `Host` or `Origin`, a line ending that isn't `\r\n`, or a
    /// folded header line. Refused.
    pub malformed: bool,
}

/// What `check_upgrade` needs from a request head (the bytes before the
/// blank line). Only `\r\n` ends a line here; a bare `\n` or `\r`, a
/// repeated `Host` or `Origin`, or a line starting with a space or tab
/// marks the head malformed, so a header this parser didn't see can't
/// reach tungstenite.
pub fn read_upgrade(head: &str) -> Upgrade<'_> {
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let (method, path) = (first.next().unwrap_or_default(), first.next().unwrap_or_default());
    let mut header: HashMap<String, &str> = HashMap::new();
    let mut malformed = head.split("\r\n").any(|l| l.contains(['\r', '\n']));
    for line in lines {
        if line.starts_with([' ', '\t']) {
            malformed = true;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            if header.contains_key(&k) && (k == "host" || k == "origin") {
                malformed = true;
            }
            header.entry(k).or_insert(v.trim());
        }
    }
    let has = |k: &str, word: &str| header.get(k).is_some_and(|v| v.to_ascii_lowercase().contains(word));
    Upgrade {
        websocket: method == "GET" && has("upgrade", "websocket") && has("connection", "upgrade"),
        path,
        host: header.get("host").copied(),
        origin: header.get("origin").copied(),
        malformed,
    }
}

/// The browser an upgrade may drive, or the status to refuse it with:
/// 404 for anything but an upgrade (no `/json/*`, nothing for a page's
/// `fetch`), 403 for a malformed head (`read_upgrade`), any `Origin` (browsers always send one; agent-browser
/// doesn't), a `Host` other than `127.0.0.1:<port>`/`localhost:<port>`
/// (DNS rebinding), or a token that isn't live; 409 when the browser
/// already has a live connection.
pub fn check_upgrade(req: &Upgrade, port: u16, tokens: &Tokens, live: impl Fn(&str) -> bool) -> Result<String, u16> {
    if !req.websocket {
        return Err(404);
    }
    if req.malformed || req.origin.is_some() {
        return Err(403);
    }
    let host_ok = req.host.is_some_and(|h| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"));
    if !host_ok {
        return Err(403);
    }
    let browser = req.path.strip_prefix('/').and_then(|t| tokens.check(t)).ok_or(403u16)?;
    if live(browser) {
        return Err(409);
    }
    Ok(browser.to_owned())
}

// ---- the method allowlist ---------------------------------------------------------

/// CDP methods an agent may send (review § 2). CEF's in-process session is
/// a trusted one (local files, any target, the whole profile's storage), so
/// this is an allowlist: anything not named here is refused and never
/// reaches the page. The same list applies inside child sessions.
pub fn allowed_method(method: &str) -> bool {
    let Some((domain, name)) = method.split_once('.') else { return false };
    // `Domain.name`, letters and digits only: a name that differs from a
    // refused one by a space, a newline or another dot is refused too.
    let word = |w: &str| !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric());
    if !word(domain) || !word(name) {
        return false;
    }
    match domain {
        "Accessibility" | "DOMSnapshot" | "CSS" | "Overlay" | "Log" | "Inspector" | "Performance" | "PerformanceTimeline" | "Animation"
        | "LayerTree" | "Schema" | "WebMCP" => true,
        // evaluate/callFunctionOn are inherent: the agent runs script as the page.
        "Runtime" => name != "addBinding",
        "DOM" => !matches!(name, "setFileInputFiles" | "getFileInfo"),
        "Page" => matches!(
            name,
            "enable" | "disable" | "getFrameTree" | "getLayoutMetrics" | "getNavigationHistory" | "navigateToHistoryEntry" | "navigate"
                | "reload" | "stopLoading" | "captureScreenshot" | "printToPDF" | "startScreencast" | "stopScreencast"
                | "screencastFrameAck" | "handleJavaScriptDialog" | "createIsolatedWorld" | "setLifecycleEventsEnabled"
                | "bringToFront" | "setDocumentContent"
        ),
        "Input" => matches!(name, "dispatchMouseEvent" | "dispatchKeyEvent" | "insertText" | "dispatchTouchEvent" | "imeSetComposition"),
        "Emulation" => matches!(
            name,
            "setDeviceMetricsOverride" | "clearDeviceMetricsOverride" | "setEmulatedMedia" | "setUserAgentOverride" | "setLocaleOverride"
                | "setTimezoneOverride" | "setGeolocationOverride" | "clearGeolocationOverride" | "setFocusEmulationEnabled"
                | "setTouchEmulationEnabled"
        ),
        "Network" => matches!(
            name,
            "enable" | "disable" | "getResponseBody" | "getRequestPostData" | "setExtraHTTPHeaders" | "emulateNetworkConditions"
                | "setCacheDisabled" | "setUserAgentOverride"
        ),
        // The ones Herdr answers from the pane's tabs (never sent to CEF) and the frame ones CEF answers.
        "Target" => matches!(
            name,
            "setAutoAttach" | "detachFromTarget" | "getTargetInfo" | "setDiscoverTargets" | "getTargets" | "createTarget" | "attachToTarget"
                | "closeTarget" | "activateTarget"
        ),
        "Browser" => name == "getVersion",
        _ => false,
    }
}

/// While the page itself is outside the URL gate (a person or a popup put
/// it on `file:` or `chrome:`), only these work: the agent can move it back.
fn navigation(method: &str) -> bool {
    matches!(method, "Page.navigate" | "Page.navigateToHistoryEntry" | "Page.reload" | "Page.getFrameTree" | "Page.enable")
}

/// A tab of a browser pane, as a CDP page target.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabInfo {
    /// The tab's runtime id (`b1:3`), also its targetId.
    pub id: String,
    pub url: String,
    pub title: String,
    /// The tab's CEF browser exists, so its DevTools session takes messages.
    pub ready: bool,
}

/// The tabs one agent connection sees (a connection is for a whole browser
/// pane), and the one the person has shown.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pane {
    pub tabs: Vec<TabInfo>,
    pub shown: String,
}

impl Pane {
    fn tab(&self, id: &str) -> Option<&TabInfo> {
        self.tabs.iter().find(|t| t.id == id)
    }
}

/// What a connection asks of the window, which owns the tabs.
#[derive(Clone, Debug, PartialEq)]
pub enum TabAction {
    /// `Target.createTarget`: a tab after the shown one, shown.
    New { url: String },
    /// `Target.attachToTarget` on a tab whose page doesn't exist yet.
    Open { tab: String },
    /// `Target.activateTarget`, `Page.bringToFront`.
    Select { tab: String },
    /// `Target.closeTarget`.
    Close { tab: String },
}

/// A [`TabAction`] and the number its answer (`Link::done`) comes back under.
#[derive(Clone, Debug, PartialEq)]
pub struct TabOp {
    pub ticket: u64,
    pub action: TabAction,
}

/// What to do with one message from the agent.
#[derive(Debug, PartialEq)]
pub enum FromAgent {
    /// Send to `tab`'s page, under the proxy's own request id.
    Forward { id: i64, tab: String, message: String },
    /// Answer the agent directly: events first, the answer last.
    Reply(Vec<String>),
    /// The window changes the tabs; the answer follows (`Link::done`).
    Tab(TabOp),
}

/// Session ids Herdr hands out for tabs, unique in the app run so an id an
/// old connection held never names another tab.
static SESSION_NUMBER: AtomicU64 = AtomicU64::new(0);

/// A request waiting on a page: the agent's id and session, and the tab asked.
struct Pending {
    agent: Value,
    session: Option<String>,
    tab: String,
}

enum Then {
    /// A tab made for `createTarget`: answers with its id once its page exists.
    Created,
    /// `attachToTarget` on a tab whose page doesn't exist yet.
    Attached,
    /// A tab change: answers with this when the window has made it.
    Answer(Value),
}

struct Waiting {
    agent: Value,
    session: Option<String>,
    ticket: u64,
    tab: Option<String>,
    then: Then,
}

/// Where a message is addressed: the browser, or a session (a tab's own, or
/// one of its frames').
enum Route {
    Browser,
    Tab(String),
}

/// One agent connection's state. It is for one browser pane: Herdr is the
/// browser endpoint, with the pane's tabs as page targets. Sessions are
/// flat: a tab's session ids are Herdr's, stripped on the way to the tab's
/// DevTools session and put back on what comes from it; the sessions the page
/// itself auto-attaches (frames) keep CEF's ids and are mapped to their tab.
pub struct Link {
    pane: Pane,
    discover: bool,
    /// session id → tab id: the sessions this connection attached.
    sessions: HashMap<String, String>,
    /// CEF's child session id → (tab id, targetId), from `Target.attachedToTarget`.
    children: HashMap<String, (String, String)>,
    /// The proxy's request id → the request.
    pending: HashMap<i64, Pending>,
    waiting: Vec<Waiting>,
    /// Tabs this connection sent a message to: they reopen when it ends.
    touched: BTreeSet<String>,
    ticket: u64,
}

impl Link {
    pub fn new(pane: Pane) -> Link {
        Link {
            pane,
            discover: false,
            sessions: HashMap::new(),
            children: HashMap::new(),
            pending: HashMap::new(),
            waiting: Vec::new(),
            touched: BTreeSet::new(),
            ticket: 0,
        }
    }

    /// The tabs this connection sent messages to.
    pub fn touched(&self) -> Vec<String> {
        self.touched.iter().cloned().collect()
    }

    fn full(&self) -> bool {
        self.pending.len() + self.waiting.len() >= MAX_IN_FLIGHT
    }

    fn info(&self, tab: &TabInfo) -> Value {
        json!({
            "targetId": tab.id, "type": "page", "title": tab.title, "url": tab.url,
            "attached": self.sessions.values().any(|t| *t == tab.id), "canAccessOpener": false,
        })
    }

    /// The tab's url for the URL gate: the page's committed one (`url_of`),
    /// else the url the tab was made with.
    fn page_url(&self, tab: &str, url_of: &dyn Fn(&str) -> Option<String>) -> String {
        url_of(tab).filter(|u| !u.is_empty()).or_else(|| self.pane.tab(tab).map(|t| t.url.clone())).unwrap_or_default()
    }

    /// Checks one agent message against the allowlist and rewrites its id
    /// (`next` is the proxy's id counter). Target methods are answered here,
    /// from the pane's tabs, and never reach a page. `url_of`: a tab's
    /// committed main-frame url (the URL gate).
    pub fn from_agent(&mut self, text: &str, next: &mut i64, url_of: &dyn Fn(&str) -> Option<String>) -> FromAgent {
        let one = |text: String| FromAgent::Reply(vec![text]);
        let Ok(Value::Object(mut msg)) = serde_json::from_str::<Value>(text) else {
            return one(error(&Value::Null, None, -32700, "herdr: not a JSON object"));
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        // A session id is a string or absent. Anything else is refused here,
        // not left to Chromium's parser to read as the page's own session.
        let session = match msg.get("sessionId") {
            None => None,
            Some(Value::String(s)) => Some(s.clone()).filter(|s| !s.is_empty()),
            Some(_) => return one(error(&id, None, -32600, "herdr: sessionId must be a string")),
        };
        let refuse = |m: &str| one(error(&id, session.as_deref(), -32601, m));
        let Some(method) = msg.get("method").and_then(Value::as_str).map(str::to_owned) else {
            return refuse("herdr: a request needs an id and a method");
        };
        if !(id.is_i64() || id.is_u64()) {
            return refuse("herdr: a request needs an id and a method");
        }
        let route = match session.as_deref() {
            // Detaching a frame's session may be asked at the root: it goes to the frame's tab.
            None if method == "Target.detachFromTarget" => match msg.get("params").and_then(|p| p.get("sessionId")).and_then(Value::as_str).and_then(|s| self.children.get(s)) {
                Some((tab, _)) => Route::Tab(tab.clone()),
                None => Route::Browser,
            },
            None => Route::Browser,
            Some(s) => match self.sessions.get(s).or_else(|| self.children.get(s).map(|(tab, _)| tab)) {
                Some(tab) => Route::Tab(tab.clone()),
                None => return refuse(&format!("herdr: unknown session {s}")),
            },
        };
        if !allowed_method(&method) {
            return refuse(&format!("herdr: {method} is not allowed for agents"));
        }
        // Every check below reads (and setAutoAttach writes) params as an
        // object: anything else is an error answer, never a panic.
        match msg.get("params") {
            None | Some(Value::Null) => {
                msg.insert("params".into(), json!({}));
            }
            Some(Value::Object(_)) => {}
            Some(_) => return one(error(&id, session.as_deref(), -32602, "herdr: params must be an object")),
        }
        let params = msg.entry("params").or_insert_with(|| json!({}));
        let asked = Asked { id: &id, session: session.as_deref() };
        if let Some(answer) = self.target_method(&method, &route, &asked, params) {
            return answer;
        }
        let tab = match &route {
            Route::Tab(tab) => tab.clone(),
            // The one page-less method that goes to a page: Chromium's version.
            Route::Browser if method == "Browser.getVersion" => {
                let ready = self.pane.tabs.iter().filter(|t| t.ready);
                match ready.clone().find(|t| t.id == self.pane.shown).or_else(|| ready.clone().next()) {
                    Some(t) => t.id.clone(),
                    None => return refuse("herdr: no page is open in this browser yet"),
                }
            }
            Route::Browser => {
                return refuse(&format!("herdr: {method} needs a session: attach a tab first (Target.attachToTarget with flatten: true)"));
            }
        };
        if matches!(route, Route::Tab(_)) {
            let page_url = self.page_url(&tab, url_of);
            if !allowed_url(&page_url) && !navigation(&method) {
                let scheme = page_url.split(':').next().unwrap_or_default();
                return refuse(&format!("herdr: the page is at {scheme}:, not an http, https or about:blank page: navigate it first"));
            }
        }
        if let Err(m) = self.check_params(&method, params, &tab) {
            return refuse(&m);
        }
        if self.full() {
            return refuse("herdr: too many requests in flight");
        }
        if matches!(route, Route::Tab(_)) {
            self.touched.insert(tab.clone());
        }
        *next += 1;
        self.pending.insert(*next, Pending { agent: id, session: session.clone(), tab: tab.clone() });
        msg.remove("sessionId");
        // A frame's session is CEF's own: it stays on the message.
        if let Some(s) = session.filter(|s| self.children.contains_key(s)) {
            msg.insert("sessionId".into(), json!(s));
        }
        msg.insert("id".into(), json!(*next));
        FromAgent::Forward { id: *next, tab, message: Value::Object(msg).to_string() }
    }

    /// The Target domain and `Page.bringToFront`, from the pane's tabs. None:
    /// not one of these (or a frame's, which the page answers).
    fn target_method(&mut self, method: &str, route: &Route, asked: &Asked, params: &Value) -> Option<FromAgent> {
        let reply = |out: Vec<String>| Some(FromAgent::Reply(out));
        let refuse = |m: &str| reply(vec![asked.error(-32601, m)]);
        let target = params.get("targetId").and_then(Value::as_str);
        let own_tab = |link: &Link| target.filter(|t| link.pane.tab(t).is_some());
        let no_tab = || refuse(&format!("herdr: {} is not a tab of this browser", target.unwrap_or("(no targetId)")));
        match method {
            "Target.setDiscoverTargets" => {
                self.discover = params.get("discover").and_then(Value::as_bool).unwrap_or(false);
                let mut out: Vec<String> = Vec::new();
                if self.discover {
                    out.extend(self.pane.tabs.iter().map(|t| event("Target.targetCreated", json!({ "targetInfo": self.info(t) }), None)));
                }
                out.push(asked.answer(json!({})));
                reply(out)
            }
            "Target.getTargets" => {
                // The shown tab first: agent-browser starts on the first page that answers.
                let shown = self.pane.tabs.iter().filter(|t| t.id == self.pane.shown);
                let rest = self.pane.tabs.iter().filter(|t| t.id != self.pane.shown);
                let infos: Vec<Value> = shown.chain(rest).map(|t| self.info(t)).collect();
                reply(vec![asked.answer(json!({ "targetInfos": infos }))])
            }
            "Target.getTargetInfo" => {
                let tab = match (own_tab(self), route) {
                    (Some(t), _) => t,
                    (None, Route::Tab(t)) if target.is_none() && self.sessions.contains_key(asked.session.unwrap_or_default()) => t.as_str(),
                    // A frame of a tab goes to the page; at the root, only tabs are known.
                    (None, Route::Browser) => return no_tab(),
                    _ => return None,
                };
                let info = self.info(self.pane.tab(tab)?);
                reply(vec![asked.answer(json!({ "targetInfo": info }))])
            }
            "Target.createTarget" => {
                let url = params.get("url").and_then(Value::as_str).filter(|u| !u.is_empty()).unwrap_or(super::registry::BLANK);
                if !allowed_url(url) {
                    return refuse(&format!("herdr: only http, https and about:blank pages open, not {url}"));
                }
                self.ask(asked, None, Then::Created, TabAction::New { url: url.to_owned() })
            }
            "Target.attachToTarget" => {
                if params.get("flatten").and_then(Value::as_bool) != Some(true) {
                    return refuse("herdr: only flat sessions: attachToTarget needs flatten: true");
                }
                let Some(tab) = own_tab(self) else { return no_tab() };
                if self.pane.tab(tab)?.ready {
                    return reply(self.attach(tab, asked));
                }
                self.ask(asked, Some(tab), Then::Attached, TabAction::Open { tab: tab.to_owned() })
            }
            "Target.detachFromTarget" => {
                let session = params.get("sessionId").and_then(Value::as_str)?;
                if !self.sessions.contains_key(session) {
                    return None;
                }
                let mut out = self.detach(session);
                out.push(asked.answer(json!({})));
                reply(out)
            }
            "Target.closeTarget" => {
                let Some(tab) = own_tab(self) else { return no_tab() };
                self.ask(asked, Some(tab), Then::Answer(json!({ "success": true })), TabAction::Close { tab: tab.to_owned() })
            }
            "Target.activateTarget" => {
                let Some(tab) = own_tab(self) else { return no_tab() };
                self.ask(asked, Some(tab), Then::Answer(json!({})), TabAction::Select { tab: tab.to_owned() })
            }
            // Selects the tab in the pane; the app window is not raised.
            "Page.bringToFront" => {
                let Route::Tab(tab) = route else { return None };
                let tab = tab.clone();
                self.ask(asked, Some(&tab), Then::Answer(json!({})), TabAction::Select { tab: tab.clone() })
            }
            // agent-browser sends it at the browser only with network filtering or proxy auth.
            "Target.setAutoAttach" if matches!(route, Route::Browser) => {
                refuse("herdr: browser-level Target.setAutoAttach is not supported: Herdr's browser endpoint has no auto-attach for the whole browser")
            }
            _ => None,
        }
    }

    /// Asks the window for a tab change; the answer waits for `done`.
    fn ask(&mut self, asked: &Asked, tab: Option<&str>, then: Then, action: TabAction) -> Option<FromAgent> {
        if self.full() {
            return Some(FromAgent::Reply(vec![asked.error(-32601, "herdr: too many requests in flight")]));
        }
        self.ticket += 1;
        let ticket = self.ticket;
        self.waiting.push(Waiting { agent: asked.id.clone(), session: asked.session.map(str::to_owned), ticket, tab: tab.map(str::to_owned), then });
        Some(FromAgent::Tab(TabOp { ticket, action }))
    }

    /// A new session on `tab`: `attachedToTarget`, then the answer.
    fn attach(&mut self, tab: &str, asked: &Asked) -> Vec<String> {
        let session = format!("HERDR{:027}", SESSION_NUMBER.fetch_add(1, Ordering::Relaxed) + 1);
        self.sessions.insert(session.clone(), tab.to_owned());
        self.touched.insert(tab.to_owned());
        let info = self.pane.tab(tab).map(|t| self.info(t)).unwrap_or_default();
        vec![
            event("Target.attachedToTarget", json!({ "sessionId": session, "targetInfo": info, "waitingForDebugger": false }), None),
            asked.answer(json!({ "sessionId": session })),
        ]
    }

    /// Ends a session of a tab. With its last one the tab's frame sessions go too.
    fn detach(&mut self, session: &str) -> Vec<String> {
        let Some(tab) = self.sessions.remove(session) else { return Vec::new() };
        let mut out = Vec::new();
        if !self.sessions.values().any(|t| *t == tab) {
            let frames: Vec<(String, String)> = self.children.iter().filter(|(_, (t, _))| *t == tab).map(|(s, (_, target))| (s.clone(), target.clone())).collect();
            for (frame, target) in frames {
                self.children.remove(&frame);
                out.push(event("Target.detachedFromTarget", json!({ "sessionId": frame, "targetId": target }), Some(session)));
            }
        }
        out.push(event("Target.detachedFromTarget", json!({ "sessionId": session, "targetId": tab }), None));
        out
    }

    /// The tab closed: its sessions end, and what was asked of it fails.
    fn gone(&mut self, tab: &str) -> Vec<String> {
        let mut out = Vec::new();
        let sessions: Vec<String> = self.sessions.iter().filter(|(_, t)| *t == tab).map(|(s, _)| s.clone()).collect();
        for s in sessions {
            out.extend(self.detach(&s));
        }
        let lost: Vec<i64> = self.pending.iter().filter(|(_, p)| p.tab == tab).map(|(id, _)| *id).collect();
        for id in lost {
            out.extend(self.fail(id, "herdr: the tab closed"));
        }
        // A tab change in flight is answered by `done`, closing included.
        let (lost, kept): (Vec<Waiting>, Vec<Waiting>) =
            std::mem::take(&mut self.waiting).into_iter().partition(|w| w.tab.as_deref() == Some(tab) && !matches!(w.then, Then::Answer(_)));
        self.waiting = kept;
        out.extend(lost.into_iter().map(|w| error(&w.agent, w.session.as_deref(), -32000, "herdr: the tab closed")));
        out
    }

    /// The window changed the tabs for `ticket` (`Ok(tab id)`), or couldn't.
    pub fn done(&mut self, ticket: u64, result: Result<String, String>) -> Vec<String> {
        let Some(at) = self.waiting.iter().position(|w| w.ticket == ticket) else { return Vec::new() };
        match result {
            Err(e) => {
                let w = self.waiting.remove(at);
                vec![error(&w.agent, w.session.as_deref(), -32000, &e)]
            }
            Ok(tab) => {
                if let Then::Answer(answer) = &self.waiting[at].then {
                    let (answer, w) = (answer.clone(), self.waiting.remove(at));
                    return vec![answer_to(&w.agent, w.session.as_deref(), answer)];
                }
                self.waiting[at].tab.get_or_insert(tab);
                self.release()
            }
        }
    }

    /// Answers what waited for a page that exists now.
    fn release(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.waiting.len() {
            let ready = self.waiting[i].tab.as_deref().and_then(|t| self.pane.tab(t)).is_some_and(|t| t.ready);
            if !ready || matches!(self.waiting[i].then, Then::Answer(_)) {
                i += 1;
                continue;
            }
            let w = self.waiting.remove(i);
            let tab = w.tab.clone().unwrap_or_default();
            match w.then {
                Then::Created => out.push(answer_to(&w.agent, w.session.as_deref(), json!({ "targetId": tab }))),
                _ => out.extend(self.attach(&tab, &Asked { id: &w.agent, session: w.session.as_deref() })),
            }
        }
        out
    }

    /// The pane's tabs changed (a tab opened, closed, moved, its page said
    /// something new, its page now exists): what the agent hears of it.
    pub fn sync(&mut self, pane: Pane) -> Vec<String> {
        let old = std::mem::replace(&mut self.pane, pane);
        let mut out = Vec::new();
        let removed: Vec<String> = old.tabs.iter().filter(|t| self.pane.tab(&t.id).is_none()).map(|t| t.id.clone()).collect();
        for id in removed {
            out.extend(self.gone(&id));
            if self.discover {
                out.push(event("Target.targetDestroyed", json!({ "targetId": id }), None));
            }
        }
        if self.discover {
            for t in &self.pane.tabs {
                match old.tab(&t.id) {
                    None => out.push(event("Target.targetCreated", json!({ "targetInfo": self.info(t) }), None)),
                    Some(o) if (&o.url, &o.title) != (&t.url, &t.title) => out.push(event("Target.targetInfoChanged", json!({ "targetInfo": self.info(t) }), None)),
                    Some(_) => {}
                }
            }
        }
        out.extend(self.release());
        out
    }

    fn check_params(&self, method: &str, params: &mut Value, tab: &str) -> Result<(), String> {
        let s = |k: &str| params.get(k).and_then(Value::as_str);
        match method {
            "Page.navigate" => {
                let url = s("url").unwrap_or_default();
                if !allowed_url(url) {
                    return Err(format!("herdr: only http, https and about:blank pages open, not {url}"));
                }
            }
            "Page.printToPDF" if s("transferMode").is_some_and(|m| m != "ReturnAsBase64") => {
                return Err("herdr: printToPDF only returns base64".into());
            }
            // `paste` pulls the person's clipboard into the page.
            "Input.dispatchKeyEvent" if params.get("commands").and_then(Value::as_array).is_some_and(|c| !c.is_empty()) => {
                return Err("herdr: key commands (paste, ...) are not allowed for agents".into());
            }
            "Target.setAutoAttach" => params["flatten"] = json!(true),
            "Target.detachFromTarget" if !s("sessionId").is_some_and(|x| self.children.contains_key(x)) => {
                return Err("herdr: detachFromTarget needs a session this connection attached".into());
            }
            // The tabs were answered before; what is left is a frame of this tab.
            "Target.getTargetInfo" if !s("targetId").is_none_or(|t| self.children.values().any(|(owner, target)| owner == tab && target == t)) => {
                return Err("herdr: getTargetInfo only for this browser's tabs and their own frames".into());
            }
            _ => {}
        }
        Ok(())
    }

    /// A forwarded request the page never got (it was reopening): the
    /// agent's error answer, or None when it's no longer in flight.
    pub fn fail(&mut self, id: i64, message: &str) -> Option<String> {
        let p = self.pending.remove(&id)?;
        Some(error(&p.agent, p.session.as_deref(), -32000, message))
    }

    /// One message from `tab`'s page: an answer goes back under the agent's
    /// own id and session; an event goes out on each session of the tab, or
    /// on its own frame session, with cookies stripped. Empty: drop it.
    pub fn from_page(&mut self, tab: &str, bytes: &[u8]) -> Vec<String> {
        self.page_message(tab, bytes).unwrap_or_default()
    }

    fn page_message(&mut self, tab: &str, bytes: &[u8]) -> Option<Vec<String>> {
        let Ok(Value::Object(mut msg)) = serde_json::from_slice::<Value>(bytes) else { return None };
        if let Some(id) = msg.get("id").and_then(Value::as_i64) {
            let p = self.pending.remove(&id)?;
            msg.insert("id".into(), p.agent);
            match p.session {
                Some(s) => msg.insert("sessionId".into(), json!(s)),
                None => msg.remove("sessionId"),
            };
            return Some(vec![Value::Object(msg).to_string()]);
        }
        let method = msg.get("method").and_then(Value::as_str)?.to_owned();
        let frame = msg.get("sessionId").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned);
        if let Some(s) = &frame
            && !self.children.get(s).is_some_and(|(owner, _)| owner == tab)
        {
            return None;
        }
        let params = msg.get_mut("params").and_then(Value::as_object_mut);
        match method.as_str() {
            "Target.attachedToTarget" => {
                let p = params?;
                let session = p.get("sessionId").and_then(Value::as_str)?.to_owned();
                let target = p.get("targetInfo").and_then(|t| t.get("targetId")).and_then(Value::as_str).unwrap_or_default().to_owned();
                self.children.insert(session, (tab.to_owned(), target));
            }
            "Target.detachedFromTarget" => {
                let session = params?.get("sessionId").and_then(Value::as_str)?.to_owned();
                self.children.remove(&session)?;
            }
            "Target.targetInfoChanged" | "Target.targetCreated" | "Target.targetDestroyed" | "Target.targetCrashed" => {
                let p = params?;
                let target = p.get("targetInfo").and_then(|t| t.get("targetId")).or_else(|| p.get("targetId")).and_then(Value::as_str)?;
                if !self.children.values().any(|(owner, t)| owner == tab && t == target) {
                    return None;
                }
            }
            m if m.starts_with("Target.") => return None,
            m if m.starts_with("Network.") => {
                if let Some(p) = params {
                    strip_cookies(p);
                }
            }
            _ => {}
        }
        if frame.is_some() {
            return Some(vec![Value::Object(msg).to_string()]);
        }
        // A tab's own events carry no session: each session on it hears them.
        let heard: Vec<String> = self.sessions.iter().filter(|(_, t)| *t == tab).map(|(s, _)| s.clone()).collect();
        Some(
            heard
                .into_iter()
                .map(|s| {
                    let mut m = msg.clone();
                    m.insert("sessionId".into(), json!(s));
                    Value::Object(m).to_string()
                })
                .collect(),
        )
    }
}

/// The request being answered: its id, and the session it came on.
struct Asked<'a> {
    id: &'a Value,
    session: Option<&'a str>,
}

impl Asked<'_> {
    fn answer(&self, result: Value) -> String {
        answer_to(self.id, self.session, result)
    }

    fn error(&self, code: i64, message: &str) -> String {
        error(self.id, self.session, code, message)
    }
}

fn answer_to(id: &Value, session: Option<&str>, result: Value) -> String {
    let mut out = json!({ "id": id, "result": result });
    if let Some(s) = session {
        out["sessionId"] = json!(s);
    }
    out.to_string()
}

/// An event, on `session` (None: the browser's own).
fn event(method: &str, params: Value, session: Option<&str>) -> String {
    let mut out = json!({ "method": method, "params": params });
    if let Some(s) = session {
        out["sessionId"] = json!(s);
    }
    out.to_string()
}

/// Network events carry `Cookie`/`Set-Cookie` values, HttpOnly ones
/// included, and `Authorization`/`Proxy-Authorization` the network stack
/// adds from its HTTP-auth cache (page script can't read those): none of
/// them reach the agent.
fn strip_cookies(p: &mut Map<String, Value>) {
    for key in ["associatedCookies", "blockedCookies", "exemptedCookies", "headersText"] {
        p.remove(key);
    }
    let clean = |h: Option<&mut Value>| {
        if let Some(Value::Object(h)) = h {
            h.retain(|k, _| !["cookie", "set-cookie", "authorization", "proxy-authorization"].iter().any(|s| k.eq_ignore_ascii_case(s)));
        }
    };
    clean(p.get_mut("headers"));
    for part in ["request", "response", "redirectResponse"] {
        if let Some(Value::Object(r)) = p.get_mut(part) {
            r.remove("headersText");
            r.remove("requestHeadersText");
            clean(r.get_mut("headers"));
            clean(r.get_mut("requestHeaders"));
        }
    }
}

fn error(id: &Value, session: Option<&str>, code: i64, message: &str) -> String {
    let mut out = json!({ "id": id, "error": { "code": code, "message": message } });
    if let Some(s) = session {
        out["sessionId"] = json!(s);
    }
    out.to_string()
}

// ---- the provider (agent-browser's plugin) ------------------------------------------

/// agent-browser's daemon idle exit for Herdr's terminals, in ms (its own
/// default is 60 minutes). While the daemon lives, it runs its own stream
/// server: a loopback websocket with no token that relays input to the page
/// it drives (upstream code; there is no switch to turn it off). A daemon
/// whose agent stopped sending commands exits after this, which ends its
/// connection to Herdr and so its token; the next command starts a new one.
/// Input sent through that stream server also keeps the daemon alive.
pub const IDLE_TIMEOUT_MS: &str = "300000";

/// The environment `run()` gives local terminals: agent-browser uses Herdr
/// as its browser provider. Herdr's plugin entry comes first and once,
/// before any list the app inherited; the socket is pinned in its args, so
/// the provider never falls back to another app home.
pub fn plugin_env(exe: &str, socket: &str, inherited: Option<&str>, session: &str) -> Vec<(String, String)> {
    let mut plugins = vec![json!({ "name": PLUGIN, "command": exe, "args": [VERB, "--socket", socket], "capabilities": ["browser.provider", CHECK] })];
    let others = inherited.and_then(|raw| serde_json::from_str::<Vec<Value>>(raw).ok()).unwrap_or_default();
    plugins.extend(others.into_iter().filter(|p| p.is_object() && p.get("name").and_then(Value::as_str) != Some(PLUGIN)));
    vec![
        ("AGENT_BROWSER_PROVIDER".into(), PLUGIN.into()),
        ("AGENT_BROWSER_PLUGINS".into(), Value::Array(plugins).to_string()),
        ("AGENT_BROWSER_SESSION".into(), session.into()),
        ("AGENT_BROWSER_IDLE_TIMEOUT_MS".into(), IDLE_TIMEOUT_MS.into()),
    ]
}

/// agent-browser's user config with Herdr's plugin entry in it: any earlier
/// `herdr` entry replaced, every other key and plugin kept. Refuses a file
/// that isn't a JSON object, rather than overwrite it.
pub fn with_herdr_plugin(existing: &str, exe: &str, socket: &str) -> Result<String, String> {
    let mut config: Value = if existing.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(existing).map_err(|e| format!("not valid JSON ({e}); fix or move it, then run this again"))?
    };
    let Some(object) = config.as_object_mut() else { return Err("not a JSON object; fix or move it, then run this again".into()) };
    let mut plugins = match object.remove("plugins") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(list)) => list,
        Some(_) => return Err("\"plugins\" is not a list; fix or move it, then run this again".into()),
    };
    plugins.retain(|p| p.get("name").and_then(Value::as_str) != Some(PLUGIN));
    plugins.push(json!({ "name": PLUGIN, "command": exe, "args": [VERB, "--socket", socket], "capabilities": ["browser.provider", CHECK] }));
    object.insert("plugins".into(), Value::Array(plugins));
    serde_json::to_string_pretty(&config).map(|s| s + "\n").map_err(|e| e.to_string())
}

/// The provider's answer to one agent-browser request. `attach` asks the
/// window (`{ attach: { terminal, session, browser } }`); it is never
/// called outside a Herdr terminal. Panes are never created here.
pub fn respond(request: &Value, env: &dyn Fn(&str) -> Option<String>, attach: impl FnOnce(Value) -> Result<Value, String>) -> Value {
    let fail = |e: String| json!({ "protocol": PROTOCOL, "success": false, "error": e });
    if request.get("protocol").and_then(Value::as_str) != Some(PROTOCOL) {
        return fail("unsupported plugin protocol".into());
    }
    match request.get("type").and_then(Value::as_str) {
        Some("plugin.manifest") => json!({ "protocol": PROTOCOL, "success": true, "manifest": {
            "name": PLUGIN, "capabilities": ["browser.provider", CHECK],
            "description": "Drive the Herdr browser pane beside this terminal" } }),
        Some("browser.close") => json!({ "protocol": PROTOCOL, "success": true, "data": {} }),
        // The same checks as a launch, with no token issued.
        Some(CHECK) => {
            let Some(terminal) = env("HC_TERMINAL_ID").filter(|t| !t.is_empty()) else { return fail(not_in_herdr()) };
            let browser = env("HERDR_BROWSER").filter(|b| !b.is_empty());
            match attach(json!({ "terminal": terminal, "session": null, "browser": browser, "check": true })) {
                Ok(r) => json!({ "protocol": PROTOCOL, "success": true, "data": { "browser": r["browser"], "selection": r["selection"], "ready": true } }),
                Err(e) => fail(e),
            }
        }
        Some("browser.launch") => {
            let Some(terminal) = env("HC_TERMINAL_ID").filter(|t| !t.is_empty()) else { return fail(not_in_herdr()) };
            let session = request.pointer("/request/session").cloned().unwrap_or(Value::Null);
            // A terminal Herdr didn't give a session of its own (one that
            // started before this Herdr) uses one named for itself, so its
            // agent-browser daemon, whose environment this provider runs
            // in, is never one another terminal started.
            if env("AGENT_BROWSER_SESSION").is_none_or(|s| s.is_empty()) && session.as_str() != Some(own_session(&terminal).as_str()) {
                return fail(needs_own_session(&terminal));
            }
            let browser = env("HERDR_BROWSER").filter(|b| !b.is_empty());
            match attach(json!({ "terminal": terminal, "session": session, "browser": browser })) {
                Ok(r) => json!({ "protocol": PROTOCOL, "success": true, "browser": {
                    "cdpUrl": r["cdpUrl"], "directPage": false,
                    "metadata": { "space": r["space"], "browser": r["browser"], "pane": r["pane"], "selection": r["selection"] } } }),
                Err(e) => fail(e),
            }
        }
        Some(other) => fail(format!("unsupported plugin request type {other:?}")),
        None => fail("plugin request has no type".into()),
    }
}

/// The id of the core this process's environment points at (`hc` picks it
/// from `XDG_STATE_HOME` and `HC_SESSION`, as the terminal's own `hc` does).
/// None when `hc` doesn't answer within 3 s.
#[cfg(unix)]
fn caller_core() -> Option<String> {
    use std::io::{BufRead, BufReader, Write};
    let mut child = crate::spawn::command(crate::core_bin::path())
        .arg("connect")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = stdin.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"hello\",\"params\":{}}\n");
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(std::time::Duration::from_secs(3)).ok();
    let _ = child.kill();
    let _ = child.wait();
    super::core_id(&serde_json::from_str(&line?).ok()?)
}

/// `herdr-app __agent-browser-provider --socket <path>`: one request on
/// stdin (at most 1 MiB), one answer on stdout. Never starts CEF or a window.
#[cfg(unix)]
pub fn provider_main(args: &[String]) -> i32 {
    use std::io::{BufRead, BufReader, Read, Write};
    let socket = match args {
        [flag, path] if flag == "--socket" => path.clone(),
        _ => crate::mods::app_dir().join("herdr-app.sock").to_string_lossy().into_owned(),
    };
    let mut bytes = Vec::new();
    let read = std::io::stdin().lock().take(MAX_FRAME as u64 + 1).read_to_end(&mut bytes);
    let request = match read {
        Ok(_) if bytes.len() > MAX_FRAME => Err("plugin request exceeds 1 MiB".to_owned()),
        Ok(_) => serde_json::from_slice::<Value>(&bytes).map_err(|_| "plugin request is not valid JSON".to_owned()),
        Err(e) => Err(format!("cannot read the plugin request: {e}")),
    };
    let answer = match request {
        Ok(request) => respond(&request, &|k| std::env::var(k).ok(), |mut attach| {
            // Terminal ids are per core; the app compares this with its own.
            attach["core"] = caller_core().map_or(Value::Null, Value::String);
            let mut stream = std::os::unix::net::UnixStream::connect(&socket).map_err(|_| no_app(&socket))?;
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
            stream.write_all(format!("{}\n", json!({ "attach": attach })).as_bytes()).map_err(|_| no_app(&socket))?;
            let mut line = String::new();
            BufReader::new(&stream).take(MAX_FRAME as u64).read_line(&mut line).map_err(|e| format!("Herdr did not answer: {e}"))?;
            let reply: Value = serde_json::from_str(&line).map_err(|_| "Herdr's answer is not JSON".to_owned())?;
            match reply.get("error").and_then(Value::as_str) {
                Some(e) => Err(e.to_owned()),
                None => Ok(reply),
            }
        }),
        Err(e) => json!({ "protocol": PROTOCOL, "success": false, "error": e }),
    };
    let mut out = std::io::stdout().lock();
    if serde_json::to_writer(&mut out, &answer).is_err() || out.flush().is_err() {
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::{parked, remote};
    use serde_json::{Value, json};

    // ---- one URL gate

    #[test]
    fn only_http_https_and_about_blank_pass_the_url_gate() {
        for ok in ["http://127.0.0.1:8765/browser-fixture.html", "https://example.com/", "HTTPS://EXAMPLE.COM", "about:blank"] {
            assert!(allowed_url(ok), "{ok}");
        }
        for bad in [
            "file:///etc/hosts", "chrome://version", "chrome-untrusted://x", "devtools://devtools/x", "view-source:https://a/",
            "javascript:alert(1)", "data:text/html,hi", "blob:https://a/x", "filesystem:https://a/x", "about:blank#blocked",
            "about:version", " http://a/", "http:/a", "mailto:x@y", "",
        ] {
            assert!(!allowed_url(bad), "{bad}");
        }
    }

    #[test]
    fn logged_urls_keep_scheme_host_and_path_only() {
        assert_eq!(loggable("https://user:pw@example.com/a/b?code=SECRET#frag"), "https://example.com/a/b");
        assert_eq!(loggable("http://127.0.0.1:8765/x.html"), "http://127.0.0.1:8765/x.html");
        assert_eq!(loggable("about:blank"), "about:blank");
        assert_eq!(loggable("http://a/x\nFORGED-LINE browser: agent access off"), "http://a/x\\nFORGED-LINE browser: agent access off");
        // Cut before escaping: an escape is never split at the cut.
        let long = format!("http://a.test/{}\u{202e}\u{202e}", "x".repeat(61));
        assert_eq!(long.chars().count(), 77);
        assert_eq!(loggable(&format!("{long}tail")), format!("http://a.test/{}\\u{{202e}}\\u{{202e}}…", "x".repeat(61)));
        let short = format!("http://a.test/{}\n", "x".repeat(60));
        assert_eq!(loggable(&short), format!("http://a.test/{}\\n", "x".repeat(60)));
        assert_eq!(public_url("https://a/b?token=1#x"), "https://a/b");
        assert_eq!(public_url("https://a/b"), "https://a/b");
    }

    #[test]
    fn popups_pass_the_gate_and_cross_origin_frames_need_a_gesture() {
        let page = "https://a.example/x";
        assert!(popup_allowed("https://b.example/", page, page, true, false));
        for bad in ["file:///etc/hosts", "chrome://version", "javascript:alert(1)", "data:text/html,x"] {
            assert!(!popup_allowed(bad, page, page, true, true), "{bad}");
        }
        // An iframe from another origin moving the top page: only on a gesture.
        assert!(!popup_allowed("https://b.example/", "https://ads.example/f", page, false, false));
        assert!(popup_allowed("https://b.example/", "https://ads.example/f", page, false, true));
        assert!(popup_allowed("https://b.example/", "https://a.example/frame", page, false, false));
    }

    #[test]
    fn remote_debugging_switches_are_found_in_any_spelling() {
        assert_eq!(forbidden_switch(["herdr-app", "--remote-debugging-port=9222"]), Some("remote-debugging-port"));
        assert_eq!(forbidden_switch(["-remote-debugging-pipe"]), Some("remote-debugging-pipe"));
        assert_eq!(forbidden_switch(["--REMOTE-ALLOW-ORIGINS=*"]), Some("remote-allow-origins"));
        assert_eq!(forbidden_switch(["--remote-debugging-address", "0.0.0.0"]), Some("remote-debugging-address"));
        assert_eq!(forbidden_switch(["herdr-app", "remote-debugging-port", "--use-mock-keychain"]), None);
    }

    // ---- selecting the caller's browser

    fn cand(browser: &'static str, tab: Option<&'static str>, focused_at: u64) -> Candidate<'static> {
        Candidate { browser, space: Some("sp1"), tab, focused_at }
    }

    #[test]
    fn the_callers_tab_wins_then_the_most_recently_focused_in_the_space() {
        let caller = Caller { space: "sp1", tab: "tb1", wanted: None };
        let all = [cand("b1", Some("tb2"), 50), cand("b2", Some("tb1"), 0)];
        assert_eq!(select(&caller, &all), Ok(("b2", "tab")));
        let all = [cand("b1", Some("tb2"), 50), cand("b3", Some("tb3"), 90)];
        assert_eq!(select(&caller, &all), Ok(("b3", "recent")));
        let all = [cand("b1", Some("tb2"), 0)];
        assert_eq!(select(&caller, &all), Ok(("b1", "space")));
        // Another space's browser is never picked.
        let other = Candidate { space: Some("sp2"), ..cand("b9", Some("tbx"), 99) };
        assert_eq!(select(&caller, &[other]).unwrap_err(), no_browser());
    }

    #[test]
    fn two_browsers_with_no_way_to_choose_ask_for_herdr_browser() {
        let caller = Caller { space: "sp1", tab: "tb1", wanted: None };
        let all = [cand("b1", Some("tb1"), 0), cand("b2", Some("tb1"), 0)];
        assert_eq!(select(&caller, &all).unwrap_err(), ambiguous(&["b1", "b2"]));
        let all = [cand("b1", Some("tb2"), 0), cand("b2", Some("tb3"), 0)];
        assert_eq!(select(&caller, &all).unwrap_err(), ambiguous(&["b1", "b2"]));
    }

    #[test]
    fn herdr_browser_overrides_within_the_callers_space_only() {
        let all = [cand("b1", Some("tb1"), 0), cand("b2", Some("tb2"), 0), Candidate { space: Some("sp2"), ..cand("b3", Some("tb9"), 0) }];
        let caller = Caller { space: "sp1", tab: "tb1", wanted: Some("b2") };
        assert_eq!(select(&caller, &all), Ok(("b2", "HERDR_BROWSER")));
        let caller = Caller { space: "sp1", tab: "tb1", wanted: Some("b3") };
        assert_eq!(select(&caller, &all).unwrap_err(), "HERDR_BROWSER=b3 is not a browser in this terminal's space: see `herdr-app ls`");
        // A layout block shows on every space: the override may name it.
        let layout = Candidate { space: None, tab: None, ..cand("side", None, 0) };
        let caller = Caller { space: "sp1", tab: "tb1", wanted: Some("side") };
        assert_eq!(select(&caller, &[layout]), Ok(("side", "HERDR_BROWSER")));
    }

    #[test]
    fn every_error_an_agent_can_see_is_pinned() {
        assert_eq!(not_in_herdr(), "this terminal is not in Herdr (HC_TERMINAL_ID is unset): agent-browser can only drive a Herdr browser pane from a Herdr terminal");
        assert_eq!(no_app("/h/herdr-app.sock"), "Herdr is not running (no socket at /h/herdr-app.sock)");
        assert_eq!(parked("t4"), "terminal t4 has no pane in Herdr (parked): give it one with herdr-app do terminal.keep --terminal t4");
        assert_eq!(remote("m1/t4"), "terminal m1/t4 runs on another machine: browser panes are only for this machine's terminals");
        assert_eq!(no_browser(), "no browser in this space: open one with: herdr-app do browser.open --url <url>");
        assert_eq!(
            ambiguous(&["b1", "b2"]),
            "two or more browsers here (b1, b2): run `export HERDR_BROWSER=<id> AGENT_BROWSER_SESSION=$AGENT_BROWSER_SESSION-<id>`, then agent-browser again (a running agent-browser session keeps the environment it started with)"
        );
    }

    // ---- tokens

    #[test]
    fn a_token_names_one_browser_until_revoked_or_replaced() {
        let mut t = Tokens::default();
        let a = t.issue("b1", [7; 32]);
        assert_eq!(a.len(), 64);
        assert_eq!(t.check(&a), Some("b1"));
        // Bad tokens: wrong bytes, wrong length, not hex.
        assert_eq!(t.check(&"0".repeat(64)), None);
        assert_eq!(t.check(&a[..63]), None);
        assert_eq!(t.check(&"zz".repeat(32)), None);
        // A new attach replaces the old token.
        let b = t.issue("b1", [9; 32]);
        assert_eq!(t.check(&a), None);
        assert_eq!(t.check(&b), Some("b1"));
        // Revoked.
        assert!(t.revoke("b1"));
        assert_eq!(t.check(&b), None);
        // Memory only: a fresh app (a restart) knows no token.
        let c = t.issue("b2", [3; 32]);
        assert_eq!(Tokens::default().check(&c), None);
    }

    // ---- the websocket upgrade

    fn up<'a>(path: &'a str, host: Option<&'a str>, origin: Option<&'a str>) -> Upgrade<'a> {
        Upgrade { websocket: true, path, host, origin, malformed: false }
    }

    #[test]
    fn the_upgrade_needs_loopback_host_no_origin_and_a_live_token() {
        let mut t = Tokens::default();
        let token = t.issue("b1", [5; 32]);
        let path = format!("/{token}");
        let none = |_: &str| false;
        assert_eq!(check_upgrade(&up(&path, Some("127.0.0.1:4000"), None), 4000, &t, none), Ok("b1".to_owned()));
        assert_eq!(check_upgrade(&up(&path, Some("localhost:4000"), None), 4000, &t, none), Ok("b1".to_owned()));
        // Any Origin, null included.
        for origin in ["https://evil.example", "null", ""] {
            assert_eq!(check_upgrade(&up(&path, Some("127.0.0.1:4000"), Some(origin)), 4000, &t, none), Err(403));
        }
        // DNS rebinding and other hosts or ports.
        for host in [Some("evil.example:4000"), Some("127.0.0.1:4001"), Some("127.0.0.1"), None] {
            assert_eq!(check_upgrade(&up(&path, host, None), 4000, &t, none), Err(403), "{host:?}");
        }
        // A bad token, a query-string token.
        assert_eq!(check_upgrade(&up(&format!("/{}", "a".repeat(64)), Some("127.0.0.1:4000"), None), 4000, &t, none), Err(403));
        assert_eq!(check_upgrade(&up(&format!("/?t={token}"), Some("127.0.0.1:4000"), None), 4000, &t, none), Err(403));
        // Not an upgrade (`/json/version`, a page's fetch): a bare 404.
        let plain = Upgrade { websocket: false, path: "/json/version", host: Some("127.0.0.1:4000"), origin: None, malformed: false };
        assert_eq!(check_upgrade(&plain, 4000, &t, none), Err(404));
        // One live connection per browser: a second upgrade gets 409.
        assert_eq!(check_upgrade(&up(&path, Some("127.0.0.1:4000"), None), 4000, &t, |b| b == "b1"), Err(409));
    }

    /// The head parser and tungstenite must read the same head: a second
    /// `Host` or `Origin`, or a header behind a bare `\n`, would be seen by
    /// tungstenite and not by the Host and Origin checks.
    #[test]
    fn a_head_that_two_parsers_could_read_differently_is_refused() {
        let mut t = Tokens::default();
        let token = t.issue("b9", [9; 32]);
        let none = |_: &str| false;
        let head = |extra: &str| format!("GET /{token} HTTP/1.1\r\nHost: 127.0.0.1:4000\r\nUpgrade: websocket\r\nConnection: Upgrade{extra}");
        let good = head("");
        assert!(!read_upgrade(&good).malformed);
        assert_eq!(check_upgrade(&read_upgrade(&good), 4000, &t, none), Ok("b9".to_owned()));
        for bad in [
            head("\r\nHost: evil.example"),
            head("\r\nhost: 127.0.0.1:4000"),
            head("\r\nOrigin: null\r\norigin: null"),
            head("\r\nX-A: 1\nOrigin: http://evil.example"),
            head("\r\nX-A: 1\rOrigin: http://evil.example"),
            head("\r\nX-A: 1\r\n Origin: http://evil.example"),
            format!("GET /{token} HTTP/1.1\nHost: 127.0.0.1:4000\r\nUpgrade: websocket\r\nConnection: Upgrade"),
        ] {
            let req = read_upgrade(&bad);
            assert!(req.malformed, "{bad:?}");
            assert_eq!(check_upgrade(&req, 4000, &t, none), Err(403), "{bad:?}");
        }
    }

    // ---- the method allowlist and the Target domain, from a pane's tabs

    const PAGE: &str = "http://127.0.0.1:8765/browser-fixture.html";

    fn tab(id: &str, url: &str, title: &str, ready: bool) -> TabInfo {
        TabInfo { id: id.into(), url: url.into(), title: title.into(), ready }
    }

    /// Three tabs, the second one shown, the third never opened.
    fn pane() -> Pane {
        Pane {
            tabs: vec![tab("b1:1", PAGE, "One", true), tab("b1:2", "https://example.com/", "Two", true), tab("b1:3", "", "", false)],
            shown: "b1:2".into(),
        }
    }

    fn none(_: &str) -> Option<String> {
        None
    }

    fn send(link: &mut Link, next: &mut i64, msg: Value) -> FromAgent {
        link.from_agent(&msg.to_string(), next, &none)
    }

    fn texts(r: FromAgent) -> Vec<Value> {
        let FromAgent::Reply(out) = r else { panic!("not a direct reply: {r:?}") };
        out.iter().map(|t| serde_json::from_str(t).unwrap()).collect()
    }

    fn refused(r: &FromAgent) -> Option<String> {
        match r {
            FromAgent::Reply(out) => serde_json::from_str::<Value>(out.last()?).ok()?["error"]["message"].as_str().map(str::to_owned),
            _ => None,
        }
    }

    fn link() -> Link {
        Link::new(pane())
    }

    /// Attaches `tab` (which must be ready) and returns the session id.
    fn attach(link: &mut Link, tab: &str) -> String {
        let mut next = 0;
        let out = texts(send(link, &mut next, json!({ "id": 1, "method": "Target.attachToTarget", "params": { "targetId": tab, "flatten": true } })));
        out.last().unwrap()["result"]["sessionId"].as_str().unwrap().to_owned()
    }

    fn page_event(link: &mut Link, tab: &str, ev: Value) -> Vec<Value> {
        link.from_page(tab, ev.to_string().as_bytes()).iter().map(|t| serde_json::from_str(t).unwrap()).collect()
    }

    #[test]
    fn what_agent_browser_sends_is_forwarded_to_the_tab_of_its_session() {
        let mut link = link();
        let session = attach(&mut link, "b1:1");
        let mut next = 0;
        for method in [
            "Page.enable", "Runtime.enable", "Runtime.runIfWaitingForDebugger", "Network.enable", "WebMCP.enable",
            "Accessibility.getFullAXTree", "DOM.getDocument", "DOM.resolveNode", "DOM.getBoxModel", "Runtime.evaluate",
            "Runtime.callFunctionOn", "Page.getFrameTree", "Page.captureScreenshot", "Page.createIsolatedWorld",
            "Input.dispatchMouseEvent", "Input.insertText", "Emulation.setDeviceMetricsOverride", "Browser.getVersion",
        ] {
            let r = send(&mut link, &mut next, json!({ "id": 1, "method": method, "params": {}, "sessionId": session }));
            let FromAgent::Forward { tab, message, .. } = r else { panic!("{method} was refused: {:?}", refused(&r)) };
            assert_eq!(tab, "b1:1");
            // The tab's page never hears Herdr's session id.
            assert!(serde_json::from_str::<Value>(&message).unwrap().get("sessionId").is_none(), "{method}");
        }
        // At the root, Browser.getVersion goes to the shown tab.
        let FromAgent::Forward { tab, .. } = send(&mut link, &mut next, json!({ "id": 2, "method": "Browser.getVersion" })) else { panic!() };
        assert_eq!(tab, "b1:2");
    }

    #[test]
    fn everything_unlisted_is_refused_with_herdrs_error() {
        let mut link = link();
        let session = attach(&mut link, "b1:1");
        let mut next = 0;
        for method in [
            "Target.attachToBrowserTarget", "Target.createBrowserContext", "Target.disposeBrowserContext", "Target.exposeDevToolsProtocol",
            "Browser.setDownloadBehavior", "Browser.grantPermissions", "Browser.close", "Network.getCookies", "Network.getAllCookies",
            "Storage.getCookies", "Network.setCookie", "DOM.setFileInputFiles", "DOM.getFileInfo", "Input.dispatchDragEvent",
            "Page.addScriptToEvaluateOnNewDocument", "Page.setDownloadBehavior", "Runtime.addBinding", "Fetch.enable",
            "DOMStorage.getDOMStorageItems", "IndexedDB.requestData", "CacheStorage.requestEntries", "ServiceWorker.enable",
            "Tracing.start", "Tethering.bind", "Security.setIgnoreCertificateErrors", "Debugger.enable", "IO.read", "Nope.nothing", "Page",
            "Runtime.addBinding ", "Runtime.evaluate\nFORGED x", "Runtime.", ".evaluate", "Runtime.evaluate.x", "Accessibility.enable\u{0}",
        ] {
            for session in [Some(session.as_str()), None] {
                let mut msg = json!({ "id": 7, "method": method });
                if let Some(s) = session {
                    msg["sessionId"] = json!(s);
                }
                let r = send(&mut link, &mut next, msg);
                assert_eq!(refused(&r), Some(format!("herdr: {method} is not allowed for agents")), "{method}");
                let reply = &texts(r)[0];
                assert_eq!(reply["id"], 7);
                assert_eq!(reply["error"]["code"], -32601);
                assert_eq!(reply.get("sessionId").and_then(Value::as_str), session);
            }
        }
    }

    #[test]
    fn a_message_at_the_root_needs_a_session_and_an_unknown_session_is_refused() {
        let mut link = link();
        let mut next = 0;
        let r = send(&mut link, &mut next, json!({ "id": 1, "method": "Runtime.evaluate", "params": { "expression": "1" } }));
        assert!(refused(&r).unwrap().starts_with("herdr: Runtime.evaluate needs a session"));
        let r = send(&mut link, &mut next, json!({ "id": 2, "method": "Runtime.evaluate", "sessionId": "S9" }));
        assert_eq!(refused(&r), Some("herdr: unknown session S9".to_owned()));
        // A session from another connection (or a stale one) is just as unknown.
        let mut other = Link::new(pane());
        let theirs = attach(&mut other, "b1:1");
        let r = send(&mut link, &mut next, json!({ "id": 3, "method": "Page.enable", "sessionId": theirs }));
        assert_eq!(refused(&r), Some(format!("herdr: unknown session {theirs}")));
        assert!(link.pending.is_empty());
    }

    #[test]
    fn discovery_lists_the_panes_tabs_shown_first_and_announces_them() {
        let mut link = link();
        let mut next = 0;
        let out = texts(send(&mut link, &mut next, json!({ "id": 1, "method": "Target.getTargets" })));
        let ids: Vec<&str> = out[0]["result"]["targetInfos"].as_array().unwrap().iter().map(|t| t["targetId"].as_str().unwrap()).collect();
        assert_eq!(ids, ["b1:2", "b1:1", "b1:3"]);
        let one = &out[0]["result"]["targetInfos"][1];
        assert_eq!((one["type"].as_str(), one["title"].as_str(), one["url"].as_str()), (Some("page"), Some("One"), Some(PAGE)));
        // Discovery on: a targetCreated per tab, then the answer.
        let out = texts(send(&mut link, &mut next, json!({ "id": 2, "method": "Target.setDiscoverTargets", "params": { "discover": true } })));
        assert_eq!(out.len(), 4);
        assert!(out[..3].iter().all(|e| e["method"] == "Target.targetCreated"));
        assert_eq!(out[0]["params"]["targetInfo"]["targetId"], "b1:1");
        assert_eq!(out[3], json!({ "id": 2, "result": {} }));
        // getTargetInfo of a tab.
        let out = texts(send(&mut link, &mut next, json!({ "id": 3, "method": "Target.getTargetInfo", "params": { "targetId": "b1:2" } })));
        assert_eq!(out[0]["result"]["targetInfo"]["title"], "Two");
    }

    #[test]
    fn a_target_of_another_pane_is_never_known() {
        let mut link = link();
        let mut next = 0;
        let session = attach(&mut link, "b1:1");
        for (method, params) in [
            ("Target.attachToTarget", json!({ "targetId": "b2:1", "flatten": true })),
            ("Target.closeTarget", json!({ "targetId": "b2:1" })),
            ("Target.activateTarget", json!({ "targetId": "b2:1" })),
            ("Target.getTargetInfo", json!({ "targetId": "b2:1" })),
            ("Target.getTargetInfo", json!({})),
            ("Target.closeTarget", json!({})),
        ] {
            let r = send(&mut link, &mut next, json!({ "id": 1, "method": method, "params": params }));
            assert!(refused(&r).unwrap().contains("is not a tab of this browser"), "{method} {params}: {:?}", refused(&r));
        }
        // A frame's targetId that no tab session reported is refused inside a session too.
        let r = send(&mut link, &mut next, json!({ "id": 2, "method": "Target.getTargetInfo", "params": { "targetId": "b2:1" }, "sessionId": session }));
        assert!(refused(&r).is_some());
        assert!(link.waiting.is_empty() && link.pending.is_empty());
    }

    #[test]
    fn attaching_gives_a_synthetic_flat_session_and_events_carry_it_back() {
        let mut link = link();
        let mut next = 0;
        let r = send(&mut link, &mut next, json!({ "id": 1, "method": "Target.attachToTarget", "params": { "targetId": "b1:1" } }));
        assert_eq!(refused(&r), Some("herdr: only flat sessions: attachToTarget needs flatten: true".to_owned()));
        let out = texts(send(&mut link, &mut next, json!({ "id": 2, "method": "Target.attachToTarget", "params": { "targetId": "b1:1", "flatten": true } })));
        assert_eq!(out[0]["method"], "Target.attachedToTarget");
        assert_eq!(out[0]["params"]["targetInfo"]["targetId"], "b1:1");
        let session = out[1]["result"]["sessionId"].as_str().unwrap().to_owned();
        assert_eq!(out[0]["params"]["sessionId"], session.as_str());
        assert_eq!(out[1]["id"], 2);
        // Another session on the same tab is a different id; both hear its events.
        let second = attach(&mut link, "b1:1");
        assert_ne!(second, session);
        let heard = page_event(&mut link, "b1:1", json!({ "method": "Runtime.consoleAPICalled", "params": { "type": "log" } }));
        let mut sessions: Vec<&str> = heard.iter().map(|e| e["sessionId"].as_str().unwrap()).collect();
        sessions.sort();
        let mut want = vec![session.as_str(), second.as_str()];
        want.sort();
        assert_eq!(sessions, want);
        // A tab nobody attached drops its events; so does a tab of another pane.
        assert!(page_event(&mut link, "b1:2", json!({ "method": "Page.loadEventFired", "params": {} })).is_empty());
        // Answers go back under the agent's id and session, matched by the proxy's id.
        let FromAgent::Forward { id, tab, .. } = send(&mut link, &mut next, json!({ "id": 7, "method": "Page.reload", "sessionId": session })) else { panic!() };
        assert_eq!(tab, "b1:1");
        let back = page_event(&mut link, "b1:1", json!({ "id": id, "result": {} }));
        assert_eq!(back, [json!({ "id": 7, "result": {}, "sessionId": session })]);
        assert!(page_event(&mut link, "b1:1", json!({ "id": id, "result": {} })).is_empty(), "an answer nobody asked for is dropped");
        // detachFromTarget ends one session only.
        let out = texts(send(&mut link, &mut next, json!({ "id": 8, "method": "Target.detachFromTarget", "params": { "sessionId": second } })));
        assert_eq!(out[0]["method"], "Target.detachedFromTarget");
        assert_eq!(out[0]["params"]["sessionId"], second.as_str());
        let r = send(&mut link, &mut next, json!({ "id": 9, "method": "Page.enable", "sessionId": second }));
        assert!(refused(&r).unwrap().starts_with("herdr: unknown session"));
        assert_eq!(page_event(&mut link, "b1:1", json!({ "method": "Page.loadEventFired", "params": {} })).len(), 1);
    }

    #[test]
    fn attaching_a_tab_that_was_never_opened_waits_for_its_page() {
        let mut link = link();
        let mut next = 0;
        let FromAgent::Tab(op) = send(&mut link, &mut next, json!({ "id": 1, "method": "Target.attachToTarget", "params": { "targetId": "b1:3", "flatten": true } })) else { panic!() };
        assert_eq!(op.action, TabAction::Open { tab: "b1:3".into() });
        assert!(link.done(op.ticket, Ok("b1:3".into())).is_empty(), "the page doesn't exist yet");
        let mut now = pane();
        now.tabs[2].ready = true;
        let out: Vec<Value> = link.sync(now).iter().map(|t| serde_json::from_str(t).unwrap()).collect();
        assert_eq!(out.len(), 2);
        assert_eq!(out[1]["id"], 1);
        assert!(out[1]["result"]["sessionId"].is_string());
        // The window couldn't: the agent hears why.
        let FromAgent::Tab(op) = send(&mut link, &mut next, json!({ "id": 2, "method": "Target.activateTarget", "params": { "targetId": "b1:1" } })) else { panic!() };
        let out: Vec<Value> = link.done(op.ticket, Err("no".into())).iter().map(|t| serde_json::from_str(t).unwrap()).collect();
        assert_eq!(out, [json!({ "id": 2, "error": { "code": -32000, "message": "no" } })]);
    }

    #[test]
    fn create_activate_and_close_go_to_the_window_and_answer_when_it_has_acted() {
        let mut link = link();
        let mut next = 0;
        // createTarget: the url gate applies, no url is about:blank.
        for url in ["file:///etc/hosts", "chrome://version", "javascript:alert(1)", "data:text/html,x"] {
            let r = send(&mut link, &mut next, json!({ "id": 1, "method": "Target.createTarget", "params": { "url": url } }));
            assert_eq!(refused(&r), Some(format!("herdr: only http, https and about:blank pages open, not {url}")));
        }
        let FromAgent::Tab(op) = send(&mut link, &mut next, json!({ "id": 2, "method": "Target.createTarget", "params": { "url": "https://example.com/x" } })) else { panic!() };
        assert_eq!(op.action, TabAction::New { url: "https://example.com/x".into() });
        let FromAgent::Tab(blank) = send(&mut link, &mut next, json!({ "id": 3, "method": "Target.createTarget", "params": {} })) else { panic!() };
        assert_eq!(blank.action, TabAction::New { url: "about:blank".into() });
        assert_ne!(op.ticket, blank.ticket);
        // The new tab exists in the pane; the answer comes with its page.
        assert!(link.done(op.ticket, Ok("b1:4".into())).is_empty());
        let mut now = pane();
        now.tabs.push(tab("b1:4", "https://example.com/x", "", false));
        assert!(link.sync(now.clone()).is_empty());
        now.tabs[3].ready = true;
        let out: Vec<Value> = link.sync(now).iter().map(|t| serde_json::from_str(t).unwrap()).collect();
        assert_eq!(out, [json!({ "id": 2, "result": { "targetId": "b1:4" } })]);
        // activateTarget and Page.bringToFront select; closeTarget closes.
        let FromAgent::Tab(op) = send(&mut link, &mut next, json!({ "id": 4, "method": "Target.activateTarget", "params": { "targetId": "b1:1" } })) else { panic!() };
        assert_eq!(op.action, TabAction::Select { tab: "b1:1".into() });
        assert_eq!(link.done(op.ticket, Ok("b1:1".into())), [json!({ "id": 4, "result": {} }).to_string()]);
        let session = attach(&mut link, "b1:2");
        let FromAgent::Tab(op) = send(&mut link, &mut next, json!({ "id": 5, "method": "Page.bringToFront", "sessionId": session })) else { panic!() };
        assert_eq!(op.action, TabAction::Select { tab: "b1:2".into() });
        let out: Vec<Value> = link.done(op.ticket, Ok("b1:2".into())).iter().map(|t| serde_json::from_str(t).unwrap()).collect();
        assert_eq!(out, [json!({ "id": 5, "result": {}, "sessionId": session })]);
        let FromAgent::Tab(op) = send(&mut link, &mut next, json!({ "id": 6, "method": "Target.closeTarget", "params": { "targetId": "b1:2" } })) else { panic!() };
        assert_eq!(op.action, TabAction::Close { tab: "b1:2".into() });
        // The window drops the tab first, then answers: closing the tab doesn't fail its own closeTarget.
        let mut gone = pane();
        gone.tabs.remove(1);
        link.sync(gone);
        assert_eq!(link.done(op.ticket, Ok("b1:2".into())), [json!({ "id": 6, "result": { "success": true } }).to_string()]);
    }

    #[test]
    fn tab_changes_from_any_source_are_announced_once_discovery_is_on() {
        let mut link = link();
        let mut next = 0;
        let session = attach(&mut link, "b1:1");
        // Discovery off: tab changes are silent, a closed tab's sessions still end.
        let mut now = pane();
        now.tabs.push(tab("b1:4", "https://a.example/", "", false));
        assert!(link.sync(now.clone()).is_empty());
        send(&mut link, &mut next, json!({ "id": 1, "method": "Target.setDiscoverTargets", "params": { "discover": true } }));
        // A tab opened (a popup, the person), a url and title that changed, a tab closed.
        now.tabs.push(tab("b1:5", "https://popup.example/", "", true));
        now.tabs[0].title = "One, renamed".into();
        now.tabs[1].url = "https://example.com/next".into();
        now.tabs.remove(2);
        let out: Vec<Value> = link.sync(now.clone()).iter().map(|t| serde_json::from_str(t).unwrap()).collect();
        let kinds: Vec<(&str, &str)> = out.iter().map(|e| (e["method"].as_str().unwrap(), e["params"]["targetInfo"]["targetId"].as_str().or(e["params"]["targetId"].as_str()).unwrap())).collect();
        assert_eq!(
            kinds,
            [("Target.targetDestroyed", "b1:3"), ("Target.targetInfoChanged", "b1:1"), ("Target.targetInfoChanged", "b1:2"), ("Target.targetCreated", "b1:5")]
        );
        assert_eq!(out[1]["params"]["targetInfo"]["title"], "One, renamed");
        // Nothing changed: nothing said.
        assert!(link.sync(now.clone()).is_empty());
        // The attached tab closes: its session detaches, then the target is destroyed.
        now.tabs.remove(0);
        let out: Vec<Value> = link.sync(now).iter().map(|t| serde_json::from_str(t).unwrap()).collect();
        assert_eq!(out[0]["method"], "Target.detachedFromTarget");
        assert_eq!(out[0]["params"], json!({ "sessionId": session, "targetId": "b1:1" }));
        assert_eq!(out[1]["method"], "Target.targetDestroyed");
        let r = send(&mut link, &mut next, json!({ "id": 2, "method": "Page.enable", "sessionId": session }));
        assert!(refused(&r).unwrap().starts_with("herdr: unknown session"));
    }

    #[test]
    fn a_request_to_a_tab_that_closes_fails_instead_of_hanging() {
        let mut link = link();
        let mut next = 0;
        let session = attach(&mut link, "b1:1");
        send(&mut link, &mut next, json!({ "id": 5, "method": "Runtime.evaluate", "sessionId": session }));
        let mut gone = pane();
        gone.tabs.remove(0);
        let out: Vec<Value> = link.sync(gone).iter().map(|t| serde_json::from_str(t).unwrap()).collect();
        assert!(out.iter().any(|e| e["id"] == 5 && e["error"]["message"] == "herdr: the tab closed" && e["sessionId"] == session.as_str()), "{out:?}");
        assert!(link.pending.is_empty());
    }

    #[test]
    fn browser_level_auto_attach_is_refused_not_acknowledged() {
        let mut link = link();
        let mut next = 0;
        let r = send(&mut link, &mut next, json!({ "id": 1, "method": "Target.setAutoAttach", "params": { "autoAttach": true, "flatten": true } }));
        let message = refused(&r).unwrap();
        assert!(message.starts_with("herdr: browser-level Target.setAutoAttach is not supported"), "{message}");
        assert!(link.pending.is_empty());
    }

    #[test]
    fn child_sessions_route_to_their_tab_and_only_from_its_auto_attach() {
        let mut link = link();
        let mut next = 0;
        let s1 = attach(&mut link, "b1:1");
        let s2 = attach(&mut link, "b1:2");
        // A page-level setAutoAttach goes to the tab with flatten forced on.
        let FromAgent::Forward { message, tab, .. } = send(
            &mut link,
            &mut next,
            json!({ "id": 1, "method": "Target.setAutoAttach", "sessionId": s1, "params": { "autoAttach": true, "waitForDebuggerOnStart": false, "flatten": false } }),
        ) else {
            panic!()
        };
        assert_eq!(tab, "b1:1");
        let sent: Value = serde_json::from_str(&message).unwrap();
        assert_eq!((&sent["params"]["flatten"], sent.get("sessionId")), (&json!(true), None));
        // Not known yet: refused.
        let r = send(&mut link, &mut next, json!({ "id": 2, "method": "Runtime.evaluate", "sessionId": "C1" }));
        assert_eq!(refused(&r), Some("herdr: unknown session C1".to_owned()));
        // The tab's page auto-attached a frame: the event reaches the agent on the tab's session, ids intact.
        let ev = json!({ "method": "Target.attachedToTarget", "params": { "sessionId": "C1", "targetInfo": { "targetId": "F1", "type": "iframe" }, "waitingForDebugger": false } });
        let heard = page_event(&mut link, "b1:1", ev.clone());
        assert_eq!(heard.len(), 1);
        assert_eq!((heard[0]["sessionId"].as_str(), heard[0]["params"]["sessionId"].as_str()), (Some(s1.as_str()), Some("C1")));
        // C1 now routes to b1:1, and keeps its own session id toward CEF.
        let FromAgent::Forward { tab, message, .. } = send(&mut link, &mut next, json!({ "id": 3, "method": "Runtime.evaluate", "sessionId": "C1" })) else { panic!() };
        assert_eq!(tab, "b1:1");
        assert_eq!(serde_json::from_str::<Value>(&message).unwrap()["sessionId"], "C1");
        // The answer returns under C1; events of C1 pass through untouched.
        let FromAgent::Forward { id, .. } = send(&mut link, &mut next, json!({ "id": 4, "method": "Page.enable", "sessionId": "C1" })) else { panic!() };
        assert_eq!(page_event(&mut link, "b1:1", json!({ "id": id, "result": {}, "sessionId": "C1" })), [json!({ "id": 4, "result": {}, "sessionId": "C1" })]);
        let ev = json!({ "method": "Runtime.consoleAPICalled", "sessionId": "C1", "params": {} });
        assert_eq!(page_event(&mut link, "b1:1", ev.clone()), [ev.clone()]);
        // Another tab can't speak on C1, and a session nobody attached is dropped.
        assert!(page_event(&mut link, "b1:2", ev).is_empty());
        assert!(page_event(&mut link, "b1:1", json!({ "method": "Runtime.consoleAPICalled", "sessionId": "C9", "params": {} })).is_empty());
        // The allowlist applies inside child sessions.
        let r = send(&mut link, &mut next, json!({ "id": 5, "method": "Target.attachToBrowserTarget", "sessionId": "C1" }));
        assert!(refused(&r).is_some());
        // detachFromTarget of a frame: own children only.
        let r = send(&mut link, &mut next, json!({ "id": 6, "method": "Target.detachFromTarget", "params": { "sessionId": "C9" } }));
        assert!(refused(&r).is_some());
        assert!(matches!(send(&mut link, &mut next, json!({ "id": 7, "method": "Target.detachFromTarget", "params": { "sessionId": "C1" } })), FromAgent::Forward { .. }));
        // getTargetInfo of a frame goes to its tab; of anything else it's refused.
        let r = send(&mut link, &mut next, json!({ "id": 8, "method": "Target.getTargetInfo", "params": { "targetId": "OTHER" }, "sessionId": s1 }));
        assert!(refused(&r).is_some());
        assert!(matches!(send(&mut link, &mut next, json!({ "id": 9, "method": "Target.getTargetInfo", "params": { "targetId": "F1" }, "sessionId": s1 })), FromAgent::Forward { .. }));
        // Target events about other targets than this tab's frames are dropped.
        let ev = json!({ "method": "Target.targetCreated", "params": { "targetInfo": { "targetId": "T7" } } });
        assert!(page_event(&mut link, "b1:1", ev).is_empty());
        let ev = json!({ "method": "Target.targetInfoChanged", "params": { "targetInfo": { "targetId": "F1", "url": "https://frame/" } } });
        assert_eq!(page_event(&mut link, "b1:1", ev).len(), 1);
        // The frame detaches: the session is gone.
        let ev = json!({ "method": "Target.detachedFromTarget", "params": { "sessionId": "C1", "targetId": "F1" } });
        assert_eq!(page_event(&mut link, "b1:1", ev).len(), 1);
        let r = send(&mut link, &mut next, json!({ "id": 10, "method": "Runtime.evaluate", "sessionId": "C1" }));
        assert!(refused(&r).is_some());
        // Ending a tab's last session takes its frames along, announced on the tab's session.
        page_event(&mut link, "b1:2", json!({ "method": "Target.attachedToTarget", "params": { "sessionId": "C2", "targetInfo": { "targetId": "F2" } } }));
        let out = texts(send(&mut link, &mut next, json!({ "id": 11, "method": "Target.detachFromTarget", "params": { "sessionId": s2 } })));
        assert_eq!((out[0]["params"]["sessionId"].as_str(), out[0]["sessionId"].as_str()), (Some("C2"), Some(s2.as_str())));
        assert_eq!(out[1]["params"]["sessionId"], s2.as_str());
    }

    #[test]
    fn checked_params_page_navigate_print_and_key_commands() {
        let mut link = link();
        let session = attach(&mut link, "b1:1");
        let mut next = 0;
        for url in ["file:///etc/hosts", "chrome://version", "javascript:alert(1)", "data:text/html,x"] {
            let r = send(&mut link, &mut next, json!({ "id": 1, "method": "Page.navigate", "params": { "url": url }, "sessionId": session }));
            assert_eq!(refused(&r), Some(format!("herdr: only http, https and about:blank pages open, not {url}")));
        }
        let r = send(&mut link, &mut next, json!({ "id": 1, "method": "Page.navigate", "params": { "url": "https://example.com/" }, "sessionId": session }));
        assert!(matches!(r, FromAgent::Forward { .. }));
        let r = send(&mut link, &mut next, json!({ "id": 2, "method": "Page.printToPDF", "params": { "transferMode": "ReturnAsStream" }, "sessionId": session }));
        assert!(refused(&r).is_some());
        let r = send(&mut link, &mut next, json!({ "id": 3, "method": "Input.dispatchKeyEvent", "params": { "type": "keyDown", "commands": ["paste"] }, "sessionId": session }));
        assert!(refused(&r).is_some());
        let r = send(&mut link, &mut next, json!({ "id": 4, "method": "Input.dispatchKeyEvent", "params": { "type": "keyDown", "commands": [] }, "sessionId": session }));
        assert!(matches!(r, FromAgent::Forward { .. }));
    }

    #[test]
    fn a_page_outside_the_gate_takes_only_navigation() {
        let mut link = link();
        let session = attach(&mut link, "b1:1");
        let mut next = 0;
        let url_of = |tab: &str| (tab == "b1:1").then(|| "file:///etc/hosts".to_owned());
        let eval = json!({ "id": 1, "method": "Runtime.evaluate", "params": { "expression": "1" }, "sessionId": session }).to_string();
        let r = link.from_agent(&eval, &mut next, &url_of);
        assert_eq!(refused(&r), Some("herdr: the page is at file:, not an http, https or about:blank page: navigate it first".to_owned()));
        let nav = json!({ "id": 2, "method": "Page.navigate", "params": { "url": "https://a/" }, "sessionId": session }).to_string();
        assert!(matches!(link.from_agent(&nav, &mut next, &url_of), FromAgent::Forward { .. }));
        // The gate is per tab: another tab at a fine page is not affected.
        let other = attach(&mut link, "b1:2");
        let eval = json!({ "id": 3, "method": "Runtime.evaluate", "sessionId": other }).to_string();
        assert!(matches!(link.from_agent(&eval, &mut next, &url_of), FromAgent::Forward { .. }));
    }

    #[test]
    fn request_ids_are_rewritten_and_in_flight_requests_are_capped() {
        let mut link = link();
        let session = attach(&mut link, "b1:1");
        let mut next = 100;
        let FromAgent::Forward { id, message, .. } = send(&mut link, &mut next, json!({ "id": 1, "method": "Page.enable", "sessionId": session })) else { panic!() };
        assert_eq!(id, 101);
        assert_eq!(serde_json::from_str::<Value>(&message).unwrap()["id"], 101);
        for i in 1..MAX_IN_FLIGHT {
            assert!(matches!(send(&mut link, &mut next, json!({ "id": i, "method": "Page.enable", "sessionId": session })), FromAgent::Forward { .. }));
        }
        let r = send(&mut link, &mut next, json!({ "id": 999, "method": "Page.enable", "sessionId": session }));
        assert_eq!(refused(&r), Some("herdr: too many requests in flight".to_owned()));
        let r = send(&mut link, &mut next, json!({ "id": 999, "method": "Target.createTarget" }));
        assert_eq!(refused(&r), Some("herdr: too many requests in flight".to_owned()));
        // A forward the page never got fails under the agent's id.
        let failed = link.fail(101, "gone").unwrap();
        assert_eq!(serde_json::from_str::<Value>(&failed).unwrap(), json!({ "id": 1, "sessionId": session, "error": { "code": -32000, "message": "gone" } }));
        assert_eq!(link.fail(101, "gone"), None);
    }

    #[test]
    fn cookies_are_stripped_from_network_events() {
        let mut link = link();
        let session = attach(&mut link, "b1:1");
        let mut one = |ev: Value| {
            let out = page_event(&mut link, "b1:1", ev);
            assert_eq!(out[0]["sessionId"], session.as_str());
            out[0].clone()
        };
        let out = one(json!({ "method": "Network.responseReceivedExtraInfo", "params": {
            "requestId": "1", "headers": { "Set-Cookie": "sid=1", "content-type": "text/html" },
            "headersText": "HTTP/1.1 200\r\nSet-Cookie: sid=1\r\n", "blockedCookies": [], "exemptedCookies": [] } }));
        assert_eq!(out["params"]["headers"], json!({ "content-type": "text/html" }));
        assert!(out["params"].get("headersText").is_none());
        assert!(out["params"].get("blockedCookies").is_none());
        assert!(out["params"].get("exemptedCookies").is_none());
        let out = one(json!({ "method": "Network.requestWillBeSentExtraInfo", "params": {
            "requestId": "1", "headers": { "cookie": "sid=1", "accept": "*/*" }, "associatedCookies": [{ "cookie": { "value": "1" } }] } }));
        assert_eq!(out["params"]["headers"], json!({ "accept": "*/*" }));
        assert!(out["params"].get("associatedCookies").is_none());
        let out = one(json!({ "method": "Network.requestWillBeSent", "params": { "request": { "url": "https://a/", "headers": { "Cookie": "sid=1" } } } }));
        assert_eq!(out["params"]["request"]["headers"], json!({}));
        let out = one(json!({ "method": "Network.responseReceived", "params": { "response": { "headers": { "set-cookie": "a=1", "x": "y" }, "requestHeaders": { "Cookie": "a" } } } }));
        assert_eq!(out["params"]["response"]["headers"], json!({ "x": "y" }));
        assert_eq!(out["params"]["response"]["requestHeaders"], json!({}));
    }

    /// The network stack adds `Authorization` from its HTTP-auth cache;
    /// page script can't read it, so the agent doesn't get it either.
    #[test]
    fn credential_headers_are_stripped_from_network_events() {
        let mut link = link();
        attach(&mut link, "b1:1");
        let mut one = |ev: Value| page_event(&mut link, "b1:1", ev)[0].clone();
        let out = one(json!({ "method": "Network.requestWillBeSentExtraInfo", "params": {
            "requestId": "1", "headers": { "Authorization": "Basic YTpi", "proxy-authorization": "Basic Yzpk", "accept": "*/*" } } }));
        assert_eq!(out["params"]["headers"], json!({ "accept": "*/*" }));
        let out = one(json!({ "method": "Network.responseReceived", "params": { "response": {
            "headers": { "www-authenticate": "Basic" }, "requestHeaders": { "AUTHORIZATION": "Bearer t", "x": "y" } } } }));
        assert_eq!(out["params"]["response"]["requestHeaders"], json!({ "x": "y" }));
        assert_eq!(out["params"]["response"]["headers"], json!({ "www-authenticate": "Basic" }));
        let out = one(json!({ "method": "Network.requestWillBeSent", "params": { "request": { "url": "https://a/", "headers": { "Proxy-Authorization": "Basic e" } } } }));
        assert_eq!(out["params"]["request"]["headers"], json!({}));
    }

    #[test]
    fn malformed_messages_get_an_error_not_a_forward() {
        let mut link = link();
        let mut next = 0;
        assert!(refused(&link.from_agent("not json", &mut next, &none)).is_some());
        assert!(refused(&link.from_agent("{\"id\":1}", &mut next, &none)).is_some());
        assert!(refused(&link.from_agent("{\"method\":\"Page.enable\"}", &mut next, &none)).is_some());
    }

    /// `params` that isn't an object (or null) is an error answer, never a
    /// panic: a panic on the connection's thread skipped its cleanup, so the
    /// token stayed live and the page didn't reset.
    #[test]
    fn params_that_are_not_an_object_get_an_error_not_a_panic() {
        for method in ["Target.setAutoAttach", "Target.createTarget", "Target.attachToTarget", "Page.navigate", "Input.dispatchKeyEvent", "Page.printToPDF", "Runtime.evaluate"] {
            for params in [json!([]), json!("x"), json!(1), json!(true), json!([{ "flatten": false }])] {
                let mut link = link();
                let mut next = 0;
                let msg = json!({ "id": 7, "method": method, "params": params });
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| link.from_agent(&msg.to_string(), &mut next, &none)));
                let Ok(FromAgent::Reply(out)) = r else { panic!("{method} with params {params} panicked or was forwarded") };
                let out: Value = serde_json::from_str(&out[0]).unwrap();
                assert_eq!(out, json!({ "id": 7, "error": { "code": -32602, "message": "herdr: params must be an object" } }));
                assert!(link.pending.is_empty() && link.waiting.is_empty(), "nothing is in flight");
            }
        }
        // null means none: forwarded, and a page's setAutoAttach still gets flatten.
        let mut link = link();
        let session = attach(&mut link, "b1:1");
        let mut next = 0;
        let r = send(&mut link, &mut next, json!({ "id": 1, "method": "Target.setAutoAttach", "params": null, "sessionId": session }));
        let FromAgent::Forward { message, .. } = r else { panic!("null params refused: {:?}", refused(&r)) };
        assert_eq!(serde_json::from_str::<Value>(&message).unwrap()["params"], json!({ "flatten": true }));
    }

    #[test]
    fn a_session_id_that_is_not_a_string_is_refused_not_forwarded() {
        for session in [json!(5), json!({}), json!(null), json!(["s1"]), json!(true)] {
            let mut link = link();
            let mut next = 0;
            let r = send(&mut link, &mut next, json!({ "id": 3, "method": "Runtime.evaluate", "params": {}, "sessionId": session }));
            let FromAgent::Reply(out) = r else { panic!("sessionId {session} was forwarded") };
            let out: Value = serde_json::from_str(&out[0]).unwrap();
            assert_eq!(out, json!({ "id": 3, "error": { "code": -32600, "message": "herdr: sessionId must be a string" } }));
            assert!(link.pending.is_empty(), "nothing is in flight");
        }
    }
    // ---- the provider (agent-browser's plugin side)

    fn launch() -> Value {
        json!({ "protocol": PROTOCOL, "type": "browser.launch", "capability": "browser.provider",
            "request": { "provider": "herdr", "session": "herdr-abc", "launchOptions": {} } })
    }

    #[test]
    fn the_provider_answers_launch_with_a_browser_url() {
        let env = |k: &str| match k {
            "HC_TERMINAL_ID" => Some("t3".to_owned()),
            "AGENT_BROWSER_SESSION" => Some("herdr-abc".to_owned()),
            _ => None,
        };
        let mut asked = Value::Null;
        let out = respond(&launch(), &env, |req| {
            asked = req;
            Ok(json!({ "cdpUrl": "ws://127.0.0.1:4000/tok", "browser": "b1", "pane": "p2", "space": "sp1", "selection": "tab" }))
        });
        assert_eq!(asked, json!({ "terminal": "t3", "session": "herdr-abc", "browser": null }));
        assert_eq!(out, json!({ "protocol": PROTOCOL, "success": true, "browser": {
            "cdpUrl": "ws://127.0.0.1:4000/tok", "directPage": false,
            "metadata": { "space": "sp1", "browser": "b1", "pane": "p2", "selection": "tab" } } }));
    }

    #[test]
    fn the_provider_never_asks_herdr_outside_a_herdr_terminal() {
        let out = respond(&launch(), &|_| None, |_| panic!("asked the socket"));
        assert_eq!(out, json!({ "protocol": PROTOCOL, "success": false, "error": not_in_herdr() }));
        let env = |k: &str| match k {
            "HC_TERMINAL_ID" => Some("t3".to_owned()),
            "AGENT_BROWSER_SESSION" => Some("herdr-abc".to_owned()),
            _ => None,
        };
        let out = respond(&launch(), &env, |_| Err(no_browser()));
        assert_eq!(out["error"], no_browser());
        assert_eq!(out["success"], false);
    }

    #[test]
    fn a_terminal_older_than_herdr_uses_a_session_named_for_itself() {
        // Only HC_TERMINAL_ID: a terminal started before this Herdr.
        let env = |k: &str| (k == "HC_TERMINAL_ID").then(|| "t3".to_owned());
        let with = |session: &str| json!({ "protocol": PROTOCOL, "type": "browser.launch", "capability": "browser.provider",
            "request": { "provider": "herdr", "session": session, "launchOptions": {} } });
        // agent-browser's shared default session, or another terminal's: refused before Herdr is asked.
        for session in ["default", "herdr-abc", "herdr-t4"] {
            let out = respond(&with(session), &env, |_| panic!("asked the socket for {session}"));
            assert_eq!(out["error"], needs_own_session("t3"), "{session}");
        }
        assert!(needs_own_session("t3").contains("agent-browser -p herdr --session herdr-t3"));
        let mut asked = Value::Null;
        let out = respond(&with("herdr-t3"), &env, |req| {
            asked = req;
            Ok(json!({ "cdpUrl": "ws://127.0.0.1:4000/tok", "browser": "b1", "pane": "p2", "space": "sp1", "selection": "tab" }))
        });
        assert_eq!(out["success"], true);
        assert_eq!(asked["session"], "herdr-t3");
    }

    #[test]
    fn herdr_s_plugin_is_added_to_agent_browser_s_config_and_nothing_else_changes() {
        let plugin = json!({ "name": "herdr", "command": "/Apps/Herdr", "args": [VERB, "--socket", "/h/herdr-app.sock"], "capabilities": ["browser.provider", CHECK] });
        let fresh: Value = serde_json::from_str(&with_herdr_plugin("", "/Apps/Herdr", "/h/herdr-app.sock").unwrap()).unwrap();
        assert_eq!(fresh, json!({ "plugins": [plugin.clone()] }));
        // No default provider: agent-browser outside Herdr is unchanged.
        assert!(fresh.get("provider").is_none());
        let existing = r#"{ "headed": true, "plugins": [ { "name": "vault", "command": "vault" }, { "name": "herdr", "command": "/old/Herdr" } ] }"#;
        let merged: Value = serde_json::from_str(&with_herdr_plugin(existing, "/Apps/Herdr", "/h/herdr-app.sock").unwrap()).unwrap();
        assert_eq!(merged, json!({ "headed": true, "plugins": [ { "name": "vault", "command": "vault" }, plugin ] }));
        for bad in ["not json", "[1, 2]", r#"{ "plugins": "x" }"#] {
            assert!(with_herdr_plugin(bad, "/Apps/Herdr", "/s").is_err(), "{bad}");
        }
    }

    #[test]
    fn a_terminal_on_another_core_is_never_attached() {
        use crate::browser::{core_id, other_core, same_core, unknown_core};
        assert_eq!(core_id(&json!({ "result": { "core": { "id": "c1", "name": "m" } } })), Some("c1".into()));
        assert_eq!(core_id(&json!({ "core": { "id": "c1" } })), Some("c1".into()));
        assert_eq!(core_id(&json!({ "core": { "id": "" } })), None);
        assert_eq!(same_core("t3", Some("c1"), Some("c1")), Ok(()));
        assert_eq!(same_core("t3", Some("c2"), Some("c1")), Err(other_core("t3")));
        assert_eq!(same_core("t3", None, Some("c1")), Err(unknown_core("t3")));
        assert_eq!(same_core("t3", Some("c1"), None), Err(unknown_core("t3")));
    }

    #[test]
    fn herdr_check_says_why_without_a_token() {
        // agent-browser hides a provider's error text on launch; `plugin run
        // herdr herdr.check` shows it, so the agent can tell the person.
        let check = json!({ "protocol": PROTOCOL, "type": "herdr.check", "capability": "herdr.check", "request": {} });
        let env = |k: &str| (k == "HC_TERMINAL_ID").then(|| "t3".to_owned());
        let mut asked = Value::Null;
        let out = respond(&check, &env, |req| {
            asked = req;
            Err(no_browser())
        });
        assert_eq!(asked["check"], true);
        assert_eq!(out, json!({ "protocol": PROTOCOL, "success": false, "error": no_browser() }));
        let out = respond(&check, &env, |_| Ok(json!({ "browser": "b1", "selection": "tab" })));
        assert_eq!(out, json!({ "protocol": PROTOCOL, "success": true, "data": { "browser": "b1", "selection": "tab", "ready": true } }));
        assert_eq!(respond(&check, &|_| None, |_| panic!())["error"], not_in_herdr());
    }

    #[test]
    fn the_provider_close_is_a_no_op_and_the_manifest_names_herdr() {
        let close = json!({ "protocol": PROTOCOL, "type": "browser.close", "request": {} });
        assert_eq!(respond(&close, &|_| None, |_| panic!()), json!({ "protocol": PROTOCOL, "success": true, "data": {} }));
        let manifest = json!({ "protocol": PROTOCOL, "type": "plugin.manifest", "request": {} });
        assert_eq!(respond(&manifest, &|_| None, |_| panic!())["manifest"]["name"], "herdr");
        let bad = json!({ "protocol": "v0", "type": "browser.launch" });
        assert_eq!(respond(&bad, &|_| None, |_| panic!())["success"], false);
    }

    #[test]
    fn the_herdr_plugin_comes_first_and_once_over_an_inherited_list() {
        let inherited = json!([{ "name": "herdr", "command": "/old/herdr-app" }, { "name": "vault", "command": "vault" }, "junk"]).to_string();
        let env = plugin_env("/Apps/Herdr", "/h/herdr-app.sock", Some(&inherited), "herdr-x1");
        assert_eq!(env[0], ("AGENT_BROWSER_PROVIDER".to_owned(), "herdr".to_owned()));
        assert_eq!(env[2], ("AGENT_BROWSER_SESSION".to_owned(), "herdr-x1".to_owned()));
        let plugins: Value = serde_json::from_str(&env[1].1).unwrap();
        assert_eq!(env[1].0, "AGENT_BROWSER_PLUGINS");
        assert_eq!(plugins, json!([
            { "name": "herdr", "command": "/Apps/Herdr", "args": [VERB, "--socket", "/h/herdr-app.sock"], "capabilities": ["browser.provider", "herdr.check"] },
            { "name": "vault", "command": "vault" },
        ]));
        // Nothing inherited, or garbage.
        let env = plugin_env("/Apps/Herdr", "/s", Some("not json"), "s");
        assert_eq!(serde_json::from_str::<Value>(&env[1].1).unwrap().as_array().unwrap().len(), 1);
    }

    /// agent-browser's daemon runs an unauthenticated stream server on
    /// loopback for as long as it lives (upstream; no opt-out). Its own
    /// default idle exit is 60 minutes; Herdr's terminals ask for 5.
    #[test]
    fn terminals_ask_agent_browser_to_exit_after_five_idle_minutes() {
        let env = plugin_env("/Apps/Herdr", "/s", None, "s");
        assert_eq!(env.iter().find(|(k, _)| k == "AGENT_BROWSER_IDLE_TIMEOUT_MS").map(|(_, v)| v.as_str()), Some("300000"));
    }
}
