//! CEF's lifetime in the app: loaded and started on the first browser, pumped
//! from GPUI's foreground executor, shut down at quit after every browser
//! has closed.
//!
//! macOS runs CEF's UI thread on the main thread, so every CEF call and
//! callback here happens there, except `on_schedule_message_pump_work`, which
//! may come from any thread and only sends on a channel.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{CString, c_int};
use std::ptr::NonNull;
use std::os::unix::ffi::OsStrExt as _;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use cef::*;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, Bool};
use objc2::{MainThreadMarker, msg_send};
use objc2_app_kit::NSView;

use super::agent;
use super::emulate::{self, Viewport};
use super::native::Event;
use super::proxy;
use super::registry::{self, Page};
use super::view;

const FRAMEWORK: &str = "Chromium Embedded Framework.framework/Chromium Embedded Framework";
/// CEF asks to be pumped at least this often (cef_app.h, CefDoMessageLoopWork).
const MAX_PUMP_DELAY: Duration = Duration::from_millis(33);
/// How long quit waits for browsers to close before giving up on a clean shutdown.
const CLOSE_DEADLINE: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Phase {
    #[default]
    Off,
    /// `cef::initialize` returned; waiting for `on_context_initialized`.
    Starting,
    Ready,
    Closing,
    Down,
    Failed,
}

/// One CEF browser: Herdr's id (a tab's), the open it belongs to, and CEF's own.
struct Open {
    id: String,
    generation: u64,
    browser: Browser,
}

#[derive(Default)]
struct State {
    phase: Phase,
    /// Browsers asked for before CEF's context was ready: (id, generation, parent, url).
    pending: Vec<(String, u64, Retained<NSView>, String)>,
    /// Asked of CEF, `on_after_created` not seen yet.
    creating: usize,
    /// Every CEF browser that exists, the ones closing included.
    browsers: Vec<Open>,
    /// Each wanted Herdr id's current open. A layout file can show the same id
    /// again while its last browser is still closing; the old browser's late
    /// callbacks then name an old generation and change nothing.
    current: HashMap<String, u64>,
    next_generation: u64,
    /// What each wanted page last said about itself.
    pages: HashMap<String, Page>,
    /// Popups allowed and not created yet: (opener's id, CEF's popup id, the popup's tab id).
    popups: Vec<(String, c_int, String)>,
    /// DevTools windows open (CEF's own; closed before shutdown).
    devtools: usize,
    /// Browsers Herdr is closing, by CEF identifier: their `do_close` is
    /// ours, not the page's.
    closing: Vec<c_int>,
    app: Option<App>,
    /// DevTools observers, by CEF identifier: one per browser an agent talked to.
    observers: HashMap<c_int, Registration>,
    /// The scale each page's viewport last fit its pane at (`refit`).
    fits: HashMap<String, f64>,
}

impl State {
    fn is_current(&self, id: &str, generation: u64) -> bool {
        self.current.get(id) == Some(&generation)
    }
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    static PUMPING: Cell<bool> = const { Cell::new(false) };
    static EVENTS: RefCell<Option<async_channel::Sender<Event>>> = const { RefCell::new(None) };
    /// Herdr's own DevTools request ids (`browser.viewport`): far above the
    /// agent proxy's, which count up from 1, so an answer to one of them is
    /// never an agent's (the proxy drops ids it didn't send).
    static OWN_ID: Cell<i64> = const { Cell::new(1_000_000_000) };
}

/// `on_schedule_message_pump_work` → the foreground pump task.
static PUMP: OnceLock<async_channel::Sender<i64>> = OnceLock::new();

/// Where page changes, focus and close requests go (the window's `Browsers`).
pub fn set_events(tx: async_channel::Sender<Event>) {
    EVENTS.with_borrow_mut(|e| *e = Some(tx));
}

/// The person pressed the mouse on Herdr while a page had the keyboard (view.rs).
pub fn clicked_herdr() {
    send(Event::ClickedHerdr);
}

/// A page hid under an overlay or came back from under one (view.rs).
pub fn covers_changed() {
    send(Event::Covers);
}

fn send(event: Event) {
    EVENTS.with_borrow(|e| {
        if let Some(tx) = e {
            let _ = tx.try_send(event);
        }
    });
}

/// Herdr's browser `id`: the current open's CEF browser, never one closing.
fn browser(id: &str) -> Option<Browser> {
    STATE.with_borrow(|s| s.browsers.iter().find(|o| s.is_current(&o.id, o.generation) && o.id == id).map(|o| o.browser.clone()))
}

fn need(id: &str) -> Result<Browser, String> {
    browser(id).ok_or_else(|| format!("browser {id} is still opening"))
}

/// The page's own view (CEF's), once the browser exists.
pub fn page_view(id: &str) -> Option<NonNull<NSView>> {
    NonNull::new(browser(id)?.host()?.window_handle().cast::<NSView>())
}

pub fn navigate(id: &str, url: &str) -> Result<(), String> {
    if !agent::allowed_url(url) {
        tracing::warn!("browser: {id} not sent to {}: only http, https and about:blank pages open", agent::loggable(url), id = super::log_safe(id));
        return Err(format!("only http, https and about:blank pages open, not {}", agent::loggable(url)));
    }
    let frame = need(id)?.main_frame().ok_or("the page has no main frame")?;
    frame.load_url(Some(&CefString::from(url)));
    Ok(())
}

/// -1 back, 1 forward.
pub fn history(id: &str, step: i32) -> Result<(), String> {
    let b = need(id)?;
    match step {
        -1 if b.can_go_back() == 1 => b.go_back(),
        1 if b.can_go_forward() == 1 => b.go_forward(),
        -1 => return Err(format!("browser {id} has nothing to go back to")),
        _ => return Err(format!("browser {id} has nothing to go forward to")),
    }
    Ok(())
}

