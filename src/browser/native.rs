//! Browser panes on macOS with the `browser` feature: the registry decides
//! what is open, runtime.rs drives CEF, view.rs places the native views.

use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, App, IntoElement, ParentElement, Styled, Window, canvas, div};
use serde_json::{Value, json};

use super::agent::{self, Caller, Candidate};
use super::registry::{self, Page, Registry};
use super::{Hand, Look, Responder, Source, Update, emulate, engine, proxy, view};
use crate::layout::Node;
use crate::model::Model;

/// What CEF reports about one browser (main thread, from the pump).
pub enum Event {
    Page { browser: String, page: Page },
    /// CEF says the page got focus: a click into it, or CEF on its own.
    Focused(String),
    CloseAsked(String),
    /// A mouse press on Herdr (outside every page) while a page had the keyboard.
    ClickedHerdr,
    /// A page hid under an overlay or came back: who has the keyboard may change.
    Covers,
}

pub struct Browsers {
    registry: Registry,
    events: Option<async_channel::Receiver<Event>>,
    /// For the log: who the window last decided has the keyboard (inner
    /// None: Herdr), and who last really had it.
    wanted: Option<Option<String>>,
    holder: Option<Responder>,
    /// Why CEF can't run, shown in every browser block.
    error: Option<String>,
    /// The browser a native agent-access dialog is open for.
    asking: Option<String>,
}

impl Browsers {
    pub fn new() -> Self {
        let (tx, rx) = async_channel::unbounded();
        engine::set_events(tx);
        Browsers { registry: Registry::default(), events: Some(rx), wanted: None, holder: None, error: None, asking: None }
    }

    pub fn events(&mut self) -> Option<async_channel::Receiver<Event>> {
        self.events.take()
    }

    pub fn apply(&mut self, event: Event) -> Update {
        match event {
            Event::Page { browser, page } => {
                let url = page.url.clone();
                let moved = self.registry.page(&browser, page);
                Update { url: moved.then_some(url), browser, ..Update::default() }
            }
            // Only a press in the page makes it the person's choice; CEF also
            // focuses a page it just created, which must not move Herdr's focus
            // (sync_browser_keys then puts the keyboard back where it belongs).
            Event::Focused(browser) if view::clicked(&browser) => {
                tracing::debug!("browser: the person clicked into {browser}", browser = super::log_safe(&browser));
                self.registry.focused(&browser, crate::model::now_ms());
                Update { browser, focused: true, ..Update::default() }
            }
            Event::Focused(browser) => {
                if self.wanted.as_ref().is_some_and(|w| w.as_deref() == Some(browser.as_str())) {
                    tracing::debug!("browser: {browser} got focus (Herdr gave it the keyboard)", browser = super::log_safe(&browser));
                } else {
                    tracing::debug!("browser: {browser} took focus on its own (not a click)", browser = super::log_safe(&browser));
                }
                Update { browser, ..Update::default() }
            }
            Event::CloseAsked(browser) => Update { browser, close: true, ..Update::default() },
            Event::ClickedHerdr => Update { clicked_herdr: true, ..Update::default() },
            Event::Covers => Update { covers: true, ..Update::default() },
        }
    }

    /// A new browser's id and its normalized url.
    pub fn prepare_open(&mut self, url: &str, model: &Model, chrome: &Node) -> Result<(String, String), String> {
        let url = registry::normalize_url(url)?;
        let blocks = registry::blocks(model, chrome);
        Ok((self.registry.issue(blocks.iter().map(|b| b.browser.as_str())), url))
    }

    /// Opens browsers for new blocks (and blocks restored from a saved
    /// layout), closes those whose block is gone.
    pub fn reconcile(&mut self, model: &Model, chrome: &Node, window: &mut Window, cx: &mut App) {
        let changes = self.registry.reconcile(&registry::blocks(model, chrome));
        for id in &changes.close {
            // Access on with no agent yet is logged off too.
            if proxy::close(id) {
                tracing::info!("{}", super::access_line(false, id, Source::Close));
            }
            engine::close(id);
            view::remove(id);
        }
        if changes.open.is_empty() {
            return;
        }
        if let Err(e) = engine::start(cx) {
            tracing::error!("browser: {e:#}");
            self.error = Some(format!("{e:#}"));
            return;
        }
        for (id, url) in changes.open {
            let agent_profile = self.registry.get(&id).is_some_and(|e| e.agent_profile);
            match view::container(&id, window) {
                Ok(parent) => engine::open(&id, parent, url, agent_profile),
                Err(e) => tracing::error!("browser: no view for {id}: {e:#}", id = super::log_safe(&id)),
            }
        }
        view::install_monitor();
    }

