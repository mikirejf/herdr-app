//! The agents' loopback websocket: `ws://127.0.0.1:<port>/<token>` for one
//! browser pane. `agent::Link` answers the Target domain from the pane's tabs
//! (kept here, `set_tabs`) and passes every other allowed message to one tab's
//! page through the engine (`engine::send_devtools`): CEF's in-process
//! DevTools session (runtime.rs). No remote-debugging port exists; this is
//! the only way in.
//!
//! Threads: one accepts, one serves each connection. Engines are called only
//! on the main thread: messages for a page queue here and the engine drains
//! them there (CEF's pump), tab changes too (they go on to the window).
//! Tokens and ports are never logged.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{ErrorKind, Write as _};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;
use tungstenite::Message;
use tungstenite::protocol::WebSocketConfig;

use super::agent::{self, FromAgent, Link, Pane, TabInfo, TabOp, Tokens};
use super::{engine, registry};

/// Messages from a page waiting for an agent that isn't reading: past
/// either limit its connection ends, so an agent can't grow the app's memory.
const MAX_QUEUED: usize = 1024;
const MAX_QUEUED_BYTES: usize = 32 << 20;
/// A write the agent doesn't take within this ends its connection.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Connections not yet past the token check, each a thread for up to 2 s
/// (`peek_head`). Any local process can connect to loopback, so past this
/// many a new one is closed at once: a flood can't use up the app's threads
/// or files. Past the check there is at most one per live token (409).
const MAX_UNCHECKED: usize = 16;
static UNCHECKED: Slots = Slots::new(MAX_UNCHECKED);

/// A fixed number of slots, each held by a guard until it drops.
struct Slots {
    used: AtomicUsize,
    max: usize,
}

struct Slot<'a>(&'a Slots);

impl Slots {
    const fn new(max: usize) -> Self {
        Slots { used: AtomicUsize::new(0), max }
    }

    fn take(&self) -> Option<Slot<'_>> {
        self.used.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| (n < self.max).then_some(n + 1)).ok().map(|_| Slot(self))
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.used.fetch_sub(1, Ordering::SeqCst);
    }
}

/// How long the accept loop waits after its `n`th error in a row (out of
/// files, say): 10 ms, doubling, at most 1 s, so it never spins.
fn backoff(n: u32) -> Duration {
    Duration::from_millis(10u64.saturating_mul(1 << n.saturating_sub(1).min(7))).min(Duration::from_secs(1))
}

struct Conn {
    id: u64,
    link: Link,
    /// Text for the agent; the connection's thread writes it.
    out: mpsc::SyncSender<String>,
    /// Bytes in `out` not yet written.
    queued: Arc<AtomicUsize>,
    /// The socket: shutting it ends the connection even while its thread
    /// is stuck writing to an agent that stopped reading.
    socket: TcpStream,
}

impl Conn {
    /// Queues `text` for the agent. False: the queue is past its limit.
    fn push(&self, text: String) -> bool {
        let bytes = text.len();
        if self.queued.fetch_add(bytes, Ordering::SeqCst) + bytes > MAX_QUEUED_BYTES {
            return false;
        }
        self.out.try_send(text).is_ok()
    }
}

enum Job {
    /// An allowed message for `tab`'s page, under the proxy's id, from connection `conn`.
    ToPage { browser: String, conn: u64, tab: String, id: i64, message: String },
    /// A tab change connection `conn` asked of the window.
    Tab { browser: String, conn: u64, op: TabOp },
    /// An agent's connection ended: the pages it used reopen, so nothing it set stays.
    Gone { browser: String, tabs: Vec<String> },
}

#[derive(Default)]
struct Hub {
    port: Option<u16>,
    tokens: Tokens,
    /// One live connection per browser (and so per token).
    conns: HashMap<String, Conn>,
    next_conn: u64,
    /// Request ids sent to pages: one space for every browser.
    next_id: i64,
    /// Each tab's committed main-frame url (the proxy's URL gate).
    urls: HashMap<String, String>,
    /// The tabs of each browser with a token, as the window last said.
    tabs: HashMap<String, Pane>,
    /// Tabs whose page takes DevTools messages.
    ready: HashSet<String>,
    jobs: VecDeque<Job>,
    /// Connections whose thread still runs.
    serving: HashSet<u64>,
}

