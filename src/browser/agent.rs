//! Agents drive a browser pane: the rules, with no CEF and no
//! sockets, so every security control here is unit-tested.
//!
//! The path: `agent-browser` in a Herdr terminal runs `herdr-app
//! __agent-browser-provider` (its plugin), which asks the window over the
//! control socket to `attach`. The window picks the caller's browser, and
//! only if the person turned agent access on for it (Allow in a native macOS
//! dialog that the pane's header button opens; never a command an agent can
//! send), hands back
//! `ws://127.0.0.1:<port>/<token>`. proxy.rs serves that socket and passes
//! CDP messages to that one page's in-process DevTools session, through the
//! allowlist below. CEF opens no remote-debugging port.

// Default builds compile these rules only for their tests; the browser feature uses them.
#![cfg_attr(not(all(target_os = "macos", feature = "browser-pane")), allow(dead_code))]

use serde_json::{Map, Value, json};

pub use super::{ALLOW_AFTER, Answer, Reply, Source, not_in_herdr, public_url};
use std::collections::HashMap;

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

// ---- consent --------------------------------------------------------------

pub const ONLY_THE_PERSON: &str =
    "agent access can only be turned on by the person: \"Allow agent\" in the browser pane's header opens a macOS dialog, and only its Allow button turns it on (turning it off works from anywhere)";

/// On only from the person's Allow in the native dialog (or the dev
/// switch of a dev-agent-access test build); off from anywhere. An agent
/// can reach the control socket, write mods and keys, and arrange or cover
/// anything Herdr draws in the window, so none of those may grant access,
/// Herdr's own pane button included: it only opens the dialog.
pub fn may_set_access(on: bool, from: Source) -> Result<(), &'static str> {
    if !on || matches!(from, Source::NativeConfirm | Source::DevSwitch) { Ok(()) } else { Err(ONLY_THE_PERSON) }
}

/// The dialog's buttons are added Cancel first (`NSAlertFirstButtonReturn`,
/// 1000: the default, Return and Esc), then Allow (1001). Any other
/// response (the modal stopped or aborted) is no answer.
pub fn answer_of(response: isize) -> Answer {
    match response {
        1000 => Answer::Cancel,
        1001 => Answer::Allow,
        _ => Answer::Closed,
    }
}

/// Whether "Allow agent…" may open the dialog now: only into the active
/// app (a click that activates Herdr opens nothing), and one dialog at a
/// time (`asking`: the browser a dialog is open for).
pub fn may_ask(app_active: bool, asking: Option<&str>) -> Result<(), &'static str> {
    if asking.is_some() {
        return Err("a dialog about agent access is already open");
    }
    if !app_active {
        return Err("Herdr isn't the active app: click Allow agent again");
    }
    Ok(())
}

/// Whether the dialog's reply turns access on: only Allow, answered while
/// Herdr was the active app, at least `ALLOW_AFTER` after it showed, for a
/// browser still open at the site the dialog named (`unchanged`). Every
/// other reply (Cancel, Return, Esc, closed) leaves access off; the error
/// says why, for the log.
pub fn turns_access_on(reply: &Reply, unchanged: bool) -> Result<(), &'static str> {
    match reply.answer {
        Answer::Allow => {}
        Answer::Cancel => return Err("the person chose Cancel"),
        Answer::Closed => return Err("the dialog closed without an answer"),
    }
    if !reply.app_active {
        return Err("Herdr wasn't the active app when it was answered");
    }
    if reply.after < ALLOW_AFTER {
        return Err("Allow came under 500 ms after the dialog showed");
    }
    if !unchanged {
        return Err("the browser closed, or its page went to another site, while the dialog was open");
    }
    Ok(())
}

/// `scheme://host[:port]` of a page, as the dialog names it.
pub fn site(url: &str) -> String {
    origin(url)
}

/// The dialog's title and text: the browser, its profile, and its site.
/// Only an id shaped like Herdr's (`b1`, `b2`, …; `registry::herdr_id`) is named:
/// an agent writes layout blocks, and the dialog must carry no sentence of
/// its choosing.
pub fn consent_text(browser: &str, site: &str) -> Result<(String, String), &'static str> {
    if !super::registry::herdr_id(browser) {
        return Err("agent access needs a browser id shaped like Herdr's (b and 1-9 digits: b1, b2, …)");
    }
    Ok((
        "Let agents drive this browser?".to_owned(),
        format!(
            "Browser {browser}, agent profile, at {}.\n\nAny agent in a Herdr terminal on this Mac can then read this page, click and type in it, run script in it and open other sites in this pane, until you click Stop agent. Use test accounts only.",
            super::log_safe(site)
        ),
    ))
}

