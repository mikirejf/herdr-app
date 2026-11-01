//! The agents' loopback websocket: `ws://127.0.0.1:<port>/<token>`
//! for one browser, passed through `agent::Link`'s allowlist to the engine
//! (`engine::send_devtools`): CEF's in-process DevTools session (runtime.rs),
//! or WebKit's translator (webkit.rs). No remote-debugging port exists; this
//! is the only way in.
//!
//! Threads: one accepts, one serves each connection. Engines are called only
//! on the main thread: messages for a page queue here and the engine drains
//! them there (CEF's pump; WebKit's wake task). Tokens and ports are never logged.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{ErrorKind, Write as _};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;
use tungstenite::Message;
use tungstenite::protocol::WebSocketConfig;

use super::agent::{self, FromAgent, Link, Tokens};
use super::engine;

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
    /// An allowed message for the page, under the proxy's id, from connection `conn`.
    ToPage { browser: String, conn: u64, id: i64, message: String },
    /// An agent's connection ended: the page reopens, so nothing it set stays.
    Gone { browser: String },
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
    /// Each browser's committed main-frame url (the proxy's URL gate).
    urls: HashMap<String, String>,
    jobs: VecDeque<Job>,
    /// Connections whose thread still runs.
    serving: HashSet<u64>,
    /// Browsers with agent access on (the registry's switch, mirrored so a
    /// close or quit logs the off of a browser no agent attached to).
    access_on: HashSet<String>,
}

static HUB: OnceLock<Mutex<Hub>> = OnceLock::new();

impl Hub {
    /// Ends `browser`'s connection from this side: its socket shuts and
    /// the page's reset is queued now, so neither waits for the agent to
    /// read. True when there was one. The caller wakes the pump.
    fn end_conn(&mut self, browser: &str) -> bool {
        let Some(c) = self.conns.remove(browser) else { return false };
        let _ = c.socket.shutdown(Shutdown::Both);
        self.jobs.push_back(Job::Gone { browser: browser.to_owned() });
        true
    }

    /// The agent's token and connection end (the connection's end ends the
    /// token too: agent-browser asks the provider for a new one).
    fn end_agent(&mut self, browser: &str) -> bool {
        let token = self.tokens.revoke(browser);
        self.end_conn(browser) || token
    }

    /// Quit: every token, connection and access switch ends. Returns each
    /// browser that had one, so each gets its "agent access off" line.
    fn quit(&mut self) -> Vec<String> {
        let mut all = self.tokens.browsers();
        all.extend(self.conns.keys().cloned());
        all.extend(self.access_on.drain());
        all.sort();
        all.dedup();
        for b in &all {
            self.end_agent(b);
        }
        all
    }
}