static HUB: OnceLock<Mutex<Hub>> = OnceLock::new();

impl Hub {
    /// Ends `browser`'s connection from this side: its socket shuts and
    /// the page's reset is queued now, so neither waits for the agent to
    /// read. True when there was one. The caller wakes the pump.
    fn end_conn(&mut self, browser: &str) -> bool {
        let Some(c) = self.conns.remove(browser) else { return false };
        let _ = c.socket.shutdown(Shutdown::Both);
        self.jobs.push_back(Job::Gone { browser: browser.to_owned(), tabs: c.link.touched() });
        true
    }

    /// Queues `texts` for `browser`'s agent. True: its queue is past the
    /// limit, so its connection and token ended (the caller wakes the pump).
    fn send(&mut self, browser: &str, texts: Vec<String>) -> bool {
        let Some(c) = self.conns.get(browser) else { return false };
        if texts.into_iter().all(|t| c.push(t)) {
            return false;
        }
        self.end_agent(browser);
        true
    }

    /// The agent hears what changed in `browser`'s tabs since it last looked.
    fn publish(&mut self, browser: &str) -> bool {
        let Some(pane) = self.tabs.get(browser).cloned() else { return false };
        let Some(c) = self.conns.get_mut(browser) else { return false };
        let texts = c.link.sync(pane);
        self.send(browser, texts)
    }

    /// The agent's token and connection end (the connection's end ends the
    /// token too: agent-browser asks the provider for a new one).
    fn end_agent(&mut self, browser: &str) -> bool {
        let token = self.tokens.revoke(browser);
        self.tabs.remove(browser);
        self.end_conn(browser) || token
    }

    /// Quit: every token and connection ends.
    fn quit(&mut self) {
        let mut all = self.tokens.browsers();
        all.extend(self.conns.keys().cloned());
        for b in &all {
            self.end_agent(b);
        }
    }
}

