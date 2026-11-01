//! The WKWebView engine (`browser-webkit`): the system's web view as each
//! browser block's page. Nothing is shipped: WebKit is part of macOS, and
//! Apple patches it with the OS.
//!
//! Same contract as runtime.rs (CEF), in the same native-view block: each
//! page is a `WKWebView` in its block's container view (view.rs). No message
//! pump and no app-class hook: WebKit runs on the app's own run loop.
//!
//! Agents: proxy.rs keeps the token, consent, allowlist and revocation, and
//! hands each allowed CDP message to `send_devtools`. translate.rs says what
//! the message does here; this file does it: script in an isolated content
//! world (webkit.js) for the DOM and the accessibility tree, page-world
//! script for `Runtime.evaluate` by value (as CDP's default context),
//! `NSEvent`s sent to the view for trusted input, `takeSnapshot` for
//! screenshots. Answers and page events go back through `proxy::from_page`.
//! Nothing outside translate.rs's table exists for an agent. No inspector
//! port is ever opened (WebKit has one only behind env vars Herdr never sets).
//!
//! Profiles: the person's pages use WebKit's default data store; agent
//! pages share one non-persistent store, gone when the app quits.
//!
//! Everything here runs on the main thread: WebKit's callbacks come there,
//! and `wake` hops proxy work there through GPUI's foreground executor.
//! WebKit can call back (KVO) from inside a call into it, so no `STATE`
//! borrow is held across one.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::OnceLock;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass as _, MainThreadMarker, MainThreadOnly, Message as _, define_class, msg_send, sel};
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSEvent, NSEventModifierFlags, NSEventType, NSImage, NSView};
use objc2_foundation::{
    NSArray, NSData, NSDataBase64EncodingOptions, NSDictionary, NSError, NSJSONSerialization, NSJSONWritingOptions, NSKeyValueObservingOptions,
    NSObjectNSKeyValueObserverRegistration as _, NSPoint, NSProcessInfo, NSRect, NSSize, NSString, NSURL, NSURLRequest,
};
use objc2_web_kit::{
    WKContentWorld, WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKNavigationResponse,
    WKNavigationResponsePolicy, WKNavigationType, WKScriptMessage, WKScriptMessageHandler, WKSnapshotConfiguration, WKUIDelegate,
    WKUserContentController, WKUserScript, WKUserScriptInjectionTime, WKWebView, WKWebViewConfiguration, WKWebsiteDataStore,
    WKWindowFeatures,
};
use serde_json::{Value, json};

use super::agent;
use super::emulate::Viewport;
use super::native::Event;
use super::proxy;
use super::registry::{BLANK, Page};
use super::translate::{self, Button, Key, Mods, Mouse, MouseKind, Shot, Step};
use super::view;

/// The isolated world's CDP helper.
const RUNTIME: &str = include_str!("webkit.js");
/// The isolated world's name: page script can't see it or reach into it.
const WORLD: &str = "herdr-cdp";
/// Agent pages only: console calls and uncaught errors, relayed for
/// `Runtime.consoleAPICalled` and `exceptionThrown`. It runs in the page's
/// own world, so a page can see it and post its own messages (translate.rs
/// `console` drops anything that isn't a relay message).
const CONSOLE_RELAY: &str = r#"(() => {
  const h = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.herdrConsole;
  if (!h) return;
  const post = (m) => { try { h.postMessage(JSON.stringify(m)); } catch (_) {} };
  const str = (x) => { try { return typeof x === "string" ? x : String(JSON.stringify(x)); } catch (_) { return String(x); } };
  for (const t of ["log", "info", "warn", "error", "debug"]) {
    const o = console[t];
    console[t] = function (...a) { post({ t, a: a.map(str) }); return o.apply(this, a); };
  }
  addEventListener("error", (e) => post({ err: String(e.message), src: String(e.filename || ""), line: e.lineno | 0 }));
  addEventListener("unhandledrejection", (e) => post({ err: "Uncaught (in promise) " + str(e.reason && e.reason.message || e.reason) }));
})();"#;
/// The view properties a page's header follows.
const OBSERVED: [&str; 5] = ["title", "URL", "loading", "canGoBack", "canGoForward"];
/// How long `Page.navigate` waits for the load to end before answering anyway.
const NAVIGATE_WAIT: Duration = Duration::from_secs(10);

/// What `browser.devtools` tells the person: WebKit's inspector is Safari's.
pub const DEVTOOLS_NOTE: Option<&str> = Some(
    "This page can be inspected now. In Safari, turn on Settings > Advanced > \"Show features for web developers\", then open Develop > this Mac > Herdr's page.",
);

/// One open page.
struct Wk {
    generation: u64,
    web: Retained<WKWebView>,
    delegate: Retained<Delegate>,
    agent_profile: bool,
    page: Page,
    /// Committed navigations so far: CDP's `loaderId`.
    loader: u64,
    user_agent: String,
    /// An agent's mouse button is down (a move is then a drag).
    buttons_down: bool,
    /// `Page.navigate` requests waiting for the load to end (the proxy's ids).
    waiting: Vec<i64>,
    /// The size the page lays out at (`browser.viewport`, an agent's device
    /// metrics), in CSS pixels; None: it fills its block.
    size: Option<(f64, f64)>,
    /// A full-page screenshot has the page laid out taller for a moment.
    growing: bool,
}

#[derive(Default)]
struct State {
    started: bool,
    /// The app is quitting: nothing opens any more.
    down: bool,
    pages: HashMap<String, Wk>,
    next_generation: u64,
    /// The agent profile's store: in memory, one for every agent page.
    agent_store: Option<Retained<WKWebsiteDataStore>>,
    /// The isolated world agents' DOM calls run in (`isolated_world`).
    world: Option<Retained<WKContentWorld>>,
    fg: Option<gpui::ForegroundExecutor>,
    bg: Option<gpui::BackgroundExecutor>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    static EVENTS: RefCell<Option<async_channel::Sender<Event>>> = const { RefCell::new(None) };
    /// Agents' key-downs that still exist (`agent_event`). Weak: an entry
    /// lives exactly as long as someone (WebKit, until it is done with the
    /// key) holds the event, and a dead one never matches a new event at
    /// the same address.
    static AGENT_KEYS: RefCell<Vec<Weak<NSEvent>>> = const { RefCell::new(Vec::new()) };
}

/// `wake` → the foreground task that runs the proxy's queued work.
static WAKE: OnceLock<async_channel::Sender<()>> = OnceLock::new();

pub fn set_events(tx: async_channel::Sender<Event>) {
    EVENTS.with_borrow_mut(|e| *e = Some(tx));
}

fn send(event: Event) {
    EVENTS.with_borrow(|e| {
        if let Some(tx) = e {
            let _ = tx.try_send(event);
        }
    });
}