fn hub() -> MutexGuard<'static, Hub> {
    HUB.get_or_init(Mutex::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// A new token url for `browser` (its access is on: the window checked).
/// An earlier token for it dies and its connection closes (the new agent
/// takes over; the old one sees its socket close).
pub fn attach(browser: &str) -> Result<String, String> {
    let port = listen()?;
    let mut random = [0u8; 32];
    // SAFETY: arc4random_buf fills exactly `len` bytes of a valid buffer
    // from the OS CSPRNG; it cannot fail.
    unsafe { libc::arc4random_buf(random.as_mut_ptr().cast(), random.len()) };
    let mut h = hub();
    let token = h.tokens.issue(browser, random);
    if h.end_conn(browser) {
        drop(h);
        engine::wake();
        tracing::info!("browser: agent session for {browser} replaced by a new attach", browser = super::log_safe(browser));
    }
    Ok(format!("ws://127.0.0.1:{port}/{token}"))
}

/// Ends `browser`'s token and connection (access off, browser close, quit,
/// CEF's agent host gone). True when something was live.
pub fn revoke(browser: &str) -> bool {
    let live = hub().end_agent(browser);
    engine::wake();
    live
}

/// Quit: every token and connection ends. Returns the browsers that had
/// one or had access on.
pub fn revoke_all() -> Vec<String> {
    let all = hub().quit();
    engine::wake();
    all
}

/// The person turned agent access on or off for `browser`.
pub fn set_access(browser: &str, on: bool) {
    let mut h = hub();
    if on {
        h.access_on.insert(browser.to_owned());
    } else {
        h.access_on.remove(browser);
    }
}

/// `browser` closed: its token and connection end. True when access was
/// on or something was live (the caller logs the off).
pub fn close(browser: &str) -> bool {
    let was_on = hub().access_on.remove(browser);
    revoke(browser) || was_on
}

/// The page's committed main-frame url changed (runtime.rs, `on_address_change`).
pub fn set_url(browser: &str, url: &str) {
    hub().urls.insert(browser.to_owned(), url.to_owned());
}

/// One DevTools message from `browser`'s page (main thread): to its agent,
/// if it has one and the message is its own.
pub fn from_page(browser: &str, bytes: &[u8]) {
    let mut h = hub();
    let Some(c) = h.conns.get_mut(browser) else { return };
    let Some(text) = c.link.from_page(bytes) else { return };
    if !c.push(text) {
        h.end_agent(browser);
        drop(h);
        engine::wake();
        tracing::warn!("browser: the agent for {browser} stopped reading; its connection and token ended", browser = super::log_safe(browser));
    }
}

/// Runs queued work for pages; the engine calls it on the main thread.
pub fn drain() {
    loop {
        let Some(job) = hub().jobs.pop_front() else { return };
        match job {
            Job::ToPage { browser, conn, id, message } => {
                // A message from a connection that has ended since never reaches the page.
                if hub().conns.get(&browser).map(|c| c.id) != Some(conn) {
                    continue;
                }
                if !engine::send_devtools(&browser, message.as_bytes()) {
                    let mut h = hub();
                    if let Some(c) = h.conns.get_mut(&browser).filter(|c| c.id == conn)
                        && let Some(reply) = c.link.fail(id, "herdr: the page isn't there right now (opening or reopening): try again")
                        && !c.push(reply)
                    {
                        h.end_agent(&browser);
                    }
                }
            }
            Job::Gone { browser } => engine::agent_gone(&browser),
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
        h.conns.insert(browser.clone(), Conn { id, link: Link::default(), out: tx, queued: queued.clone(), socket });
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
        if let Some(reply) = from_agent(&browser, id, &text)
            && ws.send(Message::text(reply)).is_err()
        {
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

/// One agent message: queued for the page, or an answer straight back.
fn from_agent(browser: &str, conn: u64, text: &str) -> Option<String> {
    #[cfg(test)]
    assert!(text != TEST_PANIC, "test: a panic on the agent connection's thread");
    let method = tracing::enabled!(tracing::Level::DEBUG).then(|| logged_method(text)).flatten();
    let mut h = hub();
    let url = h.urls.get(browser).cloned().unwrap_or_default();
    let mut next = h.next_id;
    let c = h.conns.get_mut(browser).filter(|c| c.id == conn)?;
    let r = c.link.from_agent(text, &mut next, &url);
    h.next_id = next;
    match r {
        FromAgent::Forward { id, message } => {
            // Method names only (the recon log): never params, never the token.
            if let Some(m) = &method {
                tracing::debug!("browser: cdp {browser} forward {m}", browser = super::log_safe(browser));
            }
            h.jobs.push_back(Job::ToPage { browser: browser.to_owned(), conn, id, message });
            drop(h);
            engine::wake();
            None
        }
        FromAgent::Reply(reply) => {
            if let Some(m) = &method {
                tracing::debug!("browser: cdp {browser} refused {m}", browser = super::log_safe(browser));
            }
            Some(reply)
        }
    }
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
        hub().jobs.iter().filter(|j| matches!(j, Job::Gone { browser } if browser == b)).count()
    }

    /// An event as big as `kb` KiB (the page's events reach the agent unasked).
    fn event(kb: usize) -> Vec<u8> {
        format!(r#"{{"method":"Page.frameNavigated","params":{{"x":"{}"}}}}"#, "a".repeat(kb << 10)).into_bytes()
    }

    /// Fills the socket until the connection's thread is stuck writing.
    fn stall(b: &str) {
        for _ in 0..24 {
            from_page(b, &event(512));
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    #[test]
    fn an_agents_method_name_cannot_forge_a_log_line() {
        let forged = serde_json::json!({ "id": 1, "method": "Evil.x\nFORGED-LINE browser: agent access off browser=b1 by=pane control\r\u{202e}" }).to_string();
        let logged = logged_method(&forged).unwrap();
        assert!(!logged.contains(['\n', '\r', '\u{202e}']), "{logged:?}");
        assert_eq!(logged, r"Evil.x\nFORGED-LINE browser: agent access off browser=b1 by=pane control\r\u{202e}");
        assert_eq!(logged_method(r#"{"id":1,"method":"Page.navigate"}"#).as_deref(), Some("Page.navigate"));
    }

    #[test]
    fn an_agents_own_disconnect_ends_its_token_and_resets_the_page() {
        let url = attach("t-disc").unwrap();
        let ws = connect(&url).unwrap();
        assert!(connected("t-disc"));
        drop(ws);
        assert!(wait(Duration::from_secs(3), || !connected("t-disc")));
        assert_eq!(connect(&url).err(), Some(403), "the same token works again after its connection ended");
        assert_eq!(resets("t-disc"), 1);
    }

    #[test]
    fn an_agent_that_stops_reading_is_cut_off_after_the_write_timeout() {
        let url = attach("t-stall").unwrap();
        let _ws = connect(&url).unwrap();
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
        let url = attach("t-revoke").unwrap();
        let _ws = connect(&url).unwrap();
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
        let url = attach("t-params").unwrap();
        set_url("t-params", "http://127.0.0.1:8765/browser-fixture.html");
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
    fn a_panic_on_the_connections_thread_still_ends_its_token_and_resets_the_page() {
        let url = attach("t-panic").unwrap();
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
    fn close_and_quit_report_access_that_never_had_a_token() {
        set_access("t-close", true);
        assert!(close("t-close"), "access was on, with no agent");
        assert!(!close("t-close"));
        set_access("t-close2", true);
        set_access("t-close2", false);
        assert!(!close("t-close2"));
        let mut h = Hub::default();
        h.access_on.insert("q1".into());
        h.tokens.issue("q2", [7; 32]);
        assert_eq!(h.quit(), vec!["q1".to_owned(), "q2".to_owned()]);
        assert!(h.access_on.is_empty() && h.tokens.browsers().is_empty());
    }

    #[test]
    fn a_page_flood_past_the_queue_limit_ends_the_connection() {
        let url = attach("t-flood").unwrap();
        let _ws = connect(&url).unwrap();
        stall("t-flood");
        for _ in 0..=MAX_QUEUED {
            from_page("t-flood", br#"{"method":"Page.loadEventFired","params":{}}"#);
        }
        assert!(!connected("t-flood"), "the queue grew past its limit");
        assert_eq!(resets("t-flood"), 1);
        assert_eq!(connect(&url).err(), Some(403));
    }
}