fn hub() -> MutexGuard<'static, Hub> {
    HUB.get_or_init(Mutex::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// A new token url for `browser`, whose tabs are `pane`. An earlier token for
/// it dies and its connection closes (the new agent takes over; the old one
/// sees its socket close).
pub fn attach(browser: &str, mut pane: Pane) -> Result<String, String> {
    let port = listen()?;
    let mut random = [0u8; 32];
    // SAFETY: arc4random_buf fills exactly `len` bytes of a valid buffer
    // from the OS CSPRNG; it cannot fail.
    unsafe { libc::arc4random_buf(random.as_mut_ptr().cast(), random.len()) };
    let mut h = hub();
    let token = h.tokens.issue(browser, random);
    let replaced = h.end_conn(browser);
    for t in &mut pane.tabs {
        t.ready = h.ready.contains(&t.id);
    }
    h.tabs.insert(browser.to_owned(), pane);
    if replaced {
        drop(h);
        engine::wake();
        tracing::info!("browser: agent session for {browser} replaced by a new attach", browser = super::log_safe(browser));
    }
    Ok(format!("ws://127.0.0.1:{port}/{token}"))
}

/// Ends `browser`'s token and connection (browser close, quit, CEF's agent
/// host gone). True when something was live.
pub fn revoke(browser: &str) -> bool {
    let live = hub().end_agent(browser);
    engine::wake();
    live
}

/// Quit: every token and connection ends.
pub fn revoke_all() {
    hub().quit();
    engine::wake();
}

/// The browsers with a token.
pub fn attached() -> Vec<String> {
    hub().tabs.keys().cloned().collect()
}

/// Whether `browser` has a token, so its tabs are worth publishing.
pub fn is_attached(browser: &str) -> bool {
    hub().tabs.contains_key(browser)
}

/// Ends the agent for `browser` when its queue overflowed (`Hub::send`).
fn overflowed(browser: &str) {
    engine::wake();
    tracing::warn!("browser: the agent for {browser} stopped reading; its connection and token ended", browser = super::log_safe(browser));
}

/// The tabs of `browser` and the shown one changed (a tab opened or closed,
/// a page said a new url or title). Ignored while it has no token.
pub fn set_tabs(browser: &str, tabs: Vec<TabInfo>, shown: &str) {
    let mut h = hub();
    if !h.tabs.contains_key(browser) {
        return;
    }
    let mut pane = Pane { tabs, shown: shown.to_owned() };
    for t in &mut pane.tabs {
        t.ready = h.ready.contains(&t.id);
    }
    h.tabs.insert(browser.to_owned(), pane);
    if h.publish(browser) {
        drop(h);
        overflowed(browser);
    }
}

/// `tab`'s page exists and takes DevTools messages (main thread).
pub fn tab_ready(tab: &str) {
    let mut h = hub();
    h.ready.insert(tab.to_owned());
    let browser = registry::browser_of(tab);
    if let Some(t) = h.tabs.get_mut(browser).and_then(|p| p.tabs.iter_mut().find(|t| t.id == tab)) {
        t.ready = true;
    }
    if h.publish(browser) {
        drop(h);
        overflowed(browser);
    }
}

/// `tab`'s page is gone (closed, or closed to reopen): it takes no messages now.
pub fn tab_gone(tab: &str) {
    let mut h = hub();
    h.ready.remove(tab);
    h.urls.remove(tab);
}

/// The window made the tab change `ticket` asked for (`Ok(tab)`), or couldn't.
pub fn tab_done(browser: &str, ticket: u64, result: Result<String, String>) {
    let mut h = hub();
    let Some(c) = h.conns.get_mut(browser) else { return };
    let texts = c.link.done(ticket, result);
    if h.send(browser, texts) {
        drop(h);
        overflowed(browser);
    }
}

/// The page's committed main-frame url changed (runtime.rs, `on_address_change`).
pub fn set_url(tab: &str, url: &str) {
    hub().urls.insert(tab.to_owned(), url.to_owned());
}

/// One DevTools message from `tab`'s page (main thread): to its agent,
/// if it has one and the message is its own.
pub fn from_page(tab: &str, bytes: &[u8]) {
    let browser = registry::browser_of(tab);
    let mut h = hub();
    let Some(c) = h.conns.get_mut(browser) else { return };
    let texts = c.link.from_page(tab, bytes);
    if h.send(browser, texts) {
        drop(h);
        overflowed(browser);
    }
}

/// Runs queued work for pages; the engine calls it on the main thread.
pub fn drain() {
    loop {
        let Some(job) = hub().jobs.pop_front() else { return };
        match job {
            Job::ToPage { browser, conn, tab, id, message } => {
                // A message from a connection that has ended since never reaches the page.
                if hub().conns.get(&browser).map(|c| c.id) != Some(conn) {
                    continue;
                }
                if !engine::send_devtools(&tab, message.as_bytes()) {
                    let mut h = hub();
                    if let Some(c) = h.conns.get_mut(&browser).filter(|c| c.id == conn)
                        && let Some(reply) = c.link.fail(id, "herdr: the page isn't there right now (opening or reopening): try again")
                        && h.send(&browser, vec![reply])
                    {
                        drop(h);
                        overflowed(&browser);
                    }
                }
            }
            Job::Tab { browser, conn, op } => {
                if hub().conns.get(&browser).map(|c| c.id) == Some(conn) {
                    engine::tab_op(&browser, op);
                }
            }
            Job::Gone { browser, tabs } => {
                tracing::debug!("browser: {browser}'s agent connection ended: {} page(s) to reopen", tabs.len(), browser = super::log_safe(&browser));
                for t in &tabs {
                    engine::agent_gone(t);
                }
            }
        }
    }
}

/// The listener, started on the first attach: 127.0.0.1 only, a port the OS picks.
fn listen() -> Result<u16, String> {
    if let Some(port) = hub().port {
        return Ok(port);
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("cannot open the agent socket: {e}"))?;
    let port = listener.local_addr().map_err(|e| format!("cannot open the agent socket: {e}"))?.port();
    std::thread::Builder::new()
        .name("herdr-agent-proxy".into())
        .spawn(move || {
            let mut errors = 0u32;
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        errors = 0;
                        // Past the cap the connection closes here, unread.
                        let Some(slot) = UNCHECKED.take() else {
                            tracing::debug!("browser: refused an agent connection (too many waiting)");
                            continue;
                        };
                        let _ = std::thread::Builder::new().name("herdr-agent-conn".into()).spawn(move || serve(stream, port, slot));
                    }
                    Err(e) => {
                        errors = errors.saturating_add(1);
                        if errors == 1 {
                            tracing::warn!("browser: the agent socket can't accept a connection: {e}");
                        }
                        std::thread::sleep(backoff(errors));
                    }
                }
            }
        })
        .map_err(|e| format!("cannot start the agent socket: {e}"))?;
    hub().port = Some(port);
    tracing::info!("browser: agent proxy listening on loopback");
    Ok(port)
}