pub fn clicked_herdr() {
    send(Event::ClickedHerdr);
}

pub fn covers_changed() {
    send(Event::Covers);
}

/// A mouse press landed in `id`'s page (view.rs): AppKit makes the view first
/// responder as the press goes through, which WebKit doesn't report, so the
/// press is the person's choice of the page.
pub fn pressed(id: &str) {
    if STATE.with_borrow(|s| s.pages.contains_key(id)) {
        send(Event::Focused(id.to_owned()));
    }
}

/// Herdr's view made the page first responder already; WebKit needs nothing more.
pub fn set_focus(_id: &str) {}

fn web(id: &str) -> Option<Retained<WKWebView>> {
    STATE.with_borrow(|s| s.pages.get(id).map(|p| p.web.clone()))
}

fn need(id: &str) -> Result<Retained<WKWebView>, String> {
    web(id).ok_or_else(|| format!("browser {id} is still opening"))
}

/// The view of `id`'s page, if it is open and still `generation`.
fn current(id: &str, generation: u64) -> Option<Retained<WKWebView>> {
    STATE.with_borrow(|s| s.pages.get(id).filter(|p| p.generation == generation).map(|p| p.web.clone()))
}

pub fn page_view(id: &str) -> Option<NonNull<NSView>> {
    web(id).map(|w| NonNull::from(&**w).cast())
}

fn mtm() -> Result<MainThreadMarker, String> {
    MainThreadMarker::new().ok_or_else(|| "not on the main thread".to_owned())
}

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

fn request(url: &str) -> Option<Retained<NSURLRequest>> {
    NSURL::URLWithString(&ns(url)).map(|u| NSURLRequest::requestWithURL(&u))
}

fn now_secs() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// Starts the engine, once: the proxy's wake task and the quit hook. WebKit
/// itself needs no start.
pub fn start(cx: &mut gpui::App) -> anyhow::Result<()> {
    let ready = STATE.with_borrow(|s| (s.down, s.started));
    match ready {
        (true, _) => anyhow::bail!("the app is quitting"),
        (false, true) => return Ok(()),
        _ => {}
    }
    let fg = cx.foreground_executor().clone();
    let bg = cx.background_executor().clone();
    let (tx, rx) = async_channel::unbounded::<()>();
    if WAKE.set(tx).is_ok() {
        fg.spawn(async move {
            while rx.recv().await.is_ok() {
                while rx.try_recv().is_ok() {}
                proxy::drain();
            }
        })
        .detach();
    }
    cx.on_app_quit(|_| {
        shutdown();
        async {}
    })
    .detach();
    STATE.with_borrow_mut(|s| {
        s.started = true;
        s.fg = Some(fg);
        s.bg = Some(bg);
    });
    tracing::info!("browser: WebKit engine ready (the system's WKWebView)");
    Ok(())
}

/// Quit: every agent token and connection ends, every page closes.
fn shutdown() {
    for b in proxy::revoke_all() {
        tracing::info!("{}", super::access_line(false, &b, super::Source::Quit));
    }
    let ids: Vec<String> = STATE.with_borrow_mut(|s| {
        s.down = true;
        s.pages.keys().cloned().collect()
    });
    for id in ids {
        close(&id);
    }
}

/// Runs the proxy's queued work soon, on the main thread (any thread).
pub fn wake() {
    if let Some(tx) = WAKE.get() {
        let _ = tx.try_send(());
    }
}

/// Runs `f` on the main thread after `wait`.
fn after(wait: Duration, f: impl FnOnce() + 'static) {
    let Some((fg, bg)) = STATE.with_borrow(|s| s.fg.clone().zip(s.bg.clone())) else { return };
    fg.spawn(async move {
        bg.timer(wait).await;
        f();
    })
    .detach();
}

/// The agent profile's store, made once.
fn agent_store(mtm: MainThreadMarker) -> Retained<WKWebsiteDataStore> {
    if let Some(s) = STATE.with_borrow(|s| s.agent_store.clone()) {
        return s;
    }
    // SAFETY: a main-thread WebKit constructor.
    let store = unsafe { WKWebsiteDataStore::nonPersistentDataStore(mtm) };
    tracing::info!("browser: agent profile in memory (WebKit non-persistent store)");
    STATE.with_borrow_mut(|s| s.agent_store = Some(store.clone()));
    store
}

/// Opens browser `id` in `parent` at `url` (a reopen replaces its page).
pub fn open(id: &str, parent: Retained<NSView>, url: String, agent_profile: bool) {
    let Ok(mtm) = mtm() else { return };
    if STATE.with_borrow(|s| s.down) {
        return;
    }
    close(id);
    // The one URL gate: a saved or restored block can name anything.
    let url = if agent::allowed_url(&url) {
        url
    } else {
        tracing::warn!("browser: {id} opens on about:blank, not {}: only http, https and about:blank pages open", agent::loggable(&url), id = super::log_safe(id));
        BLANK.to_owned()
    };
    let generation = STATE.with_borrow_mut(|s| {
        s.next_generation += 1;
        s.next_generation
    });
    let delegate = Delegate::new(mtm, id, generation);
    let store = if agent_profile {
        agent_store(mtm)
    } else {
        // SAFETY: a main-thread WebKit constructor.
        unsafe { WKWebsiteDataStore::defaultDataStore(mtm) }
    };
    // SAFETY: WebKit's own configuration and view constructors, on the main
    // thread; the delegate outlives the view's weak references to it (both
    // live in STATE until `close` detaches them).
    let web = unsafe {
        let config = WKWebViewConfiguration::new(mtm);
        config.setWebsiteDataStore(&store);
        if agent_profile {
            let ucc = config.userContentController();
            let page_world = WKContentWorld::pageWorld(mtm);
            let relay = WKUserScript::initWithSource_injectionTime_forMainFrameOnly_inContentWorld(
                WKUserScript::alloc(mtm),
                &ns(CONSOLE_RELAY),
                WKUserScriptInjectionTime::AtDocumentStart,
                true,
                &page_world,
            );
            ucc.addUserScript(&relay);
            ucc.addScriptMessageHandler_contentWorld_name(ProtocolObject::from_ref(&*delegate), &page_world, &ns("herdrConsole"));
        }
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), parent.bounds().size);
        let web = WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config);
        web.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        web.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        for key in OBSERVED {
            web.addObserver_forKeyPath_options_context(&delegate, &ns(key), NSKeyValueObservingOptions::New, std::ptr::null_mut());
        }
        web
    };
    parent.addSubview(&web);
    let page = Page { url: url.clone(), loading: true, ..Page::default() };
    STATE.with_borrow_mut(|s| {
        s.pages.insert(
            id.to_owned(),
            Wk { generation, web: web.clone(), delegate, agent_profile, page: page.clone(), loader: 0, user_agent: String::new(), buttons_down: false, waiting: Vec::new(), size: None, growing: false },
        )
    });
    send(Event::Page { browser: id.to_owned(), page });
    tracing::info!("browser: creating {id} at {}{} (WebKit)", agent::loggable(&url), if agent_profile { " (agent profile)" } else { "" }, id = super::log_safe(id));
    if let Some(r) = request(&url) {
        // SAFETY: a main-thread call on a live view.
        unsafe { web.loadRequest(&r) };
    }
}