    /// navigate / back / forward / reload / open_external / devtools / zoom / viewport.
    pub fn command(&mut self, name: &str, browser: &str, args: &Value) -> Result<Value, String> {
        if browser.is_empty() {
            return Err(format!("{name} needs browser (an id from `herdr-app ls`), or a focused browser pane"));
        }
        if self.registry.get(browser).is_none() {
            return Err(format!("no open browser {browser:?}: see `herdr-app ls`"));
        }
        match name {
            "browser.navigate" => {
                let url = args.get("url").and_then(Value::as_str).unwrap_or_default();
                let url = registry::normalize_url(url)?;
                engine::navigate(browser, &url)?;
                Ok(json!({ "browser": browser, "url": url }))
            }
            "browser.back" => engine::history(browser, -1).map(|()| json!({ "browser": browser })),
            "browser.forward" => engine::history(browser, 1).map(|()| json!({ "browser": browser })),
            "browser.reload" => engine::reload(browser).map(|()| json!({ "browser": browser })),
            // Herdr's own record of the page's url (the engine's), never one a caller passes.
            "browser.open_external" => {
                let url = self.registry.get(browser).map(|e| e.page.url.clone()).unwrap_or_default();
                let url = registry::external_url(&url)?;
                crate::links::open(&crate::links::Target::Url(url));
                tracing::info!("browser: {} opened in the default browser", super::log_safe(browser));
                Ok(json!({ "browser": browser }))
            }
            "browser.devtools" => {
                let open = args.get("open").and_then(Value::as_bool);
                // `note`: where the engine's inspector opens, if not in a window
                // of its own (WebKit's is Safari's); the window shows it as a toast.
                engine::devtools(browser, open).map(|open| match engine::DEVTOOLS_NOTE.filter(|_| open) {
                    Some(note) => json!({ "browser": browser, "devtools": open, "note": note }),
                    None => json!({ "browser": browser, "devtools": open }),
                })
            }
            "browser.zoom" => {
                let percent = emulate::zoom_target(args, engine::zoom(browser)?)?;
                let now = engine::set_zoom(browser, percent)?;
                Ok(json!({ "browser": browser, "zoom": emulate::shown(now) }))
            }
            "browser.viewport" => {
                let viewport = emulate::viewport(args)?;
                let shown = viewport.as_ref().map(emulate::Viewport::json);
                engine::set_viewport(browser, viewport)?;
                Ok(json!({ "browser": browser, "viewport": shown }))
            }
            other => Err(format!("{other} is not a browser command")),
        }
    }

    pub fn info(&self, browser: &str, focused: bool) -> Option<Value> {
        self.registry.get(browser).map(|e| e.json(focused))
    }

    pub fn list(&self, focused_pane: Option<&str>) -> Vec<Value> {
        self.registry.list().iter().map(|e| e.json(e.pane.is_some() && e.pane.as_deref() == focused_pane)).collect()
    }

    /// Gives the keyboard to `page` (`super::keyboard_page`), or back to GPUI
    /// when it's None. Compared with who really has it (AppKit's first
    /// responder), so a page that took focus on its own loses it again.
    pub fn keys(&mut self, page: Option<&str>, window: &mut Window) {
        if self.wanted.as_ref().map(Option::as_deref) != Some(page) {
            tracing::debug!("browser: the keyboard belongs to {}", page.map_or_else(|| "Herdr".into(), super::log_safe));
            self.wanted = Some(page.map(str::to_owned));
        }
        // Before giving it: the page's own focus request is then allowed.
        view::set_wanted(page);
        let actual = view::responder(window);
        let hand = super::hand(page, &actual);
        // A page or the bare window has it, and shouldn't.
        if hand != Hand::Keep && actual != Responder::Herdr {
            tracing::debug!("browser: taking the keyboard back from {actual}");
        }
        let holder = match hand {
            Hand::Keep => actual,
            // Not yet created: Herdr holds it meanwhile (not a hidden page),
            // and the next sync tries again.
            Hand::Give(id) if !view::give_keys(id, window) => {
                if actual != Responder::Herdr {
                    view::take_keys(window);
                }
                Responder::Herdr
            }
            Hand::Give(id) => {
                engine::set_focus(id);
                Responder::Page(id.to_owned())
            }
            Hand::TakeBack => {
                view::take_keys(window);
                Responder::Herdr
            }
        };
        if self.holder.as_ref() != Some(&holder) {
            tracing::debug!("browser: {holder} has the keyboard");
            self.holder = Some(holder);
        }
    }