pub fn reload(id: &str) -> Result<(), String> {
    need(id)?.reload();
    Ok(())
}

/// Opens or closes CEF's own DevTools window for a browser (None toggles).
/// Returns whether it's open now. No debugging port: DevTools talks to the
/// browser inside CEF.
pub fn devtools(id: &str, open: Option<bool>) -> Result<bool, String> {
    let host = need(id)?.host().ok_or("the browser has no host")?;
    let is_open = host.has_dev_tools() == 1;
    let want = open.unwrap_or(!is_open);
    if want && !is_open {
        let mut client = DevToolsClient::new(DevToolsLifeSpan::new());
        host.show_dev_tools(Some(&WindowInfo::default()), Some(&mut client), Some(&BrowserSettings::default()), None);
    } else if !want && is_open {
        host.close_dev_tools();
    }
    Ok(want)
}

/// A page's zoom in percent, as Chromium has it for its site.
fn zoom_of(b: &Browser) -> f64 {
    b.host().map_or(100.0, |h| emulate::percent_of(h.zoom_level()))
}

/// Browser `id`'s zoom in percent.
pub fn zoom(id: &str) -> Result<f64, String> {
    Ok(zoom_of(&need(id)?))
}

/// Sets browser `id`'s zoom (percent; 100 is Chromium's default, level 0).
/// Chromium keeps a zoom per site in the profile, as Chrome does, so every
/// page on that site in the same profile follows. Returns the zoom now.
pub fn set_zoom(id: &str, percent: f64) -> Result<f64, String> {
    let b = need(id)?;
    let host = b.host().ok_or("the browser has no host")?;
    host.set_zoom_level(if (percent - 100.0).abs() < 1e-9 { 0.0 } else { emulate::level_of(percent) });
    refresh_zoom();
    Ok(zoom_of(&b))
}

/// Every page's zoom, read again: a zoom change reaches every page on that site.
fn refresh_zoom() {
    let open: Vec<(String, u64, Browser)> = STATE.with_borrow(|s| {
        s.browsers.iter().filter(|o| s.is_current(&o.id, o.generation)).map(|o| (o.id.clone(), o.generation, o.browser.clone())).collect()
    });
    for (id, generation, b) in open {
        let zoom = zoom_of(&b);
        if STATE.with_borrow(|s| s.pages.get(&id).is_some_and(|p| (p.zoom - zoom).abs() > 1e-6)) {
            page_changed(&id, generation, |p| p.zoom = zoom);
        }
    }
}

/// Puts a responsive viewport on browser `id`'s page, or takes it off
/// (None): Chromium's device emulation over the page's own DevTools session,
/// the one an agent's CDP goes through too, so the last one to set it wins.
/// A viewport bigger than the pane is shrunk to fit it (`emulate::fit_scale`;
/// `refit` keeps it fitting as the pane changes size). A reopen (an agent's
/// session ending) puts it back on.
pub fn set_viewport(id: &str, viewport: Option<Viewport>) -> Result<(), String> {
    let generation = STATE
        .with_borrow(|s| s.browsers.iter().find(|o| o.id == id && s.is_current(&o.id, o.generation)).map(|o| o.generation))
        .ok_or_else(|| format!("browser {id} is still opening"))?;
    let scale = send_viewport(id, viewport.as_ref())?;
    tracing::info!("browser: {} viewport {}", super::log_safe(id), viewport.as_ref().map_or_else(|| "fit".to_owned(), |v| format!("{}x{}{} scale {scale:.2}", v.width, v.height, if v.mobile { " mobile" } else { "" })));
    page_changed(id, generation, |p| p.viewport = viewport);
    Ok(())
}

/// The block placed `id`'s container at `size` this frame (view.rs): the
/// page fills it. CEF sizes its view once, at creation, maybe before the
/// block first placed the container. `resized`: the container changed size,
/// so an emulated viewport is refitted (CEF's view follows its container on
/// its own, so the container's size is the one to watch).
pub fn fit(id: &str, size: objc2_foundation::NSSize, resized: bool) {
    if let Some(page) = page_view(id) {
        // SAFETY: CEF's live browser view, main thread.
        let page = unsafe { page.as_ref() };
        let fill = objc2_foundation::NSRect::new(objc2_foundation::NSPoint::new(0.0, 0.0), size);
        let had = page.frame();
        if had.origin != fill.origin || had.size != fill.size {
            tracing::debug!("browser: {id}'s page fills its block: {}x{} -> {}x{}", had.size.width, had.size.height, size.width, size.height, id = super::log_safe(id));
            page.setFrame(fill);
        }
    }
    if resized {
        refit(id);
    }
}

/// A mouse press landed in `id`'s page (view.rs). CEF reports the focus
/// that follows on its own (`on_got_focus`), so nothing to do here.
pub fn pressed(_id: &str) {}

/// Whether an event is an agent's key on its way out of the page.
/// Never on CEF: Chromium marks DevTools key events skip-if-unhandled, so an
/// unhandled one never comes back to the window.
pub fn agent_event(_event: &objc2_app_kit::NSEvent) -> bool {
    false
}

/// Browser `id`'s pane changed size: a viewport on it is shrunk to fit
/// again, if the fit changed.
fn refit(id: &str) {
    let Some(v) = STATE.with_borrow(|s| s.pages.get(id).and_then(|p| p.viewport.clone())) else { return };
    let pane = pane_size(id);
    if STATE.with_borrow(|s| s.fits.get(id).is_some_and(|f| (f - emulate::fit_scale(&v, pane)).abs() < 0.005)) {
        return;
    }
    match send_viewport(id, Some(&v)) {
        Ok(scale) => tracing::debug!("browser: {}'s viewport fits its pane again at scale {scale:.2}", super::log_safe(id)),
        Err(e) => tracing::warn!("browser: {}'s viewport not refitted: {e}", super::log_safe(id)),
    }
}