/// Closes browser `id`'s page and detaches everything that points at it.
pub fn close(id: &str) {
    let Some(wk) = STATE.with_borrow_mut(|s| s.pages.remove(id)) else { return };
    let (web, delegate) = (wk.web, wk.delegate);
    // SAFETY: main-thread calls on a live view; the observers were added in `open`.
    unsafe {
        for key in OBSERVED {
            web.removeObserver_forKeyPath(&delegate, &ns(key));
        }
        web.setNavigationDelegate(None);
        web.setUIDelegate(None);
        web.configuration().userContentController().removeAllScriptMessageHandlers();
        web.stopLoading();
    }
    web.removeFromSuperview();
    tracing::info!("browser: closed {} (WebKit)", super::log_safe(id));
}

pub fn navigate(id: &str, url: &str) -> Result<(), String> {
    if !agent::allowed_url(url) {
        tracing::warn!("browser: {id} not sent to {}: only http, https and about:blank pages open", agent::loggable(url), id = super::log_safe(id));
        return Err(format!("only http, https and about:blank pages open, not {}", agent::loggable(url)));
    }
    let web = need(id)?;
    let r = request(url).ok_or("not a url WebKit can load")?;
    // SAFETY: a main-thread call on a live view.
    unsafe { web.loadRequest(&r) };
    Ok(())
}

/// -1 back, 1 forward.
pub fn history(id: &str, step: i32) -> Result<(), String> {
    let web = need(id)?;
    // SAFETY: main-thread calls on a live view.
    unsafe {
        match step {
            -1 if web.canGoBack() => drop(web.goBack()),
            1 if web.canGoForward() => drop(web.goForward()),
            -1 => return Err(format!("browser {id} has nothing to go back to")),
            _ => return Err(format!("browser {id} has nothing to go forward to")),
        }
    }
    Ok(())
}

pub fn reload(id: &str) -> Result<(), String> {
    // SAFETY: a main-thread call on a live view.
    drop(unsafe { need(id)?.reload() });
    Ok(())
}

/// Makes the page inspectable from Safari's Develop menu, or not (None
/// toggles). WebKit's inspector opens in Safari, not in Herdr: the window
/// shows `DEVTOOLS_NOTE`. No port: Safari reaches the view through the OS.
pub fn devtools(id: &str, open: Option<bool>) -> Result<bool, String> {
    let web = need(id)?;
    // SAFETY: main-thread calls on a live view (macOS 13.3 and later).
    let want = open.unwrap_or(!unsafe { web.isInspectable() });
    unsafe { web.setInspectable(want) };
    tracing::info!("browser: {} is {}inspectable from Safari's Develop menu", super::log_safe(id), if want { "" } else { "no longer " });
    Ok(want)
}

/// The page's zoom in percent (WebKit's `pageZoom`, kept per pane).
pub fn zoom(id: &str) -> Result<f64, String> {
    // SAFETY: a main-thread read on a live view.
    Ok(unsafe { need(id)?.pageZoom() } * 100.0)
}

pub fn set_zoom(id: &str, percent: f64) -> Result<f64, String> {
    let web = need(id)?;
    // SAFETY: a main-thread call on a live view.
    unsafe { web.setPageZoom(percent / 100.0) };
    let now = unsafe { web.pageZoom() } * 100.0;
    changed(id, |p| p.zoom = now);
    // A viewport keeps its CSS size at the new zoom.
    place_page(id);
    Ok(now)
}

/// The page lays out at the viewport's size in CSS pixels, top-centred in
/// its block and cut off where the block ends (None: it fills its block
/// again; `translate::page_frame`). Size only: WebKit has no public
/// pixel-ratio, mobile or touch emulation.
pub fn set_viewport(id: &str, viewport: Option<Viewport>) -> Result<(), String> {
    need(id)?;
    let size = viewport.as_ref().map(|v| (f64::from(v.width), f64::from(v.height)));
    STATE.with_borrow_mut(|s| {
        if let Some(p) = s.pages.get_mut(id) {
            p.size = size;
        }
    });
    tracing::info!("browser: {} viewport {} (size only on WebKit)", super::log_safe(id), size.map_or_else(|| "fit".to_owned(), |(w, h)| format!("{w}x{h}")));
    changed(id, |p| p.viewport = viewport);
    place_page(id);
    Ok(())
}

/// The block placed `id`'s container at `size` this frame (view.rs).
pub fn fit(id: &str, _size: NSSize, _resized: bool) {
    place_page(id);
}

/// Puts the page in its container: filling it, or at its viewport's size.
fn place_page(id: &str) {
    let Some((web, size, growing)) = STATE.with_borrow(|s| s.pages.get(id).map(|p| (p.web.clone(), p.size, p.growing))) else { return };
    if growing {
        return;
    }
    // SAFETY: main-thread reads of the view tree.
    let Some(parent) = (unsafe { web.superview() }) else { return };
    let room = parent.bounds().size;
    // SAFETY: a main-thread read of a live view.
    let zoom = unsafe { web.pageZoom() };
    if size.is_some() && !clips(&parent) {
        set_clips(&parent);
    }
    let (x, y, w, h) = translate::page_frame((room.width, room.height), size, zoom);
    let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
    let had = web.frame();
    if had.origin != frame.origin || had.size != frame.size {
        web.setFrame(frame);
    }
}

fn clips(view: &NSView) -> bool {
    // SAFETY: `clipsToBounds` exists on every macOS WebKit's engine runs on
    // here (public since 14, there since 10.9); checked first anyway.
    let responds: bool = unsafe { msg_send![view, respondsToSelector: sel!(clipsToBounds)] };
    responds && unsafe { msg_send![view, clipsToBounds] }
}

fn set_clips(view: &NSView) {
    // SAFETY: as `clips`.
    if unsafe { msg_send![view, respondsToSelector: sel!(setClipsToBounds:)] } {
        let _: () = unsafe { msg_send![view, setClipsToBounds: true] };
    }
}

