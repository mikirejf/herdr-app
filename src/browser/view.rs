//! The native side of a browser block: one container `NSView` per browser
//! over GPUI's view, moved to its block's bounds each frame and hidden when
//! its block isn't painted; who has the keyboard; and the event monitor that
//! keeps Herdr's global keys working while a page has it and tells a click
//! into a page from CEF taking focus on its own.
//!
//! GPUI has no hosted-native-view element and no layout-change event, so a
//! block's canvas reports its bounds while painting (`place`) and a canvas
//! painted last hides every view not placed this frame (`end_frame`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use block2::RcBlock;
use gpui::{Bounds, Pixels, Window};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly as _};
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSView, NSWindowOrderingMode};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::keys::{self, Native, Press};
use super::{Rect, Responder, engine};

struct Container {
    view: Retained<NSView>,
    /// Its block's bounds this frame; None: not painted this frame.
    placed: Option<Rect>,
    /// Why it's hidden, for the log (None: it shows).
    hidden: Option<Hidden>,
}

#[derive(Clone, Debug, PartialEq)]
enum Hidden {
    /// Its block isn't painted (another tab, a zoomed pane, a hidden column), or not yet.
    NotShown,
    /// An overlay (its label) overlaps its block.
    Under(String),
}

thread_local! {
    static VIEWS: RefCell<HashMap<String, Container>> = RefCell::new(HashMap::new());
    /// The overlays painted this frame (label, bounds), in paint order.
    static COVERS: RefCell<Vec<(String, Rect)>> = const { RefCell::new(Vec::new()) };
    static BINDINGS: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    static MONITOR: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
    /// The page the last mouse press landed in, and when.
    static PRESSED: RefCell<Option<(String, Instant)>> = const { RefCell::new(None) };
    /// The page the window last gave the keyboard (inner None: Herdr);
    /// None until it first decided. CEF's focus requests are checked against it.
    static WANTED: RefCell<Option<Option<String>>> = const { RefCell::new(None) };
}

/// How long after a press in a page CEF's "got focus" still counts as that click.
const CLICK_FOCUS: Duration = Duration::from_secs(1);

/// GPUI's own view for a window (its window handle).
fn gpui_view(window: &Window) -> anyhow::Result<&NSView> {
    let handle = HasWindowHandle::window_handle(window).map_err(|e| anyhow::anyhow!("{e}"))?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        anyhow::bail!("not an AppKit window");
    };
    // SAFETY: GPUI's window handle is its live GPUIView; used on the main thread
    // while the window lives (the borrow can't outlive `window`).
    Ok(unsafe { handle.ns_view.cast::<NSView>().as_ref() })
}

/// The container a browser fills, made once per browser, hidden until its
/// block paints. It sits in the window's content view, above GPUI's view.
pub fn container(id: &str, window: &Window) -> anyhow::Result<Retained<NSView>> {
    if let Some(v) = VIEWS.with_borrow(|v| v.get(id).map(|c| c.view.clone())) {
        return Ok(v);
    }
    let mtm = MainThreadMarker::new().ok_or_else(|| anyhow::anyhow!("not on the main thread"))?;
    let gpui = gpui_view(window)?;
    // SAFETY: a main-thread read of the view tree.
    let content = unsafe { gpui.superview() }.ok_or_else(|| anyhow::anyhow!("GPUI's view has no superview"))?;
    // A size to start at: CEF sizes its view to the parent's when it creates it.
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0));
    let view = NSView::initWithFrame(NSView::alloc(mtm), frame);
    view.setWantsLayer(true);
    view.setHidden(true);
    content.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Above, Some(gpui));
    VIEWS.with_borrow_mut(|v| v.insert(id.to_owned(), Container { view: view.clone(), placed: None, hidden: Some(Hidden::NotShown) }));
    Ok(view)
}

fn rect(bounds: Bounds<Pixels>) -> Rect {
    Rect {
        x: f32::from(bounds.origin.x),
        y: f32::from(bounds.origin.y),
        w: f32::from(bounds.size.width),
        h: f32::from(bounds.size.height),
    }
}

/// An overlay painted this frame over these bounds (window points, top
/// left): a page it overlaps hides at the frame's end (`end_frame`).
pub fn cover(label: &str, bounds: Bounds<Pixels>) {
    COVERS.with_borrow_mut(|c| c.push((label.to_owned(), rect(bounds))));
}