    /// An agent-profile browser's (access on, last attaching terminal).
    /// None: no such browser, or the person's profile.
    pub fn agent_state(&self, browser: &str) -> Option<(bool, Option<String>)> {
        self.registry.get(browser).filter(|e| e.agent_profile).map(|e| (e.agent_access, e.last_agent.clone()))
    }

    /// The browsers with agent access on, with their last attaching terminal.
    pub fn agents_on(&self) -> Vec<(String, Option<String>)> {
        self.registry.list().iter().filter(|e| e.agent_profile && e.agent_access).map(|e| (e.browser.clone(), e.last_agent.clone())).collect()
    }

    /// Agent access on or off (`agent::may_set_access`: on only from the
    /// pane's own control). Off ends the agent's token and connection; the
    /// connection's end reopens the page (engine::agent_gone).
    pub fn set_agent_access(&mut self, browser: &str, on: bool, from: Source) -> Result<(), String> {
        agent::may_set_access(on, from).map_err(str::to_owned)?;
        let e = self.registry.get(browser).ok_or_else(|| format!("no open browser {browser:?}: see `herdr-app ls`"))?;
        if on {
            agent::needs_agent_profile(e.agent_profile).map_err(str::to_owned)?;
        }
        self.registry.set_access(browser, on);
        proxy::set_access(browser, on);
        if !on {
            proxy::revoke(browser);
        }
        tracing::info!("{}", super::access_line(on, browser, from));
        Ok(())
    }

    /// The caller's browser and a token url for it (`{ attach }` on the
    /// control socket). `tab_of`: a pane's tab.
    /// `check`: the same choice and checks, no token (`herdr.check`).
    pub fn attach(&mut self, terminal: &str, space: &str, tab: &str, wanted: Option<&str>, check: bool, tab_of: &dyn Fn(&str) -> Option<String>) -> Result<Value, String> {
        let entries: Vec<(&registry::Entry, Option<String>)> =
            self.registry.list().iter().map(|e| (e, e.pane.as_deref().and_then(tab_of))).collect();
        let candidates: Vec<Candidate> = entries
            .iter()
            .map(|(e, t)| Candidate {
                browser: &e.browser,
                space: e.space.as_deref(),
                tab: t.as_deref(),
                focused_at: e.focused_at,
                agent_profile: e.agent_profile,
                access: e.agent_access,
            })
            .collect();
        let (browser, how) = agent::select(&Caller { space, tab, wanted }, &candidates)?;
        let chosen = candidates.iter().find(|c| c.browser == browser).ok_or("the browser went away")?;
        agent::ready(chosen)?;
        let (browser, pane, space) = (browser.to_owned(), entries.iter().find(|(e, _)| e.browser == browser).and_then(|(e, _)| e.pane.clone()), chosen.space.map(str::to_owned));
        if check {
            return Ok(json!({ "browser": browser, "pane": pane, "space": space, "selection": how }));
        }
        let url = proxy::attach(&browser)?;
        self.registry.attached(&browser, terminal);
        Ok(json!({ "cdpUrl": url, "browser": browser, "pane": pane, "space": space, "selection": how }))
    }

    /// The pages hidden under an overlay now (they can't have the keyboard).
    pub fn covered(&self) -> Vec<String> {
        view::covered()
    }