/// An agent's connection to `id` ended: the page reopens at its url in a new
/// view, so nothing the agent set (a user agent, a viewport, object handles
/// in the isolated world) stays. The agent profile's cookies and storage do.
pub fn agent_gone(id: &str) {
    let Some((url, agent_profile)) = STATE.with_borrow(|s| s.pages.get(id).map(|p| (p.page.url.clone(), p.agent_profile))) else { return };
    let Some(parent) = view::existing(id) else { return };
    tracing::info!("browser: {id}'s agent session ended; reopening it at {} so nothing the agent set stays", agent::loggable(&url), id = super::log_safe(id));
    open(id, parent, url, agent_profile);
}

/// Records a page change and tells the window.
fn changed(id: &str, change: impl FnOnce(&mut Page)) {
    let page = STATE.with_borrow_mut(|s| {
        let p = s.pages.get_mut(id)?;
        change(&mut p.page);
        Some(p.page.clone())
    });
    if let Some(page) = page {
        send(Event::Page { browser: id.to_owned(), page });
    }
}

/// The view's title, url, loading and history state, read again (KVO said one changed).
fn refresh(id: &str, generation: u64) {
    let Some(web) = current(id, generation) else { return };
    // SAFETY: main-thread reads of a live view.
    let (title, url, loading, back, forward, zoom) = unsafe {
        (
            web.title().map(|t| t.to_string()).unwrap_or_default(),
            web.URL().and_then(|u| u.absoluteString()).map(|u| u.to_string()),
            web.isLoading(),
            web.canGoBack(),
            web.canGoForward(),
            web.pageZoom() * 100.0,
        )
    };
    let moved = STATE.with_borrow(|s| s.pages.get(id).is_some_and(|p| url.as_ref().is_some_and(|u| *u != p.page.url)));
    if moved && let Some(u) = &url {
        proxy::set_url(id, u);
    }
    changed(id, |p| {
        p.title = title;
        if let Some(u) = url {
            p.url = u;
        }
        p.loading = loading;
        p.can_go_back = back;
        p.can_go_forward = forward;
        p.zoom = zoom;
    });
}

// ---- agents: one allowed CDP message --------------------------------------------------

/// One DevTools message (JSON, already allowed by the proxy) for `id`'s
/// page: answered now or when WebKit is done. False: no page now.
pub fn send_devtools(id: &str, message: &[u8]) -> bool {
    let Some((web, generation)) = STATE.with_borrow(|s| s.pages.get(id).map(|p| (p.web.clone(), p.generation))) else { return false };
    let Ok(Value::Object(msg)) = serde_json::from_slice::<Value>(message) else { return true };
    let Some(pid) = msg.get("id").and_then(Value::as_i64) else { return true };
    let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
    let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
    let step = STATE.with_borrow(|s| {
        let p = s.pages.get(id)?;
        let ctx = translate::Ctx { url: &p.page.url, loader: p.loader, user_agent: &p.user_agent, buttons_down: p.buttons_down };
        Some(translate::plan(method, &params, &ctx))
    });
    let Some(step) = step else { return false };
    run(id, generation, &web, pid, step, &params);
    true
}

fn reply(id: &str, pid: i64, result: Result<Value, String>) {
    proxy::from_page(id, translate::answer(pid, result).to_string().as_bytes());
}

fn emit(id: &str, event: &Value) {
    proxy::from_page(id, event.to_string().as_bytes());
}

fn run(id: &str, generation: u64, web: &WKWebView, pid: i64, step: Step, params: &Value) {
    let owned = id.to_owned();
    let done = move |r: Result<Value, String>| reply(&owned, pid, r);
    match step {
        Step::Reply(v) => done(Ok(v)),
        Step::Fail(e) => done(Err(e)),
        Step::World(op @ ("evaluate" | "callFunctionOn")) => world_code(web, op, params, done),
        Step::World(op) => in_world(web, op, params, done),
        Step::AxTree => in_world(web, "getFullAXTree", params, move |r| done(r.and_then(|raw| translate::ax_tree(&raw)))),
        Step::PageEval => page_eval(web, params, done),
        Step::Navigate(url) => {
            let Some(r) = request(&url).filter(|_| agent::allowed_url(&url)) else {
                return done(Err(format!("only http, https and about:blank pages open, not {}", agent::loggable(&url))));
            };
            STATE.with_borrow_mut(|s| s.pages.get_mut(id).map(|p| p.waiting.push(pid)));
            // SAFETY: a main-thread call on a live view.
            unsafe { web.loadRequest(&r) };
            let owned = id.to_owned();
            after(NAVIGATE_WAIT, move || answer_navigations(&owned, generation, Some(pid), None));
        }
        Step::Reload => {
            // Answered at once: agent-browser waits for the load event itself.
            // SAFETY: a main-thread call on a live view.
            drop(unsafe { web.reload() });
            done(Ok(json!({})));
        }
        Step::Mouse(m) => {
            let r = mouse(web, &m);
            let down = match m.kind {
                MouseKind::Down => Some(true),
                MouseKind::Up => Some(false),
                _ => None,
            };
            if let Some(down) = down {
                STATE.with_borrow_mut(|s| s.pages.get_mut(id).map(|p| p.buttons_down = down));
            }
            done(r.map(|()| json!({})));
        }
        Step::Key(k) => done(key(web, &k).map(|()| json!({}))),
        Step::InsertText(text) => {
            // SAFETY: NSResponder's `insertText:`, which WKWebView implements, on the main thread.
            let _: () = unsafe { msg_send![web, insertText: &*ns(&text)] };
            done(Ok(json!({})));
        }
        Step::Screenshot(shot) => screenshot(id, web, shot, done),
        Step::Viewport(size) => {
            let v = size.map(|(width, height)| Viewport { width, height, mobile: false, scale_factor: 0.0, preset: None });
            done(set_viewport(id, v).map(|()| json!({})));
        }
        Step::UserAgent(ua) => {
            // SAFETY: a main-thread call on a live view.
            unsafe { web.setCustomUserAgent(ua.as_deref().map(ns).as_deref()) };
            done(Ok(json!({})));
        }
    }
}

/// Answers `Page.navigate` requests waiting on `id`: `only` that one (its
/// wait ran out), or all of them (the load ended, with `error` if it failed).
fn answer_navigations(id: &str, generation: u64, only: Option<i64>, error: Option<&str>) {
    let taken = STATE.with_borrow_mut(|s| {
        let p = s.pages.get_mut(id).filter(|p| p.generation == generation)?;
        let taken: Vec<i64> = match only {
            Some(pid) => p.waiting.iter().position(|w| *w == pid).map(|i| vec![p.waiting.remove(i)]).unwrap_or_default(),
            None => std::mem::take(&mut p.waiting),
        };
        Some((taken, p.loader))
    });
    let Some((taken, loader)) = taken else { return };
    for pid in taken {
        reply(id, pid, Ok(translate::navigated(loader, error)));
    }
}