fn pane_size(id: &str) -> (f64, f64) {
    // SAFETY: CEF's browser view, read on the main thread.
    page_view(id).map(|v| unsafe { v.as_ref() }.frame().size).map_or((0.0, 0.0), |s| (s.width, s.height))
}

/// Sends the viewport's DevTools calls to the page; the scale it fit at.
fn send_viewport(id: &str, viewport: Option<&Viewport>) -> Result<f64, String> {
    let host = need(id)?.host().ok_or("the browser has no host")?;
    let scale = viewport.map_or(1.0, |v| emulate::fit_scale(v, pane_size(id)));
    for (method, params) in emulate::calls(viewport, scale) {
        send_own(&host, id, method, params)?;
    }
    STATE.with_borrow_mut(|s| s.fits.insert(id.to_owned(), scale));
    Ok(scale)
}

/// Tells CEF its page has the keyboard (the view is first responder already).
pub fn set_focus(id: &str) {
    if let Some(host) = browser(id).and_then(|b| b.host()) {
        host.set_focus(1);
    }
}

/// Runs the pump soon (any thread): the agent proxy queued work for a page.
pub fn wake() {
    if let Some(tx) = PUMP.get() {
        let _ = tx.try_send(0);
    }
}

/// Sends one DevTools message (JSON, already allowed by the proxy) to
/// browser `id`'s page, watching its answers first. False: no page now.
pub fn send_devtools(id: &str, message: &[u8]) -> bool {
    let Some((b, generation)) = STATE.with_borrow(|s| {
        s.browsers.iter().find(|o| o.id == id && s.is_current(&o.id, o.generation)).map(|o| (o.browser.clone(), o.generation))
    }) else {
        return false;
    };
    let Some(host) = b.host() else { return false };
    let cef = b.identifier();
    if !STATE.with_borrow(|s| s.observers.contains_key(&cef)) {
        let mut observer = HerdrDevTools::new(id.to_owned(), generation);
        match host.add_dev_tools_message_observer(Some(&mut observer)) {
            Some(r) => STATE.with_borrow_mut(|s| s.observers.insert(cef, r)),
            None => return false,
        };
    }
    host.send_dev_tools_message(Some(message)) == 1
}

/// An agent asked the window to change `browser`'s tabs (main thread, from
/// the proxy's drain): the window runs it and answers through `proxy::tab_done`.
pub fn tab_op(browser: &str, op: agent::TabOp) {
    send(Event::Agent { browser: browser.to_owned(), op });
}

/// An agent's connection to `id` ended: what it set on the page's DevTools
/// session (overrides, headers, enabled domains, auto-attach) is undone in
/// place, and Herdr's own viewport goes back on. The page itself stays as the
/// person has it (form input, history, a popup's opener). CEF keeps one
/// DevTools client for the browser, so dropping the observer would not
/// detach it: the undo is sent call by call.
pub fn agent_gone(id: &str) {
    if phase() != Phase::Ready {
        return;
    }
    let Ok(host) = need(id).and_then(|b| b.host().ok_or_else(|| "the browser has no host".to_owned())) else { return };
    tracing::info!("browser: {}'s agent session ended; undoing what it set", super::log_safe(id));
    for (method, params) in agent::reset_calls() {
        if let Err(e) = send_own(&host, id, &method, params) {
            tracing::warn!("browser: {e}");
        }
    }
    let viewport = STATE.with_borrow(|s| s.pages.get(id).and_then(|p| p.viewport.clone()));
    if let Err(e) = send_viewport(id, viewport.as_ref()) {
        tracing::warn!("browser: {}'s viewport not put back: {e}", super::log_safe(id));
    }
}

/// One DevTools call of Herdr's own to `host`'s page, under an id the agent
/// proxy never uses.
fn send_own(host: &BrowserHost, id: &str, method: &str, params: serde_json::Value) -> Result<(), String> {
    let own = OWN_ID.get();
    OWN_ID.set(own + 1);
    let message = serde_json::json!({ "id": own, "method": method, "params": params }).to_string();
    if host.send_dev_tools_message(Some(message.as_bytes())) != 1 {
        return Err(format!("Chromium didn't take {method} for browser {id}", id = super::log_safe(id)));
    }
    Ok(())
}

fn phase() -> Phase {
    STATE.with_borrow(|s| s.phase)
}

/// Loads the framework and starts CEF, once. Later calls are no-ops; a start
/// that failed stays failed.
pub fn start(cx: &mut gpui::App) -> anyhow::Result<()> {
    match phase() {
        Phase::Off => {}
        Phase::Failed => anyhow::bail!("CEF failed to start earlier"),
        Phase::Closing | Phase::Down => anyhow::bail!("CEF is shutting down"),
        Phase::Starting | Phase::Ready => return Ok(()),
    }
    // Before initialize: CEF can call `on_context_initialized` from inside it.
    STATE.with_borrow_mut(|s| s.phase = Phase::Starting);
    if let Err(e) = start_cef() {
        STATE.with_borrow_mut(|s| s.phase = Phase::Failed);
        return Err(e);
    }
    start_pump(cx);
    // Quit closes the browsers and shuts CEF down while the window still exists.
    cx.on_app_quit(|_| {
        shutdown();
        async {}
    })
    .detach();
    Ok(())
}

/// A folder only this user can open (created or tightened to 0700).
fn private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

