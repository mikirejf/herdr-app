//! The native "Let agents drive this browser?" dialog: the one control that
//! turns agent access on.
//!
//! An app-modal `NSAlert`. AppKit draws it in its own window, above every
//! GPUI and CEF view, and no Herdr command, mod, layout or page can draw
//! over it, move it or answer it. Cancel is the default button (Return,
//! Esc); Allow has no key equivalent and stays disabled for
//! `super::ALLOW_AFTER` after the dialog shows. What the reply means is
//! `agent::turns_access_on`.
//!
//! `runModal` spins a modal run loop inside the call. Herdr calls it from a
//! GPUI foreground task, which runs in a main-queue GCD block with nothing
//! borrowed: GPUI's own main-queue work (its tasks, the control socket's
//! requests, the CEF pump) waits until the dialog closes, and AppKit
//! callbacks into GPUI meanwhile find the app free.

use std::time::Instant;

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertStyle, NSApp, NSModalPanelRunLoopMode};
use objc2_foundation::{NSRunLoop, NSString, NSTimer};

use super::{Answer, Ask, Reply, agent};

/// Whether Herdr is the active app (the dialog opens only into it).
pub fn app_active() -> bool {
    MainThreadMarker::new().is_some_and(|mtm| NSApp(mtm).isActive())
}

/// Shows `ask` and waits for the answer. Off the main thread: no dialog.
pub fn ask(ask: &Ask) -> Reply {
    let Some(mtm) = MainThreadMarker::new() else {
        return Reply { answer: Answer::Closed, after: std::time::Duration::ZERO, app_active: false };
    };
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.setMessageText(&NSString::from_str(&ask.title));
    alert.setInformativeText(&NSString::from_str(&ask.body));
    // Added first: NSAlertFirstButtonReturn, the default (Return), and Esc by its title.
    let cancel = alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    cancel.setKeyEquivalent(&NSString::from_str("\r"));
    // Added second: NSAlertSecondButtonReturn. No key reaches it.
    let allow = alert.addButtonWithTitle(&NSString::from_str("Allow"));
    allow.setKeyEquivalent(&NSString::from_str(""));
    allow.setEnabled(false);
    let enable = {
        let allow = allow.clone();
        RcBlock::new(move |_: std::ptr::NonNull<NSTimer>| allow.setEnabled(true))
    };
    // SAFETY: the timer fires on this (the main) thread's run loop, in the
    // modal mode `runModal` runs, while the alert and its button are alive.
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(super::ALLOW_AFTER.as_secs_f64(), false, &enable) };
    // SAFETY: NSModalPanelRunLoopMode is an AppKit constant, valid for the process.
    unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSModalPanelRunLoopMode) };
    let shown = Instant::now();
    let response = alert.runModal();
    timer.invalidate();
    let reply = Reply { answer: agent::answer_of(response), after: shown.elapsed(), app_active: app_active() };
    tracing::debug!("browser: agent access dialog for {} answered {:?} after {} ms", super::log_safe(&ask.browser), reply.answer, reply.after.as_millis());
    reply
}