/// The pages hidden under an overlay now: they can't have the keyboard.
pub fn covered() -> Vec<String> {
    VIEWS.with_borrow(|views| {
        let mut ids: Vec<String> = views.iter().filter(|(_, c)| matches!(c.hidden, Some(Hidden::Under(_)))).map(|(id, _)| id.clone()).collect();
        ids.sort();
        ids
    })
}

/// Puts a browser's view on its block (window points, top-left origin, as
/// GPUI paints); `end_frame` shows it unless an overlay covers it.
pub fn place(id: &str, bounds: Bounds<Pixels>, window: &Window) {
    let Ok(gpui) = gpui_view(window) else { return };
    let host = gpui.frame();
    let (x, y) = (f64::from(f32::from(bounds.origin.x)), f64::from(f32::from(bounds.origin.y)));
    let (w, h) = (f64::from(f32::from(bounds.size.width)), f64::from(f32::from(bounds.size.height)));
    // The content view's origin is its bottom left; GPUI's is the top left.
    let frame = NSRect::new(NSPoint::new(host.origin.x + x, host.origin.y + host.size.height - y - h), NSSize::new(w, h));
    VIEWS.with_borrow_mut(|views| {
        let Some(c) = views.get_mut(id) else { return };
        c.placed = Some(rect(bounds));
        let old = c.view.frame();
        let resized = old.size != frame.size;
        if old.origin != frame.origin || resized {
            tracing::debug!("browser: {id} placed at {x},{y} {w}x{h} (window points, top left)", id = super::log_safe(id));
            c.view.setFrame(frame);
        }
        // The engine puts the page in its container (checked every frame, so
        // a page created late still fits) and refits an emulated viewport.
        engine::fit(id, frame.size, resized);
    });
}

/// After a frame: a view whose block wasn't painted (another tab, a zoomed
/// pane, a hidden column) hides; so does one an overlay painted this frame
/// overlaps (`super::covered`), and its block's placeholder shows. Every
/// other placed view shows. A page that had the keyboard and hides hands it
/// to Herdr now (typing never goes into a hidden page); when the set of
/// covered pages changes, the window decides again who has the keyboard.
pub fn end_frame(window: &Window) {
    let covers = COVERS.with_borrow_mut(std::mem::take);
    let holder = match responder(window) {
        Responder::Page(id) => Some(id),
        _ => None,
    };
    let mut changed = false;
    let mut take_back = false;
    VIEWS.with_borrow_mut(|views| {
        let placed: Vec<(&str, Rect)> = views.iter().filter_map(|(id, c)| c.placed.map(|r| (id.as_str(), r))).collect();
        let decided: HashMap<String, Hidden> = super::covered(&placed, &covers)
            .into_iter()
            .filter_map(|(id, under)| under.map(|l| (id.to_owned(), Hidden::Under(l.to_owned()))))
            .collect();
        for (id, c) in views.iter_mut() {
            let want = match c.placed.take() {
                None => Some(Hidden::NotShown),
                Some(_) => decided.get(id).cloned(),
            };
            if want == c.hidden {
                continue;
            }
            let was_under = matches!(c.hidden, Some(Hidden::Under(_)));
            match &want {
                Some(Hidden::NotShown) => tracing::debug!("browser: {id} hidden (its block isn't shown)", id = super::log_safe(id)),
                Some(Hidden::Under(label)) => tracing::debug!("browser: {id} hidden under overlay {label}", id = super::log_safe(id)),
                None => match &c.hidden {
                    Some(Hidden::Under(label)) => tracing::debug!("browser: {id} shown again ({label} no longer over it)", id = super::log_safe(id)),
                    _ => tracing::debug!("browser: {id} shown", id = super::log_safe(id)),
                },
            }
            if want.is_some() && holder.as_deref() == Some(id.as_str()) {
                take_back = true;
            }
            changed |= was_under || matches!(want, Some(Hidden::Under(_)));
            c.view.setHidden(want.is_some());
            c.hidden = want;
        }
    });
    if take_back {
        // Before AppKit picks some other key view for a hidden first responder.
        tracing::debug!("browser: {} hid with the keyboard; Herdr takes it", super::log_safe(holder.as_deref().unwrap_or_default()));
        take_keys(window);
    }
    if changed {
        engine::covers_changed();
    }
}