fn start_cef() -> anyhow::Result<()> {
    // CEF reads the process's argv: a remote-debugging switch there would open a port.
    let argv: Vec<String> = std::env::args().collect();
    if let Some(switch) = agent::forbidden_switch(argv.iter().map(String::as_str)) {
        anyhow::bail!("refusing to start CEF with --{switch}: Herdr's browser never opens a DevTools port");
    }
    let exe = std::env::current_exe()?;
    let framework = exe.parent().unwrap_or(&exe).join("../Frameworks").join(FRAMEWORK);
    if !framework.is_file() {
        anyhow::bail!(
            "the CEF framework is missing at {}: start herdr-app from its dev bundle (just browser-dev)",
            framework.display()
        );
    }
    let path = CString::new(framework.as_os_str().as_bytes())?;
    if load_library(Some(unsafe { &*path.as_ptr() })) != 1 {
        anyhow::bail!("cannot load the CEF framework at {}", framework.display());
    }
    // Before any other CEF call. Null: the framework is another CEF build.
    if api_hash(sys::CEF_API_VERSION_LAST, 0).is_null() {
        anyhow::bail!("the CEF framework at {} is not the build herdr-app expects", framework.display());
    }
    declare_app_protocol()?;

    // The person's profile under `browser/`, CEF's root cache path; every
    // folder is 0700.
    let root = crate::mods::app_dir().join("browser");
    let profile = root.join("profile");
    for dir in [&crate::mods::app_dir(), &root, &profile] {
        private_dir(dir)?;
    }
    let root_str = root.to_string_lossy();
    let profile_str = profile.to_string_lossy();
    let log = root.join("cef.log");
    let settings = Settings {
        // Renderers run in Chromium's macOS sandbox (the helper starts it).
        no_sandbox: 0,
        external_message_pump: 1,
        root_cache_path: CefString::from(root_str.as_ref()),
        cache_path: CefString::from(profile_str.as_ref()),
        log_file: CefString::from(log.to_string_lossy().as_ref()),
        log_severity: LogSeverity::WARNING,
        // Never a DevTools port: agents get CDP through the app.
        remote_debugging_port: 0,
        ..Default::default()
    };
    let args = args::Args::new();
    let mut app = HerdrCefApp::new(HerdrProcessHandler::new());
    if initialize(Some(args.as_main_args()), Some(&settings), Some(&mut app), std::ptr::null_mut()) != 1 {
        anyhow::bail!("CEF did not start (exit code {})", get_exit_code());
    }
    STATE.with_borrow_mut(|s| s.app = Some(app));
    tracing::info!("browser: CEF started, profile {}", profile.display());
    Ok(())
}

/// Chromium checks `[NSApp conformsToProtocol:@protocol(CrAppProtocol)]`. GPUI's
/// `GPUIApplication` has the methods (vendor/gpui-pre-macos, HERDR:); the
/// protocols exist only once CEF's framework is loaded, so conformance is
/// declared here.
fn declare_app_protocol() -> anyhow::Result<()> {
    let mtm = MainThreadMarker::new().ok_or_else(|| anyhow::anyhow!("CEF must start on the main thread"))?;
    let class = AnyClass::get(c"GPUIApplication").ok_or_else(|| anyhow::anyhow!("no GPUIApplication class"))?;
    let mut declared = Vec::new();
    for name in [c"CrAppProtocol", c"CrAppControlProtocol", c"CefAppProtocol"] {
        if let Some(protocol) = AnyProtocol::get(name) {
            // SAFETY: both are live runtime objects; adding a protocol is idempotent.
            unsafe { objc2::ffi::class_addProtocol((class as *const AnyClass).cast_mut(), protocol) };
            declared.push(name.to_string_lossy());
        }
    }
    let app = objc2_app_kit::NSApp(mtm);
    let app: &AnyObject = &app;
    let cr = AnyProtocol::get(c"CrAppProtocol").ok_or_else(|| anyhow::anyhow!("CEF's framework has no CrAppProtocol"))?;
    // SAFETY: plain NSObject queries on NSApp.
    let conforms: Bool = unsafe { msg_send![app, conformsToProtocol: cr] };
    let handling: Bool = unsafe { msg_send![app, isHandlingSendEvent] };
    if !conforms.as_bool() {
        anyhow::bail!("NSApp does not conform to CrAppProtocol after adding it");
    }
    tracing::info!(
        "browser: NSApp conforms to CrAppProtocol ({}), isHandlingSendEvent {}",
        declared.join(", "),
        handling.as_bool()
    );
    Ok(())
}

/// The external pump: CEF says when it wants work (`delay` ms, any thread);
/// a foreground task runs it, plus a tick at least every 33 ms as CEF asks.
fn start_pump(cx: &mut gpui::App) {
    let (tx, rx) = async_channel::unbounded::<i64>();
    if PUMP.set(tx).is_err() {
        return;
    }
    let fg = cx.foreground_executor().clone();
    let bg = cx.background_executor().clone();
    let (fg2, bg2) = (fg.clone(), bg.clone());
    fg.spawn(async move {
        while let Ok(mut delay) = rx.recv().await {
            // Coalesce a burst into its soonest request.
            while let Ok(next) = rx.try_recv() {
                delay = delay.min(next);
            }
            if delay <= 0 {
                pump();
            } else {
                let wait = Duration::from_millis(delay as u64).min(MAX_PUMP_DELAY);
                let bg = bg2.clone();
                fg2.spawn(async move {
                    bg.timer(wait).await;
                    pump();
                })
                .detach();
            }
            if matches!(phase(), Phase::Down | Phase::Failed) {
                break;
            }
        }
    })
    .detach();
    let fg3 = cx.foreground_executor().clone();
    fg3.spawn(async move {
        loop {
            bg.timer(MAX_PUMP_DELAY).await;
            if matches!(phase(), Phase::Down | Phase::Failed) {
                break;
            }
            pump();
        }
    })
    .detach();
}

