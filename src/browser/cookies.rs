//! Cookies brought from another browser, set in the profile's cookie manager.

use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::c_int;
use std::rc::Rc;

use cef::*;

use crate::cookie_import::{self, Cookie, Priority, SameSite};

/// What an import did.
pub struct Report {
    pub imported: usize,
    pub failed: usize,
    /// Registrable domains at least one cookie of which was set.
    pub sites: usize,
}

struct Progress {
    left: usize,
    imported: usize,
    failed: usize,
    sites: HashSet<String>,
    manager: CookieManager,
    done: Option<Box<dyn FnOnce(Report)>>,
}

impl Progress {
    fn report(&self) -> Report {
        Report {
            imported: self.imported,
            failed: self.failed,
            sites: self.sites.len(),
        }
    }
}

type Shared = Rc<RefCell<Progress>>;

wrap_set_cookie_callback! {
    struct CookieSet {
        progress: Shared,
        domain: String,
    }

    impl SetCookieCallback {
        fn on_complete(&self, success: c_int) {
            record(&self.progress, &self.domain, success == 1);
        }
    }
}

wrap_completion_callback! {
    struct Flushed {
        progress: Shared,
    }

    impl CompletionCallback {
        fn on_complete(&self) {
            finish(&self.progress);
        }
    }
}

/// Sets every cookie, then flushes the store to disk (Chromium batches
/// writes), then calls `done`. Cookies Chromium refuses count as failed.
pub fn set_all(manager: CookieManager, cookies: Vec<Cookie>, done: Box<dyn FnOnce(Report)>) {
    let progress: Shared = Rc::new(RefCell::new(Progress {
        left: cookies.len(),
        imported: 0,
        failed: 0,
        sites: HashSet::new(),
        manager: manager.clone(),
        done: Some(done),
    }));
    if cookies.is_empty() {
        return finish(&progress);
    }
    for cookie in &cookies {
        let domain = cookie_import::registrable_domain(&cookie.domain);
        let url = CefString::from(cookie.url().as_str());
        let mut callback = CookieSet::new(progress.clone(), domain.clone());
        if manager.set_cookie(Some(&url), Some(&to_cef(cookie)), Some(&mut callback)) != 1 {
            record(&progress, &domain, false);
        }
    }
}

fn record(progress: &Shared, domain: &str, success: bool) {
    let flush = {
        let mut p = progress.borrow_mut();
        if success {
            p.imported += 1;
            p.sites.insert(domain.to_owned());
        } else {
            p.failed += 1;
        }
        p.left -= 1;
        (p.left == 0).then(|| p.manager.clone())
    };
    if let Some(manager) = flush {
        let mut callback = Flushed::new(progress.clone());
        if manager.flush_store(Some(&mut callback)) != 1 {
            finish(progress);
        }
    }
}

fn finish(progress: &Shared) {
    let (done, report) = {
        let mut p = progress.borrow_mut();
        (p.done.take(), p.report())
    };
    if let Some(done) = done {
        done(report);
    }
}

/// Flushes the profile's cookies to disk, then calls `done`.
pub fn flush(manager: &CookieManager, done: Box<dyn FnOnce()>) {
    let done = Rc::new(RefCell::new(Some(done)));
    let mut callback = FlushedAlone::new(done.clone());
    if manager.flush_store(Some(&mut callback)) != 1 {
        let done = done.borrow_mut().take();
        if let Some(done) = done {
            done();
        }
    }
}

wrap_completion_callback! {
    struct FlushedAlone {
        done: Rc<RefCell<Option<Box<dyn FnOnce()>>>>,
    }

    impl CompletionCallback {
        fn on_complete(&self) {
            let done = self.done.borrow_mut().take();
            if let Some(done) = done {
                done();
            }
        }
    }
}

/// A host-only cookie has an empty domain, so Chromium keeps it a host cookie.
fn to_cef(cookie: &Cookie) -> cef::Cookie {
    use cef::sys::{cef_cookie_priority_t as P, cef_cookie_same_site_t as S};
    let now = cookie_import::now_micros();
    let domain = if cookie.domain.starts_with('.') {
        cookie.domain.as_str()
    } else {
        ""
    };
    cef::Cookie {
        name: CefString::from(cookie.name.as_str()),
        value: CefString::from(cookie.value.as_str()),
        domain: CefString::from(domain),
        path: CefString::from(cookie.path.as_str()),
        secure: c_int::from(cookie.secure),
        httponly: c_int::from(cookie.http_only),
        creation: Basetime { val: now },
        last_access: Basetime { val: now },
        has_expires: c_int::from(cookie.expires.is_some()),
        expires: Basetime {
            val: cookie.expires.unwrap_or(0),
        },
        same_site: CookieSameSite::from(match cookie.same_site {
            SameSite::Unspecified => S::CEF_COOKIE_SAME_SITE_UNSPECIFIED,
            SameSite::None => S::CEF_COOKIE_SAME_SITE_NO_RESTRICTION,
            SameSite::Lax => S::CEF_COOKIE_SAME_SITE_LAX_MODE,
            SameSite::Strict => S::CEF_COOKIE_SAME_SITE_STRICT_MODE,
        }),
        priority: CookiePriority::from(match cookie.priority {
            Priority::Low => P::CEF_COOKIE_PRIORITY_LOW,
            Priority::Medium => P::CEF_COOKIE_PRIORITY_MEDIUM,
            Priority::High => P::CEF_COOKIE_PRIORITY_HIGH,
        }),
        ..Default::default()
    }
}