/// WebKit's JavaScript error codes (`WKErrorDomain`).
const JS_THREW: isize = 4;
const JS_RESULT_UNSUPPORTED: isize = 5;

/// The script's own message for a JavaScript error ("SyntaxError: …"), else
/// WebKit's description.
fn js_error(e: &NSError) -> String {
    let message = e.userInfo().objectForKey(&ns("WKJavaScriptExceptionMessage"));
    message.as_deref().and_then(|m| m.downcast_ref::<NSString>()).map_or_else(|| e.localizedDescription().to_string(), |m| m.to_string())
}

/// Runs `body` (a function body; `return` gives its result) in `world`;
/// `done` gets the JSON its string result holds, or WebKit's error
/// (`js_error`: a body that didn't compile says "SyntaxError").
fn call_js(web: &WKWebView, body: &str, world: &WKContentWorld, done: impl FnOnce(Result<Value, String>) + 'static) {
    let done = Cell::new(Some(done));
    let block = RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
        let Some(done) = done.take() else { return };
        // SAFETY: WebKit hands the completion a valid object or null for each.
        if let Some(e) = unsafe { error.as_ref() } {
            return done(Err(js_error(e)));
        }
        let text = unsafe { result.as_ref() }.and_then(|r| r.downcast_ref::<NSString>()).map(|s| s.to_string());
        done(match text {
            Some(t) => serde_json::from_str(&t).map_err(|e| format!("the page answered with something that isn't JSON: {e}")),
            None => Ok(Value::Null),
        });
    });
    // SAFETY: a main-thread call on a live view; no arguments dictionary
    // (the body carries its parameters as JSON), the main frame.
    unsafe { web.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(&ns(body), None, None, world, Some(&block)) };
}

/// The isolated world, made once and kept: WebKit holds a named world only
/// while someone does, and a new one has none of the last one's script
/// state (the node ids an agent got from a snapshot).
fn isolated_world(mtm: MainThreadMarker) -> Retained<WKContentWorld> {
    if let Some(w) = STATE.with_borrow(|s| s.world.clone()) {
        return w;
    }
    // SAFETY: a main-thread WebKit constructor.
    let world = unsafe { WKContentWorld::worldWithName(&ns(WORLD), mtm) };
    STATE.with_borrow_mut(|s| s.world = Some(world.clone()));
    world
}

/// The isolated world's call of `__cdp[op](params, got)`, installing the
/// helper first in a document that doesn't have it yet. The params go in as
/// a JSON literal; `pre` (agent code, `translate::spliced`) runs first and
/// leaves `got`.
fn world_body(op: &str, params: &Value, pre: Option<&str>) -> String {
    let (pre, got) = pre.map_or(("", "undefined"), |p| (p, translate::GOT));
    format!(
        "{pre}const __herdr_p = {params};\nif (!globalThis.__cdp) {{\n{RUNTIME}\n}}\ntry {{ return JSON.stringify(await globalThis.__cdp[{op:?}](__herdr_p, {got})); }} catch (e) {{ return JSON.stringify({{ __error: String(e && e.message || e) }}); }}"
    )
}

/// What the helper answered: its result, or the error it reported.
fn helper_answer(v: Value) -> Result<Value, String> {
    match v.get("__error").and_then(Value::as_str) {
        Some(e) => Err(e.to_owned()),
        None => Ok(v),
    }
}

/// `__cdp[op](params)` in the isolated world (`world_body`).
fn in_world(web: &WKWebView, op: &'static str, params: &Value, done: impl FnOnce(Result<Value, String>) + 'static) {
    let Ok(mtm) = mtm() else { return done(Err("not on the main thread".into())) };
    let world = isolated_world(mtm);
    call_js(web, &world_body(op, params, None), &world, move |r| done(r.and_then(helper_answer)));
}

/// The agent's code in `params[key]`.
fn agent_code(params: &Value, key: &str) -> Result<String, String> {
    params.get(key).and_then(Value::as_str).map(str::to_owned).ok_or_else(|| format!("Invalid parameters: {key} must be a string"))
}

/// `Runtime.evaluate` (`op` "evaluate") or `Runtime.callFunctionOn` in the
/// isolated world: the agent's code is spliced into the call
/// (`translate::spliced`), so a page's CSP doesn't apply. An expression
/// that is a statement list runs as a script instead (`run_script`, by
/// value); a function that doesn't compile is CDP's exception.
fn world_code(web: &WKWebView, op: &'static str, params: &Value, done: impl FnOnce(Result<Value, String>) + 'static) {
    let Ok(mtm) = mtm() else { return done(Err("not on the main thread".into())) };
    let key = if op == "evaluate" { "expression" } else { "functionDeclaration" };
    let code = match agent_code(params, key) {
        Ok(c) => c,
        Err(e) => return done(Err(e)),
    };
    let world = isolated_world(mtm);
    let body = world_body(op, params, Some(&translate::spliced(&code)));
    let web_ref: Retained<WKWebView> = web.retain();
    call_js(web, &body, &world.clone(), move |r| match r {
        Err(e) if translate::is_syntax_error(&e) && op == "evaluate" => run_script(&web_ref, &code, &world, done),
        Err(e) if translate::is_syntax_error(&e) => done(Ok(translate::thrown(&e))),
        r => done(r.and_then(helper_answer)),
    });
}

/// `Runtime.evaluate` by value, in the page's own world, as CDP's default
/// context: the agent runs script as the page. The expression is spliced
/// into the call (`translate::spliced`), so a page's CSP doesn't apply; a
/// statement list runs as a script instead (`run_script`).
fn page_eval(web: &WKWebView, params: &Value, done: impl FnOnce(Result<Value, String>) + 'static) {
    let Ok(mtm) = mtm() else { return done(Err("not on the main thread".into())) };
    let code = match agent_code(params, "expression") {
        Ok(c) => c,
        Err(e) => return done(Err(e)),
    };
    let wait = params.get("awaitPromise").and_then(Value::as_bool).unwrap_or(false);
    let got = translate::GOT;
    let body = format!(
        r#"{pre}const __herdr_ser = (v) => {{ const t = typeof v; if (v === undefined) return {{ type: "undefined" }}; if (v === null) return {{ type: "object", subtype: "null", value: null }};
  if (t === "string" || t === "boolean" || t === "number") return {{ type: t, value: v }}; let value; try {{ value = JSON.parse(JSON.stringify(v)); }} catch (_) {{}} return {{ type: t === "function" ? "function" : "object", value }}; }};