/// One `CefDoMessageLoopWork`, never nested (a page's context menu runs a
/// native loop inside it, and GPUI tasks still run there).
fn pump() {
    if !matches!(phase(), Phase::Starting | Phase::Ready | Phase::Closing) || PUMPING.get() {
        return;
    }
    PUMPING.set(true);
    do_message_loop_work();
    PUMPING.set(false);
    proxy::drain();
}

/// Closes a browser the way CEF wants for a child view: its DevTools first,
/// then remove its view (the superview holds the only reference, so it is
/// freed and CEF sees its window go), then a forced `CloseBrowser` as a
/// backstop. `on_before_close` follows asynchronously.
fn close_browser(browser: &Browser) {
    let Some(host) = browser.host() else { return };
    if host.has_dev_tools() == 1 {
        host.close_dev_tools();
    }
    let view = host.window_handle().cast::<NSView>();
    // SAFETY: CEF's browser view, on the main thread; CEF keeps no reference
    // of its own, so this only borrows it until it's removed.
    if let Some(view) = unsafe { view.as_ref() } {
        view.removeFromSuperview();
    }
    host.close_browser(1);
}

/// Closes Herdr's browser `id` (its block is gone). One still being created
/// closes as soon as it exists; one never started is dropped.
pub fn close(id: &str) {
    let b = browser(id);
    STATE.with_borrow_mut(|s| {
        s.pending.retain(|(p, _, _, _)| p != id);
        // Not current any more: one still being created closes once
        // `on_after_created` sees that; later callbacks of this open change nothing.
        s.current.remove(id);
        s.pages.remove(id);
        s.fits.remove(id);
    });
    proxy::tab_gone(id);
    if let Some(b) = b {
        tracing::info!("browser: closing {id} (cef {})", b.identifier(), id = super::log_safe(id));
        STATE.with_borrow_mut(|s| s.closing.push(b.identifier()));
        objc2::rc::autoreleasepool(|_| close_browser(&b));
    }
}

/// Opens browser `id` filling `parent`, now or once CEF's context is ready.
pub fn open(id: &str, parent: Retained<NSView>, url: String) {
    let generation = STATE.with_borrow_mut(|s| {
        s.next_generation += 1;
        s.current.insert(id.to_owned(), s.next_generation);
        s.pages.insert(id.to_owned(), Page { url: url.clone(), loading: true, ..Page::default() });
        s.next_generation
    });
    match phase() {
        Phase::Starting => STATE.with_borrow_mut(|s| s.pending.push((id.to_owned(), generation, parent, url))),
        Phase::Ready => create(id, generation, &parent, &url),
        other => tracing::warn!("browser: {id} not opened, CEF is {other:?}", id = super::log_safe(id)),
    }
}

fn create(id: &str, generation: u64, parent: &NSView, url: &str) {
    // The one URL gate: a saved or restored block can name anything.
    let url = if agent::allowed_url(url) {
        url
    } else {
        tracing::warn!("browser: {id} opens on about:blank, not {}: only http, https and about:blank pages open", agent::loggable(url), id = super::log_safe(id));
        registry_blank()
    };
    let size = parent.bounds().size;
    let bounds = Rect { x: 0, y: 0, width: size.width as c_int, height: size.height as c_int };
    let handle: sys::cef_window_handle_t = (parent as *const NSView).cast_mut().cast();
    let window_info = WindowInfo::default().set_as_child(handle, &bounds);
    let id = id.to_owned();
    let mut client = new_client(&id, generation);
    STATE.with_borrow_mut(|s| s.creating += 1);
    let asked = browser_host_create_browser(
        Some(&window_info),
        Some(&mut client),
        Some(&CefString::from(url)),
        Some(&BrowserSettings::default()),
        None,
        None,
    );
    if asked != 1 {
        STATE.with_borrow_mut(|s| s.creating -= 1);
        tracing::error!("browser: CEF refused to create {id}", id = super::log_safe(&id));
    } else {
        tracing::info!("browser: creating {id} at {}", agent::loggable(url), id = super::log_safe(&id));
    }
}

fn new_client(id: &str, generation: u64) -> Client {
    HerdrClient::new(
        HerdrLifeSpan::new(id.to_owned(), generation),
        HerdrLoad::new(id.to_owned(), generation),
        HerdrDisplay::new(id.to_owned(), generation),
        HerdrFocus::new(id.to_owned(), generation),
        HerdrDownloads::new(),
    )
}

/// Records a page change and tells the window (only for the id's current open).
fn page_changed(id: &str, generation: u64, change: impl FnOnce(&mut Page)) {
    let page = STATE.with_borrow_mut(|s| {
        if !s.is_current(id, generation) {
            return None;
        }
        let page = s.pages.entry(id.to_owned()).or_default();
        change(page);
        Some(page.clone())
    });
    if let Some(page) = page {
        send(Event::Page { tab: id.to_owned(), page });
    }
}

fn is_current(id: &str, generation: u64) -> bool {
    STATE.with_borrow(|s| s.is_current(id, generation))
}

fn context_ready() {
    let pending = STATE.with_borrow_mut(|s| {
        if s.phase == Phase::Starting {
            s.phase = Phase::Ready;
        }
        std::mem::take(&mut s.pending)
    });
    tracing::info!("browser: CEF context ready");
    for (id, generation, parent, url) in pending {
        create(&id, generation, &parent, &url);
    }
}