/// Agent access only in a browser on the separate agent profile.
pub fn needs_agent_profile(agent_profile: bool) -> Result<(), &'static str> {
    if agent_profile { Ok(()) } else { Err("agent access needs a browser opened with profile \"agent\"") }
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
    "no browser in this space: open one with: herdr-app do browser.open --profile agent --url <url>".into()
}
pub fn access_off(browser: &str) -> String {
    format!("agent access is off for {browser}: ask the person to turn on agent access for {browser}")
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
pub fn person_profile(browser: &str) -> String {
    format!("{browser} uses the person's profile: agents can only drive a browser opened with --profile agent")
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
    pub agent_profile: bool,
    pub access: bool,
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

/// The chosen browser may be driven: the agent profile, and access on.
pub fn ready(c: &Candidate) -> Result<(), String> {
    if !c.agent_profile {
        Err(person_profile(c.browser))
    } else if !c.access {
        Err(access_off(c.browser))
    } else {
        Ok(())
    }
}

// ---- tokens -------------------------------------------------------------------

/// The live tokens: 32 random bytes each, one per browser, in memory only
/// (an app restart forgets them). A new attach for a browser replaces its
/// token; access off, browser close and quit revoke it.
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
        "Target" => matches!(name, "setAutoAttach" | "detachFromTarget" | "getTargetInfo"),
        "Browser" => name == "getVersion",
        _ => false,
    }
}

/// While the page itself is outside the URL gate (a person or a popup put
/// it on `file:` or `chrome:`), only these work: the agent can move it back.
fn navigation(method: &str) -> bool {
    matches!(method, "Page.navigate" | "Page.navigateToHistoryEntry" | "Page.reload" | "Page.getFrameTree" | "Page.enable")
}

/// What to do with one message from the agent.
#[derive(Debug, PartialEq)]
pub enum FromAgent {
    /// Send to the page, under the proxy's own request id.
    Forward { id: i64, message: String },
    /// Answer the agent directly (refused).
    Reply(String),
}

/// One agent connection's state: its requests in flight (the proxy's id →
/// the agent's), and the child sessions the page auto-attached for it.
#[derive(Default)]
pub struct Link {
    pending: HashMap<i64, Value>,
    /// sessionId → targetId, from `Target.attachedToTarget`.
    children: HashMap<String, String>,
}

impl Link {
    /// Checks one agent message against the allowlist and rewrites its id
    /// (`next` is the browser's id counter). `page_url`: the page's
    /// committed main-frame url.
    pub fn from_agent(&mut self, text: &str, next: &mut i64, page_url: &str) -> FromAgent {
        let Ok(Value::Object(mut msg)) = serde_json::from_str::<Value>(text) else {
            return FromAgent::Reply(error(&Value::Null, None, -32700, "herdr: not a JSON object"));
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        // A session id is a string or absent. Anything else is refused here,
        // not left to Chromium's parser to read as the page's own session.
        let session = match msg.get("sessionId") {
            None => None,
            Some(Value::String(s)) => Some(s.clone()).filter(|s| !s.is_empty()),
            Some(_) => return FromAgent::Reply(error(&id, None, -32600, "herdr: sessionId must be a string")),
        };
        let refuse = |m: &str| FromAgent::Reply(error(&id, session.as_deref(), -32601, m));
        let Some(method) = msg.get("method").and_then(Value::as_str).map(str::to_owned) else {
            return refuse("herdr: a request needs an id and a method");
        };
        if !(id.is_i64() || id.is_u64()) {
            return refuse("herdr: a request needs an id and a method");
        }
        if let Some(s) = &session
            && !self.children.contains_key(s)
        {
            return refuse(&format!("herdr: unknown session {s}"));
        }
        if !allowed_method(&method) {
            return refuse(&format!("herdr: {method} is not allowed for agents"));
        }
        if !allowed_url(page_url) && !navigation(&method) {
            let scheme = page_url.split(':').next().unwrap_or_default();
            return refuse(&format!("herdr: the page is at {scheme}:, not an http, https or about:blank page: navigate it first"));
        }
        // Every check below reads (and setAutoAttach writes) params as an
        // object: anything else is an error answer, never a panic.
        match msg.get("params") {
            None | Some(Value::Null) => {
                msg.insert("params".into(), json!({}));
            }
            Some(Value::Object(_)) => {}
            Some(_) => return FromAgent::Reply(error(&id, session.as_deref(), -32602, "herdr: params must be an object")),
        }
        let params = msg.entry("params").or_insert_with(|| json!({}));
        if let Err(m) = self.check_params(&method, params) {
            return refuse(&m);
        }
        if self.pending.len() >= MAX_IN_FLIGHT {
            return refuse("herdr: too many requests in flight");
        }
        *next += 1;
        self.pending.insert(*next, id);
        msg.insert("id".into(), json!(*next));
        FromAgent::Forward { id: *next, message: Value::Object(msg).to_string() }
    }

    fn check_params(&self, method: &str, params: &mut Value) -> Result<(), String> {
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
            "Target.getTargetInfo" if s("targetId").is_some_and(|t| !self.children.values().any(|c| c == t)) => {
                return Err("herdr: getTargetInfo only for this page and its own frames".into());
            }
            _ => {}
        }
        Ok(())
    }