/// CEF made a browser's page in its container. A page born in a hidden
/// container (under an overlay, or in a block that isn't shown) reports
/// `visible` until it has been shown once (measured; after that, hiding the
/// container makes it report `hidden`). Showing and hiding
/// the container again in the same main-thread turn tells it: AppKit sends
/// the unhide and hide to the page's view at once, and nothing draws between.
#[cfg_attr(not(feature = "browser"), allow(dead_code))] // CEF only
pub fn page_created(id: &str) {
    VIEWS.with_borrow(|views| {
        let Some(c) = views.get(id) else { return };
        let Some(why) = &c.hidden else { return };
        c.view.setHidden(false);
        c.view.setHidden(true);
        let why = match why {
            Hidden::NotShown => "its block isn't shown".to_owned(),
            Hidden::Under(label) => format!("under overlay {label}"),
        };
        tracing::debug!("browser: {id}'s page was born hidden ({why}); told it it's hidden", id = super::log_safe(id));
    });
}

/// The container a browser already has (a reopen keeps it).
pub fn existing(id: &str) -> Option<Retained<NSView>> {
    VIEWS.with_borrow(|v| v.get(id).map(|c| c.view.clone()))
}

/// The browser closed: its container goes too (runtime.rs removed the page first).
pub fn remove(id: &str) {
    if let Some(c) = VIEWS.with_borrow_mut(|v| v.remove(id)) {
        c.view.removeFromSuperview();
    }
}

/// The page takes the keyboard. False while its view doesn't exist yet.
pub fn give_keys(id: &str, window: &Window) -> bool {
    let Some(page) = engine::page_view(id) else {
        tracing::debug!("browser: {id} can't take the keyboard yet: no page view", id = super::log_safe(id));
        return false;
    };
    // SAFETY: CEF's live browser view, main thread.
    let page = unsafe { page.as_ref() };
    let Some(ns_window) = gpui_view(window).ok().and_then(|g| g.window()) else { return false };
    let taken = ns_window.makeFirstResponder(Some(page));
    if !taken {
        tracing::debug!("browser: {id} can't take the keyboard: the window refused", id = super::log_safe(id));
    }
    taken
}

/// Who has AppKit's first responder in the window: GPUI's view, a page
/// (its view or one inside it), or something else (the window itself).
pub fn responder(window: &Window) -> Responder {
    let Ok(gpui) = gpui_view(window) else { return Responder::Other };
    let Some(responder) = gpui.window().and_then(|w| w.firstResponder()) else { return Responder::Other };
    let responder: &AnyObject = &responder;
    let Some(view) = responder.downcast_ref::<NSView>() else { return Responder::Other };
    if std::ptr::eq(view, gpui) {
        return Responder::Herdr;
    }
    page_of(view).map_or(Responder::Other, Responder::Page)
}

/// The window decided who has the keyboard (None: Herdr).
pub fn set_wanted(page: Option<&str>) {
    WANTED.with_borrow_mut(|w| *w = Some(page.map(str::to_owned)));
}

/// CEF asks whether page `id` may take focus (`super::may_focus`).
#[cfg_attr(not(feature = "browser"), allow(dead_code))] // CEF only
pub fn may_take_focus(id: &str) -> bool {
    let pressed = PRESSED.with_borrow(|p| p.as_ref().is_some_and(|(page, at)| page == id && at.elapsed() < CLICK_FOCUS));
    WANTED.with_borrow(|w| super::may_focus(id, w.as_ref().map(Option::as_deref), pressed))
}

fn page_of(view: &NSView) -> Option<String> {
    VIEWS.with_borrow(|views| views.iter().find(|(_, c)| view.isDescendantOf(&c.view)).map(|(id, _)| id.clone()))
}

/// Whether the person just pressed the mouse in this page: CEF's "got focus"
/// is then their click, not the page taking focus on its own (CEF does that
/// when it creates a browser). Answers once per press.
pub fn clicked(id: &str) -> bool {
    PRESSED.with_borrow_mut(|p| match p.take() {
        Some((page, at)) if page == id && at.elapsed() < CLICK_FOCUS => true,
        other => {
            *p = other;
            false
        }
    })
}

/// GPUI takes the keyboard back.
pub fn take_keys(window: &Window) {
    if let Ok(gpui) = gpui_view(window)
        && let Some(ns_window) = gpui.window()
    {
        ns_window.makeFirstResponder(Some(gpui));
    }
}

pub fn set_bindings(bindings: Vec<(String, String)>) {
    tracing::debug!("browser: {} key binding(s) stay with Herdr while a page has the keyboard (and the escape hatch)", bindings.len());
    BINDINGS.with_borrow_mut(|b| *b = bindings);
}