/// Quit: close every browser (asynchronous in CEF), pump until each one's
/// `on_before_close`, then `CefShutdown`. Runs inside GPUI's quit, while the
/// window still exists. Shutting CEF down with a browser still open crashes,
/// so after the deadline it is skipped and the helpers end with the app.
pub fn shutdown() {
    if !matches!(phase(), Phase::Starting | Phase::Ready) {
        return;
    }
    // Quit ends every agent token and connection first.
    proxy::revoke_all();
    if PUMPING.get() {
        tracing::warn!("browser: quit inside a CEF callback; CEF not shut down");
        return;
    }
    let deadline = Instant::now() + CLOSE_DEADLINE;
    let pump_until = |done: &dyn Fn(&State) -> bool| {
        while !STATE.with_borrow(|s| done(s)) && Instant::now() < deadline {
            // Its own pool, as no run loop drains one here: the closed
            // browser's view must really be freed for CEF to finish closing.
            objc2::rc::autoreleasepool(|_| do_message_loop_work());
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    // A browser still being created can't be closed yet.
    pump_until(&|s| s.phase != Phase::Starting && s.creating == 0);
    // Those already closing finish on their own.
    let browsers = STATE.with_borrow_mut(|s| {
        s.phase = Phase::Closing;
        s.pending.clear();
        let open: Vec<Browser> = s.browsers.iter().filter(|o| !s.closing.contains(&o.browser.identifier())).map(|o| o.browser.clone()).collect();
        s.closing.extend(open.iter().map(|b| b.identifier()));
        open
    });
    tracing::info!("browser: closing {} browser(s)", browsers.len());
    // Forced: no beforeunload prompt can hold up quitting.
    objc2::rc::autoreleasepool(|_| browsers.iter().for_each(close_browser));
    pump_until(&|s| s.browsers.is_empty() && s.devtools == 0);
    let left = STATE.with_borrow(|s| s.browsers.len() + s.devtools);
    if left > 0 {
        tracing::warn!("browser: {left} browser(s) still open after {CLOSE_DEADLINE:?}; CEF not shut down");
    } else {
        cef::shutdown();
        tracing::info!("browser: CEF shut down");
    }
    STATE.with_borrow_mut(|s| {
        s.phase = Phase::Down;
        s.app = None;
    });
}

/// Comma-list switches Herdr adds one item to, keeping what is already there.
const LIST_SWITCHES: [(&str, &str); 2] = [
    // Chromium clones the whole app bundle on every launch for its updater;
    // Herdr has its own updater.
    ("disable-features", "MacAppCodeSignClone"),
    // No WebAuthn (passkeys, security keys). CEF in a native parent view has
    // no dialog host, so a passkey request would run with no UI until the
    // site's timeout, and macOS passkeys need an Apple entitlement Herdr
    // doesn't have. Without `PublicKeyCredential`, sites offer passwords
    // instead.
    ("disable-blink-features", "WebAuth"),
];

/// `existing` with `item` appended as a comma-list entry.
fn with_item(existing: &str, item: &str) -> String {
    if existing.is_empty() { item.to_string() } else { format!("{existing},{item}") }
}

fn registry_blank() -> &'static str {
    super::registry::BLANK
}

wrap_app! {
    struct HerdrCefApp {
        handler: BrowserProcessHandler,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(command_line) = command_line else { return };
            // Never a DevTools port, whatever reached the command line.
            for switch in agent::FORBIDDEN_SWITCHES {
                if command_line.has_switch(Some(&CefString::from(switch))) == 1 {
                    tracing::error!("browser: --{switch} is on CEF's command line; Herdr's browser never opens a DevTools port. Stopping.");
                    std::process::abort();
                }
            }
            // A mock key for the profile's cookie encryption: the spike never
            // touches the person's login keychain. Don't sign into real
            // accounts in this profile.
            command_line.append_switch(Some(&CefString::from("use-mock-keychain")));
            for (switch, item) in LIST_SWITCHES {
                let name = CefString::from(switch);
                let existing = CefString::from(&command_line.switch_value(Some(&name))).to_string();
                command_line.append_switch_with_value(Some(&name), Some(&CefString::from(with_item(&existing, item).as_str())));
            }
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(self.handler.clone())
        }
    }
}

wrap_browser_process_handler! {
    struct HerdrProcessHandler;

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            context_ready();
        }

        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            if let Some(tx) = PUMP.get() {
                let _ = tx.try_send(delay_ms);
            }
        }
    }
}