    /// A forwarded request the page never got (it was reopening): the
    /// agent's error answer, or None when it's no longer in flight.
    pub fn fail(&mut self, id: i64, message: &str) -> Option<String> {
        let agent = self.pending.remove(&id)?;
        Some(error(&agent, None, -32000, message))
    }

    /// One message from the page: an answer goes back under the agent's own
    /// id; an event goes out only for this page or its own child sessions,
    /// with cookies stripped. None: drop it.
    pub fn from_page(&mut self, bytes: &[u8]) -> Option<String> {
        let Ok(Value::Object(mut msg)) = serde_json::from_slice::<Value>(bytes) else { return None };
        if let Some(id) = msg.get("id").and_then(Value::as_i64) {
            let agent = self.pending.remove(&id)?;
            msg.insert("id".into(), agent);
            return Some(Value::Object(msg).to_string());
        }
        let method = msg.get("method").and_then(Value::as_str)?.to_owned();
        if let Some(s) = msg.get("sessionId").and_then(Value::as_str).filter(|s| !s.is_empty())
            && !self.children.contains_key(s)
        {
            return None;
        }
        let params = msg.get_mut("params").and_then(Value::as_object_mut);
        match method.as_str() {
            "Target.attachedToTarget" => {
                let p = params?;
                let session = p.get("sessionId").and_then(Value::as_str)?.to_owned();
                let target = p.get("targetInfo").and_then(|t| t.get("targetId")).and_then(Value::as_str).unwrap_or_default().to_owned();
                self.children.insert(session, target);
            }
            "Target.detachedFromTarget" => {
                let session = params?.get("sessionId").and_then(Value::as_str)?.to_owned();
                self.children.remove(&session)?;
            }
            "Target.targetInfoChanged" | "Target.targetCreated" | "Target.targetDestroyed" | "Target.targetCrashed" => {
                let p = params?;
                let target = p.get("targetInfo").and_then(|t| t.get("targetId")).or_else(|| p.get("targetId")).and_then(Value::as_str)?;
                if !self.children.values().any(|c| c == target) {
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
        // The page's own events carry no sessionId. agent-browser's direct
        // page path names this page's session "" and drops events whose
        // sessionId differs (so console, errors and reload's load wait saw
        // nothing): tag them with "".
        msg.entry("sessionId").or_insert_with(|| json!(""));
        Some(Value::Object(msg).to_string())
    }
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
            "description": "Drive the Herdr browser pane the person allowed, beside this terminal" } }),
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
                    "cdpUrl": r["cdpUrl"], "directPage": true,
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
    #[allow(unused_imports)]
    use crate::browser::{dev_switch, parked, read_dev_switch, remote};
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

    // ---- consent

    #[test]
    fn only_the_native_confirmation_turns_access_on() {
        assert!(may_set_access(true, Source::NativeConfirm).is_ok());
        // Herdr's own pane button only opens the dialog; the socket, mods and
        // keys never turn access on.
        for from in [Source::PaneControl, Source::Socket, Source::Mod, Source::Close, Source::Quit] {
            assert_eq!(may_set_access(true, from).unwrap_err(), ONLY_THE_PERSON, "{from:?}");
        }
        // Off is accepted from anywhere.
        for from in [Source::NativeConfirm, Source::PaneControl, Source::Socket, Source::Mod, Source::Close, Source::Quit] {
            assert!(may_set_access(false, from).is_ok());
        }
    }

    fn reply(answer: Answer, after_ms: u64, app_active: bool) -> Reply {
        Reply { answer, after: std::time::Duration::from_millis(after_ms), app_active }
    }

    /// The native dialog: only Allow, in the active app, at least 500 ms
    /// after it showed, for a browser still at the site it named.
    #[test]
    fn only_allow_in_the_dialog_turns_access_on() {
        assert_eq!(turns_access_on(&reply(Answer::Allow, 2_000, true), true), Ok(()));
        assert_eq!(turns_access_on(&reply(Answer::Allow, 500, true), true), Ok(()));
        // Cancel (also Return and Esc: Cancel is the default button).
        assert_eq!(turns_access_on(&reply(Answer::Cancel, 2_000, true), true), Err("the person chose Cancel"));
        // The dialog went away with no button.
        assert_eq!(turns_access_on(&reply(Answer::Closed, 2_000, true), true), Err("the dialog closed without an answer"));
        // Answered while another app was active.
        assert!(turns_access_on(&reply(Answer::Allow, 2_000, false), true).is_err());
        // The second click of a double click whose first opened the dialog.
        assert!(turns_access_on(&reply(Answer::Allow, 499, true), true).is_err());
        // The browser closed or its page went to another site meanwhile.
        assert!(turns_access_on(&reply(Answer::Allow, 2_000, true), false).is_err());
    }