/// Once: an event monitor in front of AppKit's dispatch.
/// - Key down an agent sent a page (`engine::agent_event`): dropped. WebKit
///   sends one the page didn't handle back to the app, and it must not reach
///   a terminal, a keys.json action or a menu.
/// - Key down: while a page has the keyboard, a key bound globally in
///   keys.json (or the escape hatch) runs its action in Herdr and never
///   reaches the page.
/// - Mouse down: notes which page was pressed (`clicked`); a press outside
///   every page while one has the keyboard is a click on Herdr, and the
///   window hears of it (a layout block's page stops holding the keyboard).
pub fn install_monitor() {
    if MONITOR.with_borrow(Option::is_some) {
        return;
    }
    let handler = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit hands the monitor a live event, on the main thread.
        let ev = unsafe { event.as_ref() };
        if ev.r#type() != NSEventType::KeyDown {
            pressed(ev);
            return event.as_ptr();
        }
        if engine::agent_event(ev) {
            tracing::debug!("browser: an agent's key the page didn't take stays in the page");
            return std::ptr::null_mut();
        }
        match route(ev) {
            Some(action) => {
                match keys::native(&action) {
                    Some(native) => {
                        tracing::info!("browser: a page had the keyboard; {} goes to macOS", super::log_safe(&action));
                        run_native(native);
                    }
                    None => {
                        tracing::info!("browser: a page had the keyboard; {} goes to Herdr", super::log_safe(&action));
                        crate::mods::request(crate::mods::Request::Run { name: action, args: serde_json::Value::Null });
                    }
                }
                std::ptr::null_mut()
            }
            None => event.as_ptr(),
        }
    });
    // SAFETY: the handler returns the event it was given, or null to drop it.
    let mask = NSEventMask::KeyDown | NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown;
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &handler) };
    MONITOR.with_borrow_mut(|m| *m = monitor);
}

/// Hiding apps: what GPUI's `cx.hide()` and friends do (src/macos.rs binds
/// them natively; the command does nothing).
fn run_native(native: Native) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = objc2_app_kit::NSApp(mtm);
    match native {
        Native::Hide => app.hide(None),
        Native::HideOthers => app.hideOtherApplications(None),
        Native::ShowAll => app.unhideAllApplications(None),
    }
}

/// A mouse press: in which shown page, if any.
fn pressed(ev: &NSEvent) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    // Another window (DevTools) has its own coordinates and no pages.
    let Some(window) = ev.window(mtm) else { return };
    let at = ev.locationInWindow();
    let page = VIEWS.with_borrow(|views| {
        views.iter().find(|(_, c)| {
            if c.view.isHidden() || c.view.window().is_none_or(|w| !std::ptr::eq(&*w, &*window)) {
                return false;
            }
            let p = c.view.convertPoint_fromView(at, None);
            let b = c.view.bounds();
            p.x >= b.origin.x && p.y >= b.origin.y && p.x < b.origin.x + b.size.width && p.y < b.origin.y + b.size.height
        })
        .map(|(id, _)| id.clone())
    });
    match page {
        Some(id) => {
            PRESSED.with_borrow_mut(|p| *p = Some((id.clone(), Instant::now())));
            engine::pressed(&id);
        }
        None => {
            PRESSED.with_borrow_mut(|p| *p = None);
            let in_page = window.firstResponder().is_some_and(|r| {
                let r: &AnyObject = &r;
                r.downcast_ref::<NSView>().and_then(page_of).is_some()
            });
            if in_page {
                engine::clicked_herdr();
            }
        }
    }
}

/// The action for a key pressed while a page has the keyboard.
fn route(ev: &NSEvent) -> Option<String> {
    let mtm = MainThreadMarker::new()?;
    let responder = ev.window(mtm)?.firstResponder()?;
    let responder: &AnyObject = &responder;
    page_of(responder.downcast_ref::<NSView>()?)?;
    let flags = ev.modifierFlags();
    let text = |s: Option<Retained<objc2_foundation::NSString>>| s.map(|s| s.to_string().to_lowercase()).filter(|s| !s.is_empty());
    let key = keys::named(ev.keyCode())
        .map(str::to_owned)
        .or_else(|| text(ev.charactersByApplyingModifiers(NSEventModifierFlags::empty())))?;
    let press = Press {
        ctrl: flags.contains(NSEventModifierFlags::Control),
        alt: flags.contains(NSEventModifierFlags::Option),
        shift: flags.contains(NSEventModifierFlags::Shift),
        cmd: flags.contains(NSEventModifierFlags::Command),
        key,
        shifted: text(ev.charactersIgnoringModifiers()),
    };
    BINDINGS.with_borrow(|b| keys::find(b, &press).map(str::to_owned))
}