wrap_client! {
    struct HerdrClient {
        life_span: LifeSpanHandler,
        load: LoadHandler,
        display: DisplayHandler,
        focus: FocusHandler,
        downloads: DownloadHandler,
    }

    impl Client {
        fn download_handler(&self) -> Option<DownloadHandler> {
            Some(self.downloads.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }

        fn focus_handler(&self) -> Option<FocusHandler> {
            Some(self.focus.clone())
        }
    }
}

wrap_life_span_handler! {
    struct HerdrLifeSpan {
        id: String,
        generation: u64,
    }

    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut Browser>) {
            let Some(browser) = browser else { return };
            let view = browser.host().map(|h| h.window_handle().cast::<NSView>());
            // SAFETY: CEF's browser view, read on the main thread.
            let placed = view.and_then(|v| unsafe { v.as_ref() }).map(|v| {
                let f = v.frame();
                // CEF sized the page to its container when it was asked for;
                // the block may have placed the container since (the first
                // browser waits for CEF to start), and place() only resizes
                // pages that exist. Fill the container now.
                // SAFETY: a main-thread read of the view tree.
                let fitted = unsafe { v.superview() }.map(|parent| {
                    let size = parent.bounds().size;
                    if f.size != size {
                        v.setFrame(objc2_foundation::NSRect::new(objc2_foundation::NSPoint::new(0.0, 0.0), size));
                    }
                    format!(", fits its block at {}x{}", size.width, size.height)
                });
                format!("{}x{}, in a window: {}{}", f.size.width, f.size.height, v.window().is_some(), fitted.unwrap_or_default())
            });
            tracing::info!("browser: created {} (cef {}), view {}", super::log_safe(&self.id), browser.identifier(), placed.unwrap_or_else(|| "missing".into()));
            let browser = browser.clone();
            // Its block went while CEF was creating it: close it right away.
            let wanted = STATE.with_borrow_mut(|s| {
                s.creating = s.creating.saturating_sub(1);
                s.popups.retain(|(_, _, tab)| *tab != self.id);
                s.browsers.push(Open { id: self.id.clone(), generation: self.generation, browser: browser.clone() });
                s.is_current(&self.id, self.generation)
            });
            if !wanted {
                tracing::info!("browser: {} (cef {}) is no longer wanted; closing it", super::log_safe(&self.id), browser.identifier());
                STATE.with_borrow_mut(|s| s.closing.push(browser.identifier()));
                close_browser(&browser);
            } else {
                view::page_created(&self.id);
                proxy::tab_ready(&self.id);
            }
        }

        /// A popup (`window.open`, a target link) becomes a tab of this pane: a
        /// real popup browser hosted in a container of its own, so the page
        /// that opened it keeps `window.opener` (OAuth sign-ins need it). The
        /// tab is reserved here, in the callback, and wired up in
        /// `on_after_created`.
        fn on_before_popup(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            popup_id: c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            user_gesture: c_int,
            _popup_features: Option<&PopupFeatures>,
            window_info: Option<&mut WindowInfo>,
            client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            no_javascript_access: Option<&mut c_int>,
        ) -> c_int {
            if !is_current(&self.id, self.generation) {
                return 1;
            }
            let (Some(window_info), Some(client), Some(no_javascript_access)) = (window_info, client, no_javascript_access) else { return 1 };
            let url = target_url.map(|u| u.to_string()).unwrap_or_default();
            // The URL gate, and a cross-origin frame needs the person's gesture.
            // `window.open()` with no url starts blank and navigates later.
            if !url.is_empty() {
                let frame_url = frame.as_ref().map(|f| CefString::from(&f.url()).to_string()).unwrap_or_default();
                let is_main = frame.as_ref().is_some_and(|f| f.is_main() == 1);
                let main_url = browser.and_then(|b| b.main_frame()).map(|f| CefString::from(&f.url()).to_string()).unwrap_or_default();
                if !agent::popup_allowed(&url, &frame_url, &main_url, is_main, user_gesture == 1) {
                    tracing::warn!(
                        "browser: {} popup to {} dropped (gesture {}, main frame {is_main}): only http, https and about:blank, and a cross-origin frame needs a click",
                        super::log_safe(&self.id), agent::loggable(&url), user_gesture == 1
                    );
                    return 1;
                }
            }
            let tab = registry::tab_id(registry::browser_of(&self.id));
            let parent = match view::popup_container(&tab) {
                Ok(parent) => parent,
                Err(e) => {
                    tracing::warn!("browser: {} popup dropped, no view for it: {e:#}", super::log_safe(&self.id));
                    return 1;
                }
            };
            let size = parent.bounds().size;
            let bounds = Rect { x: 0, y: 0, width: size.width as c_int, height: size.height as c_int };
            let handle: sys::cef_window_handle_t = Retained::as_ptr(&parent).cast_mut().cast();
            *window_info = std::mem::take(window_info).set_as_child(handle, &bounds);
            let generation = STATE.with_borrow_mut(|s| {
                s.next_generation += 1;
                s.current.insert(tab.clone(), s.next_generation);
                s.pages.insert(tab.clone(), Page { url: url.clone(), loading: true, ..Page::default() });
                s.popups.push((self.id.clone(), popup_id, tab.clone()));
                s.creating += 1;
                s.next_generation
            });
            *client = Some(new_client(&tab, generation));
            // The opener keeps its handle on the popup (`window.opener`, postMessage).
            *no_javascript_access = 0;
            tracing::info!("browser: {} popup {} opens as tab {}", super::log_safe(&self.id), agent::loggable(&url), super::log_safe(&tab));
            send(Event::Popup { opener: self.id.clone(), tab, url });
            0
        }

        /// CEF dropped a popup it had allowed: its tab goes.
        fn on_before_popup_aborted(&self, _browser: Option<&mut Browser>, popup_id: c_int) {
            let tab = STATE.with_borrow_mut(|s| {
                let at = s.popups.iter().position(|(opener, id, _)| *opener == self.id && *id == popup_id)?;
                let (_, _, tab) = s.popups.remove(at);
                s.creating = s.creating.saturating_sub(1);
                s.current.remove(&tab);
                s.pages.remove(&tab);
                Some(tab)
            });
            if let Some(tab) = tab {
                tracing::info!("browser: popup of {} aborted; tab {} dropped", super::log_safe(&self.id), super::log_safe(&tab));
                view::remove(&tab);
                send(Event::PopupAborted { opener: self.id.clone(), tab });
            }
        }

        fn do_close(&self, browser: Option<&mut Browser>) -> c_int {
            // 0 would make CEF send performClose: to the browser's top-level
            // window: Herdr's own. 1: Herdr closes it. Closes Herdr starts
            // land here too (ignored); a page that closes itself
            // (window.close()) asks the window to close its tab.
            let Some(browser) = browser else { return 1 };
            let cef = browser.identifier();
            let ours = STATE.with_borrow(|s| s.phase == Phase::Closing || s.closing.contains(&cef));
            if !ours && is_current(&self.id, self.generation) {
                tracing::info!("browser: {} asked to close", super::log_safe(&self.id));
                send(Event::CloseAsked(self.id.clone()));
            }
            1
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            let cef = browser.map(|b| b.identifier());
            STATE.with_borrow_mut(|s| {
                // By CEF's identifier: a newer browser with the same Herdr id stays.
                s.browsers.retain(|o| Some(o.browser.identifier()) != cef);
                s.closing.retain(|b| Some(*b) != cef);
                if let Some(cef) = cef {
                    s.observers.remove(&cef);
                }
                if s.is_current(&self.id, self.generation) {
                    s.current.remove(&self.id);
                    s.pages.remove(&self.id);
                }
            });
            tracing::info!("browser: closed {} (cef {})", super::log_safe(&self.id), cef.unwrap_or_default());
        }
    }
}