try {{ if ("err" in {got}) throw {got}.err; let v = {got}.ok; if ({wait}) v = await v; return JSON.stringify({{ result: __herdr_ser(v) }}); }}
catch (e) {{ const d = e instanceof Error ? e.name + ": " + e.message : String(e); return JSON.stringify({{ result: {{ type: "object", subtype: "error", description: d }}, exceptionDetails: {{ exceptionId: 1, text: "Uncaught", lineNumber: 0, columnNumber: 0, exception: {{ type: "object", subtype: "error", description: d }} }} }}); }}"#,
        pre = translate::spliced(&code),
    );
    // SAFETY: a main-thread WebKit constructor.
    let world = unsafe { WKContentWorld::pageWorld(mtm) };
    let web_ref: Retained<WKWebView> = web.retain();
    call_js(web, &body, &world.clone(), move |r| match r {
        Err(e) if translate::is_syntax_error(&e) => run_script(&web_ref, &code, &world, done),
        r => done(r),
    });
}

/// Runs the agent's `script` in `world` as a script, not inside a function
/// body, so a statement list gives its completion value (by value: WebKit
/// passes back only JSON-like values). WebKit compiles it itself: the
/// page's CSP doesn't apply. No `await` at its top level, as in CDP.
fn run_script(web: &WKWebView, script: &str, world: &WKContentWorld, done: impl FnOnce(Result<Value, String>) + 'static) {
    let done = Cell::new(Some(done));
    let block = RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
        let Some(done) = done.take() else { return };
        // SAFETY: WebKit hands the completion a valid object or null for each.
        if let Some(e) = unsafe { error.as_ref() } {
            let webkit = e.domain().to_string() == "WKErrorDomain";
            return done(match e.code() {
                JS_THREW if webkit => Ok(translate::thrown(&js_error(e))),
                JS_RESULT_UNSUPPORTED if webkit => Ok(translate::script_opaque()),
                _ => Err(js_error(e)),
            });
        }
        done(Ok(match unsafe { result.as_ref() } {
            None => translate::script_result(None),
            Some(v) => to_json(v).map_or_else(translate::script_opaque, |v| translate::script_result(Some(v))),
        }));
    });
    // SAFETY: a main-thread call on a live view, the main frame.
    unsafe { web.evaluateJavaScript_inFrame_inContentWorld_completionHandler(&ns(script), None, world, Some(&block)) };
}

/// A value WebKit passed back (strings, numbers, arrays, dictionaries, null)
/// as JSON; None for anything JSON can't hold (a date, NaN).
fn to_json(value: &AnyObject) -> Option<Value> {
    // In an array: `isValidJSONObject` checks the whole tree and never
    // throws, where `dataWithJSONObject` raises on what it can't write.
    let wrapped = NSArray::from_slice(&[value]);
    // SAFETY: an NSArray is a valid argument for both.
    if !unsafe { NSJSONSerialization::isValidJSONObject(&wrapped) } {
        return None;
    }
    let data = unsafe { NSJSONSerialization::dataWithJSONObject_options_error(&wrapped, NSJSONWritingOptions::empty()) }.ok()?;
    match serde_json::from_slice::<Value>(&data.to_vec()).ok()? {
        Value::Array(mut a) if a.len() == 1 => a.pop(),
        _ => None,
    }
}

fn flags(m: Mods) -> NSEventModifierFlags {
    let mut f = NSEventModifierFlags::empty();
    for (on, flag) in [(m.alt, NSEventModifierFlags::Option), (m.ctrl, NSEventModifierFlags::Control), (m.cmd, NSEventModifierFlags::Command), (m.shift, NSEventModifierFlags::Shift)] {
        if on {
            f |= flag;
        }
    }
    f
}

fn uptime() -> f64 {
    NSProcessInfo::processInfo().systemUptime()
}

/// An agent's mouse event, as an `NSEvent` sent straight to the view: the
/// page sees trusted input, and Herdr's keyboard focus doesn't move.
fn mouse(web: &WKWebView, m: &Mouse) -> Result<(), String> {
    let window = web.window().ok_or("the page isn't in a window")?;
    // CDP's CSS pixels are points times the page's zoom.
    let zoom = unsafe { web.pageZoom() };
    let (x, y) = (m.x * zoom, m.y * zoom);
    let y = if web.isFlipped() { y } else { web.bounds().size.height - y };
    let at = web.convertPoint_toView(NSPoint::new(x, y), None);
    use NSEventType as T;
    let kind = match (m.kind, m.button) {
        (MouseKind::Down, Button::Left) => T::LeftMouseDown,
        (MouseKind::Down, Button::Right) => T::RightMouseDown,
        (MouseKind::Down, Button::Other) => T::OtherMouseDown,
        (MouseKind::Up, Button::Left) => T::LeftMouseUp,
        (MouseKind::Up, Button::Right) => T::RightMouseUp,
        (MouseKind::Up, Button::Other) => T::OtherMouseUp,
        (MouseKind::Drag, Button::Right) => T::RightMouseDragged,
        (MouseKind::Drag, Button::Other) => T::OtherMouseDragged,
        (MouseKind::Drag, Button::Left) => T::LeftMouseDragged,
        (MouseKind::Move | MouseKind::Wheel, _) => T::MouseMoved,
    };
    let pressure = if matches!(m.kind, MouseKind::Down | MouseKind::Drag) { 1.0 } else { 0.0 };
    let ev = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind,
        at,
        flags(m.mods),
        uptime(),
        window.windowNumber(),
        None,
        0,
        m.clicks,
        pressure,
    )
    .ok_or("AppKit made no mouse event")?;
    match kind {
        T::LeftMouseDown => web.mouseDown(&ev),
        T::RightMouseDown => web.rightMouseDown(&ev),
        T::OtherMouseDown => web.otherMouseDown(&ev),
        T::LeftMouseUp => web.mouseUp(&ev),
        T::RightMouseUp => web.rightMouseUp(&ev),
        T::OtherMouseUp => web.otherMouseUp(&ev),
        T::LeftMouseDragged => web.mouseDragged(&ev),
        T::RightMouseDragged => web.rightMouseDragged(&ev),
        T::OtherMouseDragged => web.otherMouseDragged(&ev),
        _ => web.mouseMoved(&ev),
    }
    Ok(())
}

/// Whether `event` is an agent's key-down (`key`) on its way out of the page.
/// WebKit sends a key-down the page didn't handle (no editable focus, no
/// `preventDefault`) back through `[NSApp sendEvent:]`
/// (`WebViewImpl::doneWithKeyEvent`), to the key window's first responder:
/// Herdr's terminal, a keys.json action, the menus. view.rs's monitor drops
/// it there. CEF has no such path.
pub fn agent_event(event: &NSEvent) -> bool {
    AGENT_KEYS.with_borrow_mut(|keys| {
        keys.retain(|k| k.load().is_some());
        keys.iter().any(|k| k.load().is_some_and(|e| std::ptr::eq(&*e, event)))
    })
}