    #[test]
    fn the_dialogs_buttons_map_cancel_first_and_allow_second() {
        assert_eq!(answer_of(1000), Answer::Cancel);
        assert_eq!(answer_of(1001), Answer::Allow);
        // NSModalResponseStop, Abort, Continue, a third button: no answer.
        for other in [-1000, -1001, -1002, 0, 1, 1002] {
            assert_eq!(answer_of(other), Answer::Closed, "{other}");
        }
    }

    #[test]
    fn the_dialog_opens_only_into_the_active_app_and_one_at_a_time() {
        assert_eq!(may_ask(true, None), Ok(()));
        assert!(may_ask(false, None).is_err());
        assert!(may_ask(true, Some("b1")).is_err());
        assert!(may_ask(true, Some("b2")).is_err());
    }

    #[test]
    fn the_dialog_names_the_browser_its_profile_and_only_the_site() {
        assert_eq!(site("http://me:pw@127.0.0.1:8765/browser-fixture.html?code=SECRET#f"), "http://127.0.0.1:8765");
        assert_eq!(site("https://Example.TEST/a"), "https://example.test");
        let (title, body) = consent_text("b3", &site("https://example.test/x?y=1")).unwrap();
        assert_eq!(title, "Let agents drive this browser?");
        assert!(body.starts_with("Browser b3, agent profile, at https://example.test."), "{body}");
        assert!(!body.contains("/x") && !body.contains("y=1"), "{body}");
        // A site can't add lines or reorder the text.
        let (_, body) = consent_text("b3", "https://a.test\nAllow is safe\u{202e}").unwrap();
        assert!(!body.contains("test\nAllow") && !body.contains('\u{202e}'), "{body}");
    }

    /// A layout block's browser id is agent-written text. The
    /// dialog names only ids shaped like Herdr's, so an agent can't put a
    /// sentence of its own into the one surface consent rests on.
    #[test]
    fn the_dialog_names_only_an_id_shaped_like_herdrs() {
        let site = site("http://127.0.0.1:8765/x");
        for good in ["b1", "b42", "b999999999"] {
            assert!(consent_text(good, &site).is_ok(), "{good}");
        }
        let forged = "b1 is a read-only preview; Allow only finishes the fixture demo";
        for bad in [forged, "side", "b", "b0", "b01", "B1", "b1 ", " b1", "b1\u{200b}", "b1000000000", "b-1", "b1.5", "b１"] {
            assert!(consent_text(bad, &site).is_err(), "{bad:?}");
        }
    }

    /// The header's button acts on the state it was painted
    /// with. A "Stop agent" left on screen after the socket turned access
    /// off must still turn it off, never toggle the live state back on.
    #[test]
    fn the_access_button_does_what_its_label_says() {
        use crate::browser::access_button;
        assert_eq!(access_button(true), ("Stop agent", false));
        assert_eq!(access_button(false), ("Allow agent…", true));
        // Whatever it shows, "Stop agent" only ever turns access off.
        for shown in [true, false] {
            let (label, sets) = access_button(shown);
            assert_eq!(label == "Stop agent", !sets, "{label} sets {sets}");
        }
    }

    #[test]
    fn agent_access_needs_the_agent_profile() {
        assert_eq!(needs_agent_profile(false).unwrap_err(), "agent access needs a browser opened with profile \"agent\"");
        assert!(needs_agent_profile(true).is_ok());
    }

    /// Every build but a debug one with the `dev-agent-access` feature: the
    /// variable means nothing (default and `--features browser` test runs,
    /// and every release build).
    #[cfg(not(all(debug_assertions, feature = "dev-agent-access")))]
    #[test]
    fn builds_without_the_dev_feature_have_no_dev_switch() {
        // SAFETY: test-only; no other test reads this variable.
        unsafe { std::env::set_var("HERDR_DEV_AGENT_ACCESS", "1") };
        read_dev_switch();
        assert!(!dev_switch());
        unsafe { std::env::remove_var("HERDR_DEV_AGENT_ACCESS") };
    }

    // ---- selecting the caller's browser

    fn cand(browser: &'static str, tab: Option<&'static str>, focused_at: u64) -> Candidate<'static> {
        Candidate { browser, space: Some("sp1"), tab, focused_at, agent_profile: true, access: true }
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
    fn a_selected_browser_needs_access_on_and_the_agent_profile() {
        assert_eq!(ready(&Candidate { access: false, ..cand("b1", None, 0) }).unwrap_err(), access_off("b1"));
        assert_eq!(ready(&Candidate { access: false, agent_profile: false, ..cand("b1", None, 0) }).unwrap_err(), person_profile("b1"));
        assert!(ready(&cand("b1", None, 0)).is_ok());
    }