wrap_load_handler! {
    struct HerdrLoad {
        id: String,
        generation: u64,
    }

    impl LoadHandler {
        fn on_loading_state_change(&self, browser: Option<&mut Browser>, is_loading: c_int, can_go_back: c_int, can_go_forward: c_int) {
            // The new page's site may have a zoom of its own.
            let zoom = browser.map(|b| zoom_of(b));
            page_changed(&self.id, self.generation, |p| {
                if let Some(zoom) = zoom {
                    p.zoom = zoom;
                }
                p.loading = is_loading == 1;
                p.can_go_back = can_go_back == 1;
                p.can_go_forward = can_go_forward == 1;
            });
        }

        fn on_load_end(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, http_status_code: c_int) {
            let Some(frame) = frame else { return };
            if frame.is_main() == 1 {
                let url = CefString::from(&frame.url()).to_string();
                tracing::info!("browser: {} load end {} status {http_status_code}", super::log_safe(&self.id), agent::loggable(&url));
            }
        }

        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            if frame.is_some_and(|f| f.is_main() == 1) {
                let text = error_text.map(|t| t.to_string()).unwrap_or_default();
                let url = failed_url.map(|u| u.to_string()).unwrap_or_default();
                tracing::warn!("browser: {} load error {} {error_code:?} {text}", super::log_safe(&self.id), agent::loggable(&url));
            }
        }
    }
}

wrap_display_handler! {
    struct HerdrDisplay {
        id: String,
        generation: u64,
    }

    impl DisplayHandler {
        fn on_address_change(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, url: Option<&CefString>) {
            if frame.is_some_and(|f| f.is_main() == 1) {
                let url = url.map(|u| u.to_string()).unwrap_or_default();
                if is_current(&self.id, self.generation) {
                    proxy::set_url(&self.id, &url);
                }
                page_changed(&self.id, self.generation, |p| p.url = url);
            }
        }

        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            let title = title.map(|t| t.to_string()).unwrap_or_default();
            page_changed(&self.id, self.generation, |p| p.title = title);
        }
    }
}

wrap_focus_handler! {
    struct HerdrFocus {
        id: String,
        generation: u64,
    }

    impl FocusHandler {
        /// CEF asks before a page takes focus (it does for every browser it
        /// creates, and on loads): refused unless the window gave this page
        /// the keyboard or the person just pressed the mouse in it.
        fn on_set_focus(&self, _browser: Option<&mut Browser>, source: FocusSource) -> c_int {
            let source = if source == FocusSource::NAVIGATION { "navigation" } else { "system" };
            let current = is_current(&self.id, self.generation);
            let allowed = current && view::may_take_focus(&self.id);
            if !allowed {
                tracing::debug!("browser: {} asked for the keyboard ({source}); refused, it isn't the page that should have it", super::log_safe(&self.id));
            }
            c_int::from(!allowed)
        }

        fn on_got_focus(&self, _browser: Option<&mut Browser>) {
            if is_current(&self.id, self.generation) {
                send(Event::Focused(self.id.clone()));
            }
        }
    }
}

wrap_client! {
    struct DevToolsClient {
        life_span: LifeSpanHandler,
    }

    impl Client {
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }
    }
}

// CEF's DevTools window: counted, so quit waits for it to close too.
wrap_life_span_handler! {
    struct DevToolsLifeSpan;

    impl LifeSpanHandler {
        fn on_after_created(&self, _browser: Option<&mut Browser>) {
            let open = STATE.with_borrow_mut(|s| {
                s.devtools += 1;
                s.devtools
            });
            tracing::info!("browser: a DevTools window opened ({open} open)");
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            let open = STATE.with_borrow_mut(|s| {
                s.devtools = s.devtools.saturating_sub(1);
                s.devtools
            });
            tracing::info!("browser: a DevTools window closed ({open} open)");
        }
    }
}

// Downloads: allowed, as in every browser; no folder is chosen here.
wrap_download_handler! {
    struct HerdrDownloads {}

    impl DownloadHandler {
        fn can_download(&self, _browser: Option<&mut Browser>, _url: Option<&CefString>, _method: Option<&CefString>) -> c_int {
            1
        }
    }
}

// The agent's side of a page's DevTools session: every answer and event
// goes to the proxy, which passes on only this agent's own.
wrap_dev_tools_message_observer! {
    struct HerdrDevTools {
        id: String,
        generation: u64,
    }

    impl DevToolsMessageObserver {
        fn on_dev_tools_message(&self, _browser: Option<&mut Browser>, message: Option<&[u8]>) -> c_int {
            if let Some(message) = message
                && is_current(&self.id, self.generation)
            {
                proxy::from_page(&self.id, message);
            }
            1
        }

        /// Chromium dropped this tab's agent and with it everything set on its
        /// session: the agent's sessions on this tab end, its token and its
        /// other tabs stay.
        fn on_dev_tools_agent_detached(&self, _browser: Option<&mut Browser>) {
            if is_current(&self.id, self.generation) && proxy::tab_detached(&self.id) {
                tracing::info!("browser: agent sessions on {} ended: CEF's DevTools agent went away", super::log_safe(&self.id));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cef_starts_with_webauthn_off_and_keeps_other_switch_values() {
        // Passkeys stay off until Herdr can show the system sheet itself.
        assert!(LIST_SWITCHES.contains(&("disable-blink-features", "WebAuth")));
        assert!(LIST_SWITCHES.contains(&("disable-features", "MacAppCodeSignClone")));
        assert_eq!(with_item("", "WebAuth"), "WebAuth");
        assert_eq!(with_item("Foo,Bar", "WebAuth"), "Foo,Bar,WebAuth");
    }
}