/// An agent's key event, as an `NSEvent` sent straight to the view. A
/// key-down is noted first (`agent_event`), so it can't leave the page.
fn key(web: &WKWebView, k: &Key) -> Result<(), String> {
    let window = web.window().ok_or("the page isn't in a window")?;
    let ev = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        if k.up { NSEventType::KeyUp } else { NSEventType::KeyDown },
        NSPoint::new(0.0, 0.0),
        flags(k.mods),
        uptime(),
        window.windowNumber(),
        None,
        &ns(&k.chars),
        &ns(&k.plain),
        false,
        k.code,
    )
    .ok_or("AppKit made no key event")?;
    if k.up {
        web.keyUp(&ev);
    } else {
        AGENT_KEYS.with_borrow_mut(|keys| {
            keys.retain(|k| k.load().is_some());
            keys.push(Weak::from_retained(&ev));
        });
        web.keyDown(&ev);
    }
    Ok(())
}

/// `Page.captureScreenshot`: WebKit's snapshot of the view (a clip past the
/// visible page lays the page out taller first, then back), as base64.
fn screenshot(id: &str, web: &WKWebView, shot: Shot, done: impl FnOnce(Result<Value, String>) + 'static) {
    let Ok(mtm) = mtm() else { return done(Err("not on the main thread".into())) };
    // SAFETY: main-thread reads of a live view.
    let zoom = unsafe { web.pageZoom() };
    let view = web.bounds().size;
    if shot.too_big((view.width / zoom, view.height / zoom), zoom) {
        return done(Err("the screenshot is too large: clip it to under 32 million pixels".into()));
    }
    let restore = shot.grow_to((view.width / zoom, view.height / zoom)).map(|(w, h)| {
        STATE.with_borrow_mut(|s| s.pages.get_mut(id).map(|p| p.growing = true));
        let old = web.frame();
        web.setFrame(NSRect::new(old.origin, NSSize::new(w * zoom, h * zoom)));
        old
    });
    // SAFETY: a main-thread WebKit constructor and setters.
    let config = unsafe { WKSnapshotConfiguration::new(mtm) };
    if let Some((x, y, w, h)) = shot.clip {
        unsafe { config.setRect(NSRect::new(NSPoint::new(x * zoom, y * zoom), NSSize::new(w * zoom, h * zoom))) };
    }
    let owned = id.to_owned();
    let web_ref: Retained<WKWebView> = web.retain();
    let done = Cell::new(Some(done));
    let block = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
        if restore.is_some() {
            STATE.with_borrow_mut(|s| s.pages.get_mut(&owned).map(|p| p.growing = false));
            if let Some(old) = restore {
                web_ref.setFrame(old);
            }
            place_page(&owned);
        }
        let Some(done) = done.take() else { return };
        // SAFETY: WebKit hands the completion a valid object or null for each.
        if let Some(e) = unsafe { error.as_ref() } {
            return done(Err(format!("WebKit took no snapshot: {}", e.localizedDescription())));
        }
        let Some(image) = (unsafe { image.as_ref() }) else { return done(Err("WebKit took no snapshot".into())) };
        done(encode(image, shot.jpeg).map(|data| json!({ "data": data })));
    });
    // SAFETY: a main-thread call on a live view.
    unsafe { web.takeSnapshotWithConfiguration_completionHandler(Some(&config), &block) };
}

/// An image as base64 PNG (or JPEG).
fn encode(image: &NSImage, jpeg: bool) -> Result<String, String> {
    let tiff = image.TIFFRepresentation().ok_or("the snapshot has no bitmap")?;
    let rep = NSBitmapImageRep::imageRepWithData(&tiff).ok_or("the snapshot has no bitmap")?;
    let kind = if jpeg { NSBitmapImageFileType::JPEG } else { NSBitmapImageFileType::PNG };
    // SAFETY: an empty properties dictionary of the declared type.
    let data: Retained<NSData> = unsafe { rep.representationUsingType_properties(kind, &NSDictionary::new()) }.ok_or("the snapshot didn't encode")?;
    Ok(data.base64EncodedStringWithOptions(NSDataBase64EncodingOptions::empty()).to_string())
}

// ---- WebKit's callbacks ----------------------------------------------------------------------