    /// Goes inside an overlay (absolute, its full size): reports its bounds
    /// while painting, so a page under it hides (`label` names it in the log).
    pub fn cover(&self, label: String) -> Option<AnyElement> {
        Some(canvas(|_, _, _| {}, move |bounds, _, _, _| view::cover(&label, bounds)).absolute().size_full().into_any_element())
    }

    /// The native dialog for "Allow agent…" on `browser`, if it may open now
    /// (`agent::may_ask`: the app active, no other dialog open; an open
    /// agent-profile browser). Marks it open until `answer`.
    pub fn ask(&mut self, browser: &str) -> Result<super::Ask, String> {
        agent::may_ask(super::confirm::app_active(), self.asking.as_deref()).map_err(str::to_owned)?;
        let e = self.registry.get(browser).ok_or_else(|| format!("no open browser {browser:?}"))?;
        agent::needs_agent_profile(e.agent_profile).map_err(str::to_owned)?;
        let site = agent::site(&e.page.url);
        let (title, body) = agent::consent_text(browser, &site).map_err(str::to_owned)?;
        self.asking = Some(browser.to_owned());
        Ok(super::Ask { browser: browser.to_owned(), site, title, body })
    }

    /// The dialog's reply: access turns on only if `agent::turns_access_on`
    /// says so, for a browser still open on the agent profile at the site
    /// the dialog named. Ok(true): it is on now. Every refusal is logged.
    /// The check reads the registry, which page events update through the
    /// window's event channel; this runs as the dialog returns, before any
    /// event from while it was open is applied, so a navigation during the
    /// dialog isn't seen here (`docs/browser-dev.md` says so).
    pub fn answer(&mut self, ask: &super::Ask, reply: super::Reply) -> Result<bool, String> {
        self.asking = None;
        let b = &ask.browser;
        let unchanged = self.registry.get(b).is_some_and(|e| e.agent_profile && agent::site(&e.page.url) == ask.site);
        if let Err(why) = agent::turns_access_on(&reply, unchanged) {
            tracing::info!("browser: agent access stays off browser={}: {why}", super::log_safe(b));
            return Ok(false);
        }
        self.set_agent_access(b, true, Source::NativeConfirm)?;
        Ok(true)
    }

    pub fn set_bindings(&mut self, bindings: Vec<(String, String)>) {
        view::set_bindings(bindings);
    }

    /// The block: what shows while the page isn't there (opening, an
    /// error, another block owns it, an overlay over it: the page's title
    /// and url on the theme's `surface`), and a canvas that puts the page's
    /// view on the block each frame.
    pub fn element(&self, node: &str, browser: &str, url: &str, look: Look) -> AnyElement {
        let owner = self.registry.owned_by(node).filter(|e| e.browser == browser);
        let (title, note) = match (&self.error, owner) {
            (Some(e), _) => (None, format!("The browser can't start: {e}")),
            (None, Some(e)) if e.page.loading => (None, format!("Opening {}", e.page.url)),
            (None, Some(e)) => (Some(e.page.title.clone()).filter(|t| !t.is_empty()), e.page.url.clone()),
            (None, None) if self.registry.get(browser).is_some() => (None, format!("Browser {browser} shows in another block.")),
            // An agent-profile block whose id isn't shaped like Herdr's (`registry::blocks`).
            (None, None) if !registry::herdr_id(browser) => (None, "This browser doesn't open: an agent-profile browser needs an id shaped like Herdr's (b and 1-9 digits: b1, b2, …).".to_owned()),
            (None, None) => (None, format!("Opening {url}")),
        };
        let mut el = div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .p_4()
            .overflow_hidden()
            .bg(look.surface)
            .when_some(title, |d, t| d.text_color(look.text).child(t))
            .child(div().text_color(look.dim).child(note));
        if let (None, Some(_)) = (&self.error, owner) {
            let id = browser.to_owned();
            el = el.child(canvas(|_, _, _| {}, move |bounds, _, window, _| view::place(&id, bounds, window)).absolute().size_full());
        }
        el.into_any_element()
    }

    /// Painted after everything else: hides the pages whose block wasn't painted this frame.
    pub fn frame_end(&self) -> Option<AnyElement> {
        Some(canvas(|_, _, _| {}, |_, _, window, _| view::end_frame(window)).absolute().size_0().into_any_element())
    }
}