/// The request line and headers, read without consuming them (tungstenite
/// reads them again for the handshake). None: not HTTP, too big, too slow.
fn peek_head(stream: &TcpStream) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut buf = vec![0u8; 8192];
    loop {
        match stream.peek(&mut buf) {
            Ok(0) => return None,
            Ok(n) => {
                if let Some(end) = buf[..n].windows(4).position(|w| w == b"\r\n\r\n") {
                    return String::from_utf8(buf[..end].to_vec()).ok();
                }
                if n == buf.len() {
                    return None;
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => return None,
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A bare status: no body, no CORS headers, nothing a page can read.
fn refuse(mut stream: TcpStream, code: u16) {
    let reason = match code {
        403 => "Forbidden",
        409 => "Conflict",
        _ => "Not Found",
    };
    let _ = stream.write_all(format!("HTTP/1.1 {code} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes());
    tracing::debug!("browser: refused an agent connection ({code})");
}

/// A connection's end, however its thread ends (a panic too): before the
/// handshake finished only the connection goes; after it, the agent's own
/// end also ends its token and resets the page, unless a revoke, replace or
/// full queue ended it first (they did both on their side).
struct Ending {
    browser: String,
    id: u64,
    open: bool,
}

impl Drop for Ending {
    fn drop(&mut self) {
        let own = {
            let mut h = hub();
            h.serving.remove(&self.id);
            match h.conns.get(&self.browser) {
                Some(c) if c.id == self.id && self.open => h.end_agent(&self.browser),
                Some(c) if c.id == self.id => {
                    h.conns.remove(&self.browser);
                    false
                }
                _ => false,
            }
        };
        if own {
            engine::wake();
        }
        if std::thread::panicking() {
            tracing::error!("browser: the agent connection to {} stopped on an error in Herdr", super::log_safe(&self.browser));
        }
        if self.open {
            tracing::info!("browser: the agent connection to {} ended", super::log_safe(&self.browser));
        }
    }
}

fn serve(stream: TcpStream, port: u16, slot: Slot<'static>) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let Some(head) = peek_head(&stream) else { return };
    let Ok(socket) = stream.try_clone() else { return };
    let (browser, id, rx, queued) = {
        let mut h = hub();
        let checked = agent::check_upgrade(&agent::read_upgrade(&head), port, &h.tokens, |b| h.conns.contains_key(b));
        let browser = match checked {
            Ok(b) => b,
            Err(code) => {
                drop(h);
                return refuse(stream, code);
            }
        };
        h.next_conn += 1;
        let id = h.next_conn;
        let (tx, rx) = mpsc::sync_channel(MAX_QUEUED);
        let queued = Arc::new(AtomicUsize::new(0));
        let link = Link::new(h.tabs.get(&browser).cloned().unwrap_or_default());
        h.conns.insert(browser.clone(), Conn { id, link, out: tx, queued: queued.clone(), socket });
        h.serving.insert(id);
        (browser, id, rx, queued)
    };
    // Past the token check: the slot is free for the next connection.
    drop(slot);
    let mut ending = Ending { browser: browser.clone(), id, open: false };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let config = WebSocketConfig::default().max_message_size(Some(agent::MAX_FRAME)).max_frame_size(Some(agent::MAX_FRAME));
    let Ok(mut ws) = tungstenite::accept_with_config(stream, Some(config)) else { return };
    ending.open = true;
    let _ = ws.get_mut().set_read_timeout(Some(Duration::from_millis(15)));
    tracing::info!("browser: an agent connected to {browser}", browser = super::log_safe(&browser));
    'conn: loop {
        loop {
            match rx.try_recv() {
                Ok(t) => {
                    queued.fetch_sub(t.len(), Ordering::SeqCst);
                    // A write the agent doesn't take in WRITE_TIMEOUT fails here too.
                    if ws.send(Message::text(t)).is_err() {
                        break 'conn;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                // The hub dropped this connection (revoke, replace, a full queue).
                Err(mpsc::TryRecvError::Disconnected) => break 'conn,
            }
        }
        let text = match ws.read() {
            Ok(Message::Text(t)) => t.to_string(),
            Ok(Message::Binary(b)) => match String::from_utf8(b.to_vec()) {
                Ok(t) => t,
                Err(_) => continue,
            },
            Ok(Message::Close(_)) => break,
            Ok(_) => continue,
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => continue,
            Err(_) => break,
        };
        if from_agent(&browser, id, &text).into_iter().any(|reply| ws.send(Message::text(reply)).is_err()) {
            break;
        }
    }
    // `ending` drops here: the agent's own end (it closed, or stopped
    // reading) ends its token too and the page resets.
}

/// Test only: an agent message that panics the connection's thread, to
/// prove `Ending` still ends the token and resets the page.
#[cfg(test)]
const TEST_PANIC: &str = "herdr-test: panic here";

/// The method an agent message names, as the debug log shows it: the
/// agent's text, escaped (`super::log_safe`), so it can't forge a log line.
fn logged_method(text: &str) -> Option<String> {
    serde_json::from_str::<Value>(text).ok()?.get("method")?.as_str().map(super::log_safe)
}

/// One agent message: queued for a page or the window, or answers straight back.
fn from_agent(browser: &str, conn: u64, text: &str) -> Vec<String> {
    #[cfg(test)]
    assert!(text != TEST_PANIC, "test: a panic on the agent connection's thread");
    let method = tracing::enabled!(tracing::Level::DEBUG).then(|| logged_method(text)).flatten();
    let mut h = hub();
    let Hub { conns, urls, next_id, jobs, .. } = &mut *h;
    let Some(c) = conns.get_mut(browser).filter(|c| c.id == conn) else { return Vec::new() };
    let r = c.link.from_agent(text, next_id, &|tab| urls.get(tab).cloned());
    // Method names only (the recon log): never params, never the token.
    let note = |what: &str| {
        if let Some(m) = &method {
            tracing::debug!("browser: cdp {browser} {what} {m}", browser = super::log_safe(browser));
        }
    };
    let (job, replies) = match r {
        FromAgent::Forward { id, tab, message } => {
            note("forward");
            (Some(Job::ToPage { browser: browser.to_owned(), conn, tab, id, message }), Vec::new())
        }
        FromAgent::Tab(op) => {
            note("tab");
            (Some(Job::Tab { browser: browser.to_owned(), conn, op }), Vec::new())
        }
        FromAgent::Reply(replies) => {
            note("answered");
            (None, replies)
        }
    };
    if let Some(job) = job {
        jobs.push_back(job);
        drop(h);
        engine::wake();
    }
    replies
}

#[cfg(test)]
mod tests {
    //! Real sockets against the real hub (no CEF: jobs only queue, nothing
    //! drains them here). Each test uses its own browser id.
    use super::*;
    use tungstenite::WebSocket;
    use tungstenite::handshake::HandshakeError;

    fn connect(url: &str) -> Result<WebSocket<TcpStream>, u16> {
        let addr = url.trim_start_matches("ws://").split('/').next().unwrap();
        let stream = TcpStream::connect(addr).unwrap();
        match tungstenite::client::client(url, stream) {
            Ok((ws, _)) => Ok(ws),
            Err(HandshakeError::Failure(tungstenite::Error::Http(r))) => Err(r.status().as_u16()),
            Err(e) => panic!("handshake: {e}"),
        }
    }

    fn wait(limit: Duration, done: impl Fn() -> bool) -> bool {
        let end = Instant::now() + limit;
        while Instant::now() < end {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        done()
    }

    fn connected(b: &str) -> bool {
        hub().conns.contains_key(b)
    }

    fn resets(b: &str) -> usize {
        hub().jobs.iter().filter(|j| matches!(j, Job::Gone { browser, .. } if browser == b)).count()
    }

    fn tab(b: &str, n: u32, ready: bool) -> TabInfo {
        TabInfo { id: format!("{b}:{n}"), url: "https://example.com/".into(), title: format!("Tab {n}"), ready }
    }

    /// A pane of browser `b` with one ready tab, `b:1`.
    fn pane(b: &str) -> Pane {
        tab_ready(&format!("{b}:1"));
        Pane { tabs: vec![tab(b, 1, true)], shown: format!("{b}:1") }
    }

    fn say(ws: &mut WebSocket<TcpStream>, msg: serde_json::Value) {
        ws.send(Message::text(msg.to_string())).unwrap();
    }

    /// Connects to `url` and attaches tab `b:1`: the agent's session id.
    fn attach_tab(ws: &mut WebSocket<TcpStream>, b: &str) -> String {
        say(ws, serde_json::json!({ "id": 1, "method": "Target.attachToTarget", "params": { "targetId": format!("{b}:1"), "flatten": true } }));
        assert_eq!(answer(ws)["method"], "Target.attachedToTarget");
        answer(ws)["result"]["sessionId"].as_str().unwrap().to_owned()
    }

    /// An event as big as `kb` KiB (the page's events reach the agent unasked).
    fn event(kb: usize) -> Vec<u8> {
        format!(r#"{{"method":"Page.frameNavigated","params":{{"x":"{}"}}}}"#, "a".repeat(kb << 10)).into_bytes()
    }

    /// Fills the socket until the connection's thread is stuck writing.
    fn stall(b: &str) {
        for _ in 0..24 {
            from_page(&format!("{b}:1"), &event(512));
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    #[test]
    fn an_agents_method_name_cannot_forge_a_log_line() {
        let forged = serde_json::json!({ "id": 1, "method": "Evil.x\nFORGED-LINE browser: agent attach browser=b1 terminal=t1\r\u{202e}" }).to_string();
        let logged = logged_method(&forged).unwrap();
        assert!(!logged.contains(['\n', '\r', '\u{202e}']), "{logged:?}");
        assert_eq!(logged, r"Evil.x\nFORGED-LINE browser: agent attach browser=b1 terminal=t1\r\u{202e}");
        assert_eq!(logged_method(r#"{"id":1,"method":"Page.navigate"}"#).as_deref(), Some("Page.navigate"));
    }

    #[test]
    fn an_agents_own_disconnect_ends_its_token_and_resets_the_page() {
        let url = attach("t-disc", pane("t-disc")).unwrap();
        let ws = connect(&url).unwrap();
        assert!(connected("t-disc"));
        drop(ws);
        assert!(wait(Duration::from_secs(3), || !connected("t-disc")));
        assert_eq!(connect(&url).err(), Some(403), "the same token works again after its connection ended");
        assert_eq!(resets("t-disc"), 1);
    }

    #[test]
    fn an_agent_that_stops_reading_is_cut_off_after_the_write_timeout() {
        let url = attach("t-stall", pane("t-stall")).unwrap();
        let mut ws = connect(&url).unwrap();
        attach_tab(&mut ws, "t-stall");
        stall("t-stall");
        assert!(connected("t-stall"));
        // Each write that moves nothing for WRITE_TIMEOUT fails; a partial one
        // can take one more round.
        assert!(wait(WRITE_TIMEOUT * 4, || !connected("t-stall")), "a blocked write never ends");
        assert_eq!(resets("t-stall"), 1);
        assert_eq!(connect(&url).err(), Some(403));
    }

    #[test]
    fn a_revoke_ends_a_stuck_connection_and_resets_the_page_at_once() {
        let url = attach("t-revoke", pane("t-revoke")).unwrap();
        let mut ws = connect(&url).unwrap();
        attach_tab(&mut ws, "t-revoke");
        stall("t-revoke");
        let id = hub().conns.get("t-revoke").unwrap().id;
        assert!(revoke("t-revoke"));
        // The reset is queued by the revoke, not by the thread's exit.
        assert_eq!(resets("t-revoke"), 1);
        assert!(wait(Duration::from_secs(1), || !hub().serving.contains(&id)), "the connection's thread is still stuck writing");
        assert_eq!(resets("t-revoke"), 1, "one reset per connection");
        assert_eq!(connect(&url).err(), Some(403));
    }

    /// The next text message from the page side of `ws`, skipping nothing.
    fn answer(ws: &mut WebSocket<TcpStream>) -> Value {
        ws.get_mut().set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        match ws.read().unwrap() {
            Message::Text(t) => serde_json::from_str(&t).unwrap(),
            other => panic!("not text: {other:?}"),
        }
    }

    #[test]
    fn malformed_params_get_an_error_and_the_connection_stays() {
        let url = attach("t-params", pane("t-params")).unwrap();
        set_url("t-params:1", "http://127.0.0.1:8765/browser-fixture.html");
        let mut ws = connect(&url).unwrap();
        ws.send(Message::text(r#"{"id":3,"method":"Target.setAutoAttach","params":[]}"#)).unwrap();
        assert_eq!(answer(&mut ws), serde_json::json!({ "id": 3, "error": { "code": -32602, "message": "herdr: params must be an object" } }));
        assert!(connected("t-params"), "the connection ended on a malformed message");
        assert_eq!(resets("t-params"), 0);
        drop(ws);
        assert!(wait(Duration::from_secs(3), || !connected("t-params")));
        assert_eq!(resets("t-params"), 1);
    }

    #[test]
    fn the_target_domain_is_answered_from_the_panes_tabs_and_never_reaches_a_page() {
        let url = attach("t-targets", pane("t-targets")).unwrap();
        let mut ws = connect(&url).unwrap();
        say(&mut ws, serde_json::json!({ "id": 1, "method": "Target.getTargets" }));
        let infos = answer(&mut ws)["result"]["targetInfos"].clone();
        assert_eq!(infos[0]["targetId"], "t-targets:1");
        assert_eq!(infos.as_array().unwrap().len(), 1);
        let session = attach_tab(&mut ws, "t-targets");
        // A tab of another pane doesn't exist here.
        say(&mut ws, serde_json::json!({ "id": 2, "method": "Target.attachToTarget", "params": { "targetId": "t-other:1", "flatten": true } }));
        assert_eq!(answer(&mut ws)["error"]["code"], -32601);
        // A page request queues for that tab, under the proxy's id and without Herdr's session id.
        say(&mut ws, serde_json::json!({ "id": 3, "method": "Runtime.evaluate", "sessionId": session, "params": { "expression": "1" } }));
        assert!(wait(Duration::from_secs(3), || hub().jobs.iter().any(|j| matches!(j, Job::ToPage { browser, tab, message, .. } if browser == "t-targets" && tab == "t-targets:1" && !message.contains("sessionId")))));
        assert!(!hub().jobs.iter().any(|j| matches!(j, Job::Tab { browser, .. } if browser == "t-targets")));
    }

    #[test]
    fn a_tab_closing_ends_its_sessions_and_the_connection_stays() {
        let b = "t-closing";
        tab_ready("t-closing:1");
        tab_ready("t-closing:2");
        let two = Pane { tabs: vec![tab(b, 1, true), tab(b, 2, true)], shown: "t-closing:1".into() };
        let url = attach(b, two).unwrap();
        let mut ws = connect(&url).unwrap();
        say(&mut ws, serde_json::json!({ "id": 1, "method": "Target.setDiscoverTargets", "params": { "discover": true } }));
        assert_eq!(answer(&mut ws)["method"], "Target.targetCreated");
        assert_eq!(answer(&mut ws)["method"], "Target.targetCreated");
        assert_eq!(answer(&mut ws)["id"], 1);
        let session = attach_tab(&mut ws, b);
        // The person closes tab 1 (a command, a click): the agent hears, and keeps its connection.
        set_tabs(b, vec![tab(b, 2, true)], "t-closing:2");
        let detached = answer(&mut ws);
        assert_eq!((detached["method"].as_str(), detached["params"]["sessionId"].as_str()), (Some("Target.detachedFromTarget"), Some(session.as_str())));
        assert_eq!(answer(&mut ws), serde_json::json!({ "method": "Target.targetDestroyed", "params": { "targetId": "t-closing:1" } }));
        say(&mut ws, serde_json::json!({ "id": 5, "method": "Target.getTargets" }));
        let infos = answer(&mut ws)["result"]["targetInfos"].clone();
        assert_eq!(infos.as_array().unwrap().len(), 1);
        assert_eq!(infos[0]["targetId"], "t-closing:2");
        assert!(connected(b), "closing a tab ended the connection");
        assert_eq!(resets(b), 0);
        // The close dropped the tab's url and readiness.
        set_url("t-closing:1", "https://x/");
        tab_gone("t-closing:1");
        assert!(!hub().urls.contains_key("t-closing:1"));
        // A tab change asked by the agent goes to the window and is answered when it has acted.
        say(&mut ws, serde_json::json!({ "id": 6, "method": "Target.createTarget", "params": { "url": "https://example.com/new" } }));
        let ticket = loop {
            let found = hub().jobs.iter().find_map(|j| match j {
                Job::Tab { browser, op, .. } if browser == b => Some(op.ticket),
                _ => None,
            });
            if let Some(t) = found {
                break t;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        tab_done(b, ticket, Ok("t-closing:3".into()));
        tab_ready("t-closing:3");
        set_tabs(b, vec![tab(b, 2, true), tab(b, 3, true)], "t-closing:3");
        let mut seen = answer(&mut ws);
        while seen.get("id").is_none() {
            seen = answer(&mut ws);
        }
        assert_eq!(seen, serde_json::json!({ "id": 6, "result": { "targetId": "t-closing:3" } }));
    }

    #[test]
    fn a_panic_on_the_connections_thread_still_ends_its_token_and_resets_the_page() {
        let url = attach("t-panic", pane("t-panic")).unwrap();
        let mut ws = connect(&url).unwrap();
        ws.send(Message::text(TEST_PANIC)).unwrap();
        assert!(wait(Duration::from_secs(3), || !connected("t-panic")), "the connection outlived its thread");
        assert_eq!(resets("t-panic"), 1);
        assert_eq!(connect(&url).err(), Some(403), "the token outlived its connection's thread");
    }

    #[test]
    fn waiting_connections_past_the_cap_get_no_slot_until_one_frees() {
        let slots = Slots::new(2);
        let (a, b) = (slots.take(), slots.take());
        assert!(a.is_some() && b.is_some());
        assert!(slots.take().is_none());
        drop(a);
        assert!(slots.take().is_some());
        drop(b);
        assert_eq!(slots.used.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn accept_errors_back_off_up_to_a_second() {
        assert_eq!(backoff(1), Duration::from_millis(10));
        assert_eq!(backoff(2), Duration::from_millis(20));
        assert_eq!(backoff(7), Duration::from_millis(640));
        assert_eq!(backoff(8), Duration::from_secs(1));
        assert_eq!(backoff(u32::MAX), Duration::from_secs(1));
        assert!(backoff(0) > Duration::ZERO);
    }

    #[test]
    fn quit_ends_every_token() {
        let mut h = Hub::default();
        h.tokens.issue("q1", [7; 32]);
        h.tokens.issue("q2", [8; 32]);
        h.quit();
        assert!(h.tokens.browsers().is_empty());
    }

    #[test]
    fn a_page_flood_past_the_queue_limit_ends_the_connection() {
        let url = attach("t-flood", pane("t-flood")).unwrap();
        let mut ws = connect(&url).unwrap();
        attach_tab(&mut ws, "t-flood");
        stall("t-flood");
        for _ in 0..=MAX_QUEUED {
            from_page("t-flood:1", br#"{"method":"Page.loadEventFired","params":{}}"#);
        }
        assert!(!connected("t-flood"), "the queue grew past its limit");
        assert_eq!(resets("t-flood"), 1);
        assert_eq!(connect(&url).err(), Some(403));
    }
}