struct Ivars {
    id: String,
    generation: u64,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; Delegate has no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "HerdrWebKitPage"]
    #[ivars = Ivars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl WKNavigationDelegate for Delegate {
        /// The URL gate for the main frame (`translate::may_load`), and no downloads.
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_action(&self, _web: &WKWebView, action: &WKNavigationAction, handler: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>) {
            // SAFETY: main-thread reads of the action WebKit passed in.
            let (url, target, download) = unsafe {
                let url = action.request().URL().and_then(|u| u.absoluteString()).map(|u| u.to_string()).unwrap_or_default();
                (url, action.targetFrame().map(|f| f.isMainFrame()), action.shouldPerformDownload())
            };
            let id = super::log_safe(&self.ivars().id);
            let policy = if download {
                tracing::info!("browser: {id} download of {} refused (no downloads on WebKit)", agent::loggable(&url));
                WKNavigationActionPolicy::Cancel
            } else if !translate::may_load(&url, target) {
                tracing::warn!("browser: {id} not sent to {}: only http, https and about:blank pages open", agent::loggable(&url));
                WKNavigationActionPolicy::Cancel
            } else {
                WKNavigationActionPolicy::Allow
            };
            handler.call((policy,));
        }

        /// A response the view can't show would be a download: refused.
        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn decide_response(&self, _web: &WKWebView, response: &WKNavigationResponse, handler: &block2::DynBlock<dyn Fn(WKNavigationResponsePolicy)>) {
            // SAFETY: a main-thread read of the response WebKit passed in.
            let policy = if unsafe { response.canShowMIMEType() } {
                WKNavigationResponsePolicy::Allow
            } else {
                tracing::info!("browser: {} download refused (no downloads on WebKit)", super::log_safe(&self.ivars().id));
                WKNavigationResponsePolicy::Cancel
            };
            handler.call((policy,));
        }

        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        fn did_start(&self, _web: &WKWebView, _navigation: Option<&WKNavigation>) {
            let Ivars { id, generation } = self.ivars();
            if current(id, *generation).is_some() {
                emit(id, &translate::started());
            }
        }

        #[unsafe(method(webView:didCommitNavigation:))]
        fn did_commit(&self, web: &WKWebView, _navigation: Option<&WKNavigation>) {
            let Ivars { id, generation } = self.ivars();
            // SAFETY: a main-thread read of a live view.
            let url = unsafe { web.URL() }.and_then(|u| u.absoluteString()).map(|u| u.to_string()).unwrap_or_default();
            let loader = STATE.with_borrow_mut(|s| {
                let p = s.pages.get_mut(id).filter(|p| p.generation == *generation)?;
                p.loader += 1;
                Some(p.loader)
            });
            let Some(loader) = loader else { return };
            proxy::set_url(id, &url);
            for e in translate::committed(&url, loader) {
                emit(id, &e);
            }
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, web: &WKWebView, _navigation: Option<&WKNavigation>) {
            let Ivars { id, generation } = self.ivars();
            if current(id, *generation).is_none() {
                return;
            }
            for e in translate::finished(now_secs()) {
                emit(id, &e);
            }
            answer_navigations(id, *generation, None, None);
            if STATE.with_borrow(|s| s.pages.get(id).is_some_and(|p| p.user_agent.is_empty())) {
                user_agent(id, *generation, web);
            }
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail(&self, _web: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            self.failed(error);
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional(&self, _web: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            self.failed(error);
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn crashed(&self, web: &WKWebView) {
            let Ivars { id, generation } = self.ivars();
            if current(id, *generation).is_some() {
                tracing::warn!("browser: {}'s web content process ended; reloading", super::log_safe(id));
                // SAFETY: a main-thread call on a live view.
                drop(unsafe { web.reload() });
            }
        }
    }

    unsafe impl WKUIDelegate for Delegate {
        /// No new windows: a popup or a new-tab link opens in this pane, if
        /// the popup rule (`agent::popup_allowed`) lets it.
        #[unsafe(method(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn popup(&self, web: &WKWebView, _config: &WKWebViewConfiguration, action: &WKNavigationAction, _features: &WKWindowFeatures) -> *mut WKWebView {
            let Ivars { id, generation } = self.ivars();
            if current(id, *generation).is_none() {
                return std::ptr::null_mut();
            }
            // SAFETY: main-thread reads of the action WebKit passed in and of a live view.
            let (url, frame_url, is_main, main_url, gesture) = unsafe {
                let url = action.request().URL().and_then(|u| u.absoluteString()).map(|u| u.to_string()).unwrap_or_default();
                let source = action.sourceFrame();
                let frame_url = source.as_ref().and_then(|f| f.request().URL()).and_then(|u| u.absoluteString()).map(|u| u.to_string()).unwrap_or_default();
                let is_main = source.as_ref().is_some_and(|f| f.isMainFrame());
                let main_url = web.URL().and_then(|u| u.absoluteString()).map(|u| u.to_string()).unwrap_or_default();
                // WebKit reports no gesture flag; a clicked link is one.
                (url, frame_url, is_main, main_url, action.navigationType() == WKNavigationType::LinkActivated)
            };
            if url.is_empty() {
                return std::ptr::null_mut();
            }
            if agent::popup_allowed(&url, &frame_url, &main_url, is_main, gesture) {
                tracing::info!("browser: {} popup {} opens in its pane", super::log_safe(id), agent::loggable(&url));
                // After this callback returns, not inside it.
                let (id, generation) = (id.clone(), *generation);
                after(Duration::ZERO, move || {
                    if current(&id, generation).is_some() {
                        let _ = navigate(&id, &url);
                    }
                });
            } else {
                tracing::warn!(
                    "browser: {} popup to {} dropped (link click {gesture}, main frame {is_main}): only http, https and about:blank, and a cross-origin frame needs a click",
                    super::log_safe(id), agent::loggable(&url)
                );
            }
            std::ptr::null_mut()
        }

        /// The page closed itself (`window.close()`): its pane goes.
        #[unsafe(method(webViewDidClose:))]
        fn did_close(&self, _web: &WKWebView) {
            let Ivars { id, generation } = self.ivars();
            if current(id, *generation).is_some() {
                tracing::info!("browser: {} asked to close", super::log_safe(id));
                send(Event::CloseAsked(id.clone()));
            }
        }
    }

    unsafe impl WKScriptMessageHandler for Delegate {
        /// The console relay (agent pages only): to the agent, if one is connected.
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn message(&self, _ucc: &WKUserContentController, message: &WKScriptMessage) {
            let Ivars { id, generation } = self.ivars();
            if current(id, *generation).is_none() {
                return;
            }
            // The relay runs in the main frame only, but the handler is
            // reachable from every frame: an iframe's post is not the page's.
            // SAFETY: main-thread reads of the message WebKit passed in.
            if !unsafe { message.frameInfo().isMainFrame() } {
                return;
            }
            let body = unsafe { message.body() };
            let Some(text) = body.downcast_ref::<NSString>().map(|s| s.to_string()) else { return };
            if let Some(event) = translate::console(&text, now_secs() * 1000.0) {
                emit(id, &event);
            }
        }
    }

    impl Delegate {
        /// KVO on the view's title, url, loading and history state.
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe(&self, _key: Option<&NSString>, _object: Option<&AnyObject>, _change: Option<&AnyObject>, _context: *mut c_void) {
            let Ivars { id, generation } = self.ivars();
            refresh(id, *generation);
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker, id: &str, generation: u64) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { id: id.to_owned(), generation });
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    fn failed(&self, error: &NSError) {
        let Ivars { id, generation } = self.ivars();
        if current(id, *generation).is_none() {
            return;
        }
        let text = error.localizedDescription().to_string();
        // NSURLErrorCancelled: a newer navigation replaced this load, which is normal.
        if error.code() == -999 && error.domain().to_string() == "NSURLErrorDomain" {
            tracing::debug!("browser: {} load cancelled by a newer one", super::log_safe(id));
        } else {
            tracing::warn!("browser: {} load error {}", super::log_safe(id), super::log_safe(&text));
        }
        emit(id, &json!({ "method": "Page.frameStoppedLoading", "params": { "frameId": translate::FRAME } }));
        answer_navigations(id, *generation, None, Some(&text));
    }
}

/// Reads the page's user agent once, for `Browser.getVersion`.
fn user_agent(id: &str, generation: u64, web: &WKWebView) {
    let owned = id.to_owned();
    let block = RcBlock::new(move |result: *mut AnyObject, _error: *mut NSError| {
        // SAFETY: WebKit hands the completion a valid object or null.
        let Some(ua) = unsafe { result.as_ref() }.and_then(|r| r.downcast_ref::<NSString>()).map(|s| s.to_string()) else { return };
        STATE.with_borrow_mut(|s| s.pages.get_mut(&owned).filter(|p| p.generation == generation).map(|p| p.user_agent = ua));
    });
    // SAFETY: a main-thread call on a live view.
    unsafe { web.evaluateJavaScript_completionHandler(&ns("navigator.userAgent"), Some(&block)) };
}