    #[test]
    fn every_error_an_agent_can_see_is_pinned() {
        assert_eq!(not_in_herdr(), "this terminal is not in Herdr (HC_TERMINAL_ID is unset): agent-browser can only drive a Herdr browser pane from a Herdr terminal");
        assert_eq!(no_app("/h/herdr-app.sock"), "Herdr is not running (no socket at /h/herdr-app.sock)");
        assert_eq!(parked("t4"), "terminal t4 has no pane in Herdr (parked): give it one with herdr-app do terminal.keep --terminal t4");
        assert_eq!(remote("m1/t4"), "terminal m1/t4 runs on another machine: browser panes are only for this machine's terminals");
        assert_eq!(no_browser(), "no browser in this space: open one with: herdr-app do browser.open --profile agent --url <url>");
        assert_eq!(access_off("b1"), "agent access is off for b1: ask the person to turn on agent access for b1");
        assert_eq!(
            ambiguous(&["b1", "b2"]),
            "two or more browsers here (b1, b2): run `export HERDR_BROWSER=<id> AGENT_BROWSER_SESSION=$AGENT_BROWSER_SESSION-<id>`, then agent-browser again (a running agent-browser session keeps the environment it started with)"
        );
        assert_eq!(person_profile("b1"), "b1 uses the person's profile: agents can only drive a browser opened with --profile agent");
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

    // ---- the method allowlist (review § 2, items 3 and 4)

    const PAGE: &str = "http://127.0.0.1:8765/browser-fixture.html";

    fn send(link: &mut Link, next: &mut i64, msg: Value) -> FromAgent {
        link.from_agent(&msg.to_string(), next, PAGE)
    }

    fn refused(r: &FromAgent) -> Option<String> {
        match r {
            FromAgent::Reply(text) => serde_json::from_str::<Value>(text).ok()?["error"]["message"].as_str().map(str::to_owned),
            FromAgent::Forward { .. } => None,
        }
    }

    #[test]
    fn what_agent_browser_sends_is_forwarded() {
        let mut link = Link::default();
        let mut next = 0;
        for method in [
            "Page.enable", "Runtime.enable", "Runtime.runIfWaitingForDebugger", "Network.enable", "WebMCP.enable",
            "Accessibility.getFullAXTree", "DOM.getDocument", "DOM.resolveNode", "DOM.getBoxModel", "Runtime.evaluate",
            "Runtime.callFunctionOn", "Page.getFrameTree", "Page.captureScreenshot", "Page.createIsolatedWorld",
            "Input.dispatchMouseEvent", "Input.insertText", "Emulation.setDeviceMetricsOverride", "Browser.getVersion",
            "Target.getTargetInfo",
        ] {
            let r = send(&mut link, &mut next, json!({ "id": 1, "method": method, "params": {} }));
            assert!(matches!(r, FromAgent::Forward { .. }), "{method} was refused: {:?}", refused(&r));
        }
    }

    #[test]
    fn everything_unlisted_is_refused_with_herdrs_error() {
        let mut link = Link::default();
        let mut next = 0;
        for method in [
            "Target.attachToBrowserTarget", "Target.attachToTarget", "Target.createTarget", "Target.getTargets", "Target.setDiscoverTargets",
            "Browser.setDownloadBehavior", "Browser.grantPermissions", "Browser.close", "Network.getCookies", "Network.getAllCookies",
            "Storage.getCookies", "Network.setCookie", "DOM.setFileInputFiles", "DOM.getFileInfo", "Input.dispatchDragEvent",
            "Page.addScriptToEvaluateOnNewDocument", "Page.setDownloadBehavior", "Runtime.addBinding", "Fetch.enable",
            "DOMStorage.getDOMStorageItems", "IndexedDB.requestData", "CacheStorage.requestEntries", "ServiceWorker.enable",
            "Tracing.start", "Tethering.bind", "Security.setIgnoreCertificateErrors", "Debugger.enable", "IO.read", "Nope.nothing", "Page",
            "Runtime.addBinding ", "Runtime.evaluate\nFORGED x", "Runtime.", ".evaluate", "Runtime.evaluate.x", "Accessibility.enable\u{0}",
        ] {
            let r = send(&mut link, &mut next, json!({ "id": 7, "method": method }));
            assert_eq!(refused(&r), Some(format!("herdr: {method} is not allowed for agents")), "{method}");
            let FromAgent::Reply(text) = r else { unreachable!() };
            let reply: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(reply["id"], 7);
            assert_eq!(reply["error"]["code"], -32601);
        }
    }

    #[test]
    fn checked_params_page_navigate_print_and_key_commands() {
        let mut link = Link::default();
        let mut next = 0;
        for url in ["file:///etc/hosts", "chrome://version", "javascript:alert(1)", "data:text/html,x"] {
            let r = send(&mut link, &mut next, json!({ "id": 1, "method": "Page.navigate", "params": { "url": url } }));
            assert_eq!(refused(&r), Some(format!("herdr: only http, https and about:blank pages open, not {url}")));
        }
        let r = send(&mut link, &mut next, json!({ "id": 1, "method": "Page.navigate", "params": { "url": "https://example.com/" } }));
        assert!(matches!(r, FromAgent::Forward { .. }));
        let r = send(&mut link, &mut next, json!({ "id": 2, "method": "Page.printToPDF", "params": { "transferMode": "ReturnAsStream" } }));
        assert!(refused(&r).is_some());
        let r = send(&mut link, &mut next, json!({ "id": 3, "method": "Input.dispatchKeyEvent", "params": { "type": "keyDown", "commands": ["paste"] } }));
        assert!(refused(&r).is_some());
        let r = send(&mut link, &mut next, json!({ "id": 4, "method": "Input.dispatchKeyEvent", "params": { "type": "keyDown", "commands": [] } }));
        assert!(matches!(r, FromAgent::Forward { .. }));
    }

    #[test]
    fn a_page_outside_the_gate_takes_only_navigation() {
        let mut link = Link::default();
        let mut next = 0;
        let r = link.from_agent(&json!({ "id": 1, "method": "Runtime.evaluate", "params": { "expression": "1" } }).to_string(), &mut next, "file:///etc/hosts");
        assert_eq!(refused(&r), Some("herdr: the page is at file:, not an http, https or about:blank page: navigate it first".to_owned()));
        let r = link.from_agent(&json!({ "id": 2, "method": "Page.navigate", "params": { "url": "https://a/" } }).to_string(), &mut next, "chrome://version");
        assert!(matches!(r, FromAgent::Forward { .. }));
    }

    #[test]
    fn request_ids_are_rewritten_and_answers_map_back() {
        let mut link = Link::default();
        let mut next = 100;
        let FromAgent::Forward { id, message } = send(&mut link, &mut next, json!({ "id": 1, "method": "Page.enable" })) else { panic!() };
        assert_eq!(id, 101);
        assert_eq!(serde_json::from_str::<Value>(&message).unwrap()["id"], 101);
        let back = link.from_page(json!({ "id": 101, "result": {} }).to_string().as_bytes()).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&back).unwrap(), json!({ "id": 1, "result": {} }));
        // An answer nobody here asked for is dropped.
        assert_eq!(link.from_page(json!({ "id": 101, "result": {} }).to_string().as_bytes()), None);
        assert_eq!(link.from_page(json!({ "id": 5, "result": {} }).to_string().as_bytes()), None);
    }

    #[test]
    fn in_flight_requests_are_capped() {
        let mut link = Link::default();
        let mut next = 0;
        for i in 0..MAX_IN_FLIGHT {
            assert!(matches!(send(&mut link, &mut next, json!({ "id": i, "method": "Page.enable" })), FromAgent::Forward { .. }));
        }
        let r = send(&mut link, &mut next, json!({ "id": 999, "method": "Page.enable" }));
        assert_eq!(refused(&r), Some("herdr: too many requests in flight".to_owned()));
    }

    #[test]
    fn child_sessions_only_from_this_connections_auto_attach() {
        let mut link = Link::default();
        let mut next = 0;
        // setAutoAttach goes out with flatten forced on.
        let FromAgent::Forward { message, .. } =
            send(&mut link, &mut next, json!({ "id": 1, "method": "Target.setAutoAttach", "params": { "autoAttach": true, "waitForDebuggerOnStart": false, "flatten": false } }))
        else {
            panic!()
        };
        assert_eq!(serde_json::from_str::<Value>(&message).unwrap()["params"]["flatten"], true);
        // A message for a session nobody attached is refused.
        let r = send(&mut link, &mut next, json!({ "id": 2, "method": "Runtime.evaluate", "sessionId": "S9" }));
        assert_eq!(refused(&r), Some("herdr: unknown session S9".to_owned()));
        // The page auto-attached a frame: that session becomes usable.
        let ev = json!({ "method": "Target.attachedToTarget", "params": { "sessionId": "S1", "targetInfo": { "targetId": "T1", "type": "iframe" }, "waitingForDebugger": false } });
        assert!(link.from_page(ev.to_string().as_bytes()).is_some());
        let r = send(&mut link, &mut next, json!({ "id": 3, "method": "Runtime.evaluate", "sessionId": "S1" }));
        assert!(matches!(r, FromAgent::Forward { .. }));
        // The allowlist applies inside child sessions too.
        let r = send(&mut link, &mut next, json!({ "id": 4, "method": "Target.attachToBrowserTarget", "sessionId": "S1" }));
        assert!(refused(&r).is_some());
        // detachFromTarget only for an own child.
        let r = send(&mut link, &mut next, json!({ "id": 5, "method": "Target.detachFromTarget", "params": { "sessionId": "S9" } }));
        assert!(refused(&r).is_some());
        let r = send(&mut link, &mut next, json!({ "id": 6, "method": "Target.detachFromTarget", "params": { "sessionId": "S1" } }));
        assert!(matches!(r, FromAgent::Forward { .. }));
        // getTargetInfo on another target is refused; on an own child it goes.
        let r = send(&mut link, &mut next, json!({ "id": 7, "method": "Target.getTargetInfo", "params": { "targetId": "OTHER" } }));
        assert!(refused(&r).is_some());
        let r = send(&mut link, &mut next, json!({ "id": 8, "method": "Target.getTargetInfo", "params": { "targetId": "T1" } }));
        assert!(matches!(r, FromAgent::Forward { .. }));
        // Events from sessions this connection never saw are dropped.
        let ev = json!({ "method": "Runtime.consoleAPICalled", "sessionId": "S9", "params": {} });
        assert_eq!(link.from_page(ev.to_string().as_bytes()), None);
        let ev = json!({ "method": "Target.targetCreated", "params": { "targetInfo": { "targetId": "T7" } } });
        assert_eq!(link.from_page(ev.to_string().as_bytes()), None);
        // Detached: the session is gone.
        let ev = json!({ "method": "Target.detachedFromTarget", "params": { "sessionId": "S1", "targetId": "T1" } });
        assert!(link.from_page(ev.to_string().as_bytes()).is_some());
        let r = send(&mut link, &mut next, json!({ "id": 9, "method": "Runtime.evaluate", "sessionId": "S1" }));
        assert!(refused(&r).is_some());
    }

    /// agent-browser's direct-page session id is "": a page event without
    /// that tag never reaches its console, errors or reload wait.
    #[test]
    fn page_events_carry_the_direct_page_session_id() {
        let mut link = Link::default();
        let out = |link: &mut Link, ev: Value| serde_json::from_str::<Value>(&link.from_page(ev.to_string().as_bytes()).unwrap()).unwrap();
        for method in ["Runtime.consoleAPICalled", "Runtime.exceptionThrown", "Page.loadEventFired"] {
            assert_eq!(out(&mut link, json!({ "method": method, "params": {} }))["sessionId"], "", "{method}");
        }
        // A child session keeps its own id.
        link.from_page(json!({ "method": "Target.attachedToTarget", "params": { "sessionId": "S1", "targetInfo": { "targetId": "T1" } } }).to_string().as_bytes()).unwrap();
        assert_eq!(out(&mut link, json!({ "method": "Runtime.consoleAPICalled", "sessionId": "S1", "params": {} }))["sessionId"], "S1");
        // Answers are matched by id and stay untagged.
        let mut next = 0;
        let FromAgent::Forward { id, .. } = send(&mut link, &mut next, json!({ "id": 7, "method": "Page.reload" })) else { panic!() };
        assert_eq!(out(&mut link, json!({ "id": id, "result": {} })), json!({ "id": 7, "result": {} }));
    }

    #[test]
    fn cookies_are_stripped_from_network_events() {
        let mut link = Link::default();
        let ev = json!({ "method": "Network.responseReceivedExtraInfo", "params": {
            "requestId": "1", "headers": { "Set-Cookie": "sid=1", "content-type": "text/html" },
            "headersText": "HTTP/1.1 200\r\nSet-Cookie: sid=1\r\n", "blockedCookies": [], "exemptedCookies": [] } });
        let out: Value = serde_json::from_str(&link.from_page(ev.to_string().as_bytes()).unwrap()).unwrap();
        assert_eq!(out["params"]["headers"], json!({ "content-type": "text/html" }));
        assert!(out["params"].get("headersText").is_none());
        assert!(out["params"].get("blockedCookies").is_none());
        assert!(out["params"].get("exemptedCookies").is_none());
        let ev = json!({ "method": "Network.requestWillBeSentExtraInfo", "params": {
            "requestId": "1", "headers": { "cookie": "sid=1", "accept": "*/*" }, "associatedCookies": [{ "cookie": { "value": "1" } }] } });
        let out: Value = serde_json::from_str(&link.from_page(ev.to_string().as_bytes()).unwrap()).unwrap();
        assert_eq!(out["params"]["headers"], json!({ "accept": "*/*" }));
        assert!(out["params"].get("associatedCookies").is_none());
        let ev = json!({ "method": "Network.requestWillBeSent", "params": { "request": { "url": "https://a/", "headers": { "Cookie": "sid=1" } } } });
        let out: Value = serde_json::from_str(&link.from_page(ev.to_string().as_bytes()).unwrap()).unwrap();
        assert_eq!(out["params"]["request"]["headers"], json!({}));
        let ev = json!({ "method": "Network.responseReceived", "params": { "response": { "headers": { "set-cookie": "a=1", "x": "y" }, "requestHeaders": { "Cookie": "a" } } } });
        let out: Value = serde_json::from_str(&link.from_page(ev.to_string().as_bytes()).unwrap()).unwrap();
        assert_eq!(out["params"]["response"]["headers"], json!({ "x": "y" }));
        assert_eq!(out["params"]["response"]["requestHeaders"], json!({}));
    }

    /// The network stack adds `Authorization` from its HTTP-auth cache;
    /// page script can't read it, so the agent doesn't get it either.
    #[test]
    fn credential_headers_are_stripped_from_network_events() {
        let mut link = Link::default();
        let ev = json!({ "method": "Network.requestWillBeSentExtraInfo", "params": {
            "requestId": "1", "headers": { "Authorization": "Basic YTpi", "proxy-authorization": "Basic Yzpk", "accept": "*/*" } } });
        let out: Value = serde_json::from_str(&link.from_page(ev.to_string().as_bytes()).unwrap()).unwrap();
        assert_eq!(out["params"]["headers"], json!({ "accept": "*/*" }));
        let ev = json!({ "method": "Network.responseReceived", "params": { "response": {
            "headers": { "www-authenticate": "Basic" }, "requestHeaders": { "AUTHORIZATION": "Bearer t", "x": "y" } } } });
        let out: Value = serde_json::from_str(&link.from_page(ev.to_string().as_bytes()).unwrap()).unwrap();
        assert_eq!(out["params"]["response"]["requestHeaders"], json!({ "x": "y" }));
        assert_eq!(out["params"]["response"]["headers"], json!({ "www-authenticate": "Basic" }));
        let ev = json!({ "method": "Network.requestWillBeSent", "params": { "request": { "url": "https://a/", "headers": { "Proxy-Authorization": "Basic e" } } } });
        let out: Value = serde_json::from_str(&link.from_page(ev.to_string().as_bytes()).unwrap()).unwrap();
        assert_eq!(out["params"]["request"]["headers"], json!({}));
    }

    #[test]
    fn malformed_messages_get_an_error_not_a_forward() {
        let mut link = Link::default();
        let mut next = 0;
        assert!(refused(&link.from_agent("not json", &mut next, PAGE)).is_some());
        assert!(refused(&link.from_agent("{\"id\":1}", &mut next, PAGE)).is_some());
        assert!(refused(&link.from_agent("{\"method\":\"Page.enable\"}", &mut next, PAGE)).is_some());
    }

    /// `params` that isn't an object (or null) is an error answer, never a
    /// panic: a panic on the connection's thread skipped its cleanup, so the
    /// token stayed live and the page didn't reset.
    #[test]
    fn params_that_are_not_an_object_get_an_error_not_a_panic() {
        for method in ["Target.setAutoAttach", "Page.navigate", "Input.dispatchKeyEvent", "Page.printToPDF", "Runtime.evaluate"] {
            for params in [json!([]), json!("x"), json!(1), json!(true), json!([{ "flatten": false }])] {
                let mut link = Link::default();
                let mut next = 0;
                let msg = json!({ "id": 7, "method": method, "params": params });
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| link.from_agent(&msg.to_string(), &mut next, PAGE)));
                let Ok(FromAgent::Reply(text)) = r else { panic!("{method} with params {params} panicked or was forwarded") };
                let out: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(out, json!({ "id": 7, "error": { "code": -32602, "message": "herdr: params must be an object" } }));
                assert!(link.pending.is_empty(), "nothing is in flight");
            }
        }
        // null means none: forwarded, and setAutoAttach still gets flatten.
        let mut link = Link::default();
        let mut next = 0;
        let r = send(&mut link, &mut next, json!({ "id": 1, "method": "Target.setAutoAttach", "params": null }));
        let FromAgent::Forward { message, .. } = r else { panic!("null params refused: {:?}", refused(&r)) };
        assert_eq!(serde_json::from_str::<Value>(&message).unwrap()["params"], json!({ "flatten": true }));
    }

    #[test]
    fn a_session_id_that_is_not_a_string_is_refused_not_forwarded() {
        for session in [json!(5), json!({}), json!(null), json!(["s1"]), json!(true)] {
            let mut link = Link::default();
            let mut next = 0;
            let r = send(&mut link, &mut next, json!({ "id": 3, "method": "Runtime.evaluate", "params": {}, "sessionId": session }));
            let FromAgent::Reply(text) = r else { panic!("sessionId {session} was forwarded") };
            let out: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(out, json!({ "id": 3, "error": { "code": -32600, "message": "herdr: sessionId must be a string" } }));
            assert!(link.pending.is_empty(), "nothing is in flight");
        }
        // Absent: the page's own session, forwarded.
        let mut link = Link::default();
        let mut next = 0;
        assert!(matches!(send(&mut link, &mut next, json!({ "id": 4, "method": "Runtime.evaluate", "params": {} })), FromAgent::Forward { .. }));
    }

    // ---- the provider (agent-browser's plugin side)

    fn launch() -> Value {
        json!({ "protocol": PROTOCOL, "type": "browser.launch", "capability": "browser.provider",
            "request": { "provider": "herdr", "session": "herdr-abc", "launchOptions": {} } })
    }

    #[test]
    fn the_provider_answers_launch_with_a_direct_page_url() {
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
            "cdpUrl": "ws://127.0.0.1:4000/tok", "directPage": true,
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
        let out = respond(&launch(), &env, |_| Err(access_off("b1")));
        assert_eq!(out["error"], access_off("b1"));
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
            Err(access_off("b1"))
        });
        assert_eq!(asked["check"], true);
        assert_eq!(out, json!({ "protocol": PROTOCOL, "success": false, "error": access_off("b1") }));
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
