//! Which browsers the window has open, and what each one shows: pure data,
//! no CEF, so the bookkeeping is testable without a window.
//!
//! The layout is the source of truth: a browser block (a pane in a tab, or a
//! block in the shown layout) says a browser with that id should be open at
//! its saved url. `reconcile` compares the blocks with what is open and says
//! what to open and what to close.

// Default builds compile these rules only for their tests; the browser feature uses them.
#![cfg_attr(not(all(target_os = "macos", feature = "browser-pane")), allow(dead_code))]

use crate::layout::{Content, Node};
use crate::model::Model;
use serde_json::{Value, json};

/// A browser block somewhere in the window's world.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub browser: String,
    /// Where it was last (the layout's copy): a browser that isn't open opens here.
    pub url: String,
    /// The layout node showing it (its pane id, for a pane).
    pub node: String,
    /// The space and pane, for a browser in a tab; None for a layout block.
    pub space: Option<String>,
    pub pane: Option<String>,
    /// Opened with `profile: "agent"` (a separate Chromium profile).
    pub agent_profile: bool,
}

/// `profile: "agent"`; anything else (absent, `"person"`) is the person's profile.
pub fn is_agent(profile: Option<&str>) -> bool {
    profile == Some("agent")
}

/// What the page in an open browser says about itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    /// Its zoom in percent (Chromium keeps one per site in the profile).
    pub zoom: f64,
    /// The viewport `browser.viewport` set on this page; None: it fills its pane.
    pub viewport: Option<super::emulate::Viewport>,
}

impl Default for Page {
    fn default() -> Self {
        Page { url: String::new(), title: String::new(), loading: false, can_go_back: false, can_go_forward: false, zoom: 100.0, viewport: None }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub browser: String,
    pub page: Page,
    /// The node that shows it. Another block with the same id shows a placeholder.
    pub node: String,
    pub space: Option<String>,
    pub pane: Option<String>,
    /// Off until the person turns it on; never saved.
    pub agent_access: bool,
    pub agent_profile: bool,
    /// The terminal that last attached an agent, as it claimed (`HC_TERMINAL_ID`).
    pub last_agent: Option<String>,
    /// When the page last took the keyboard, ms since the epoch (0: never).
    pub focused_at: u64,
}

impl Entry {
    /// One browser as mods and `herdr-app ls` see it.
    /// The url keeps its userinfo, query and fragment only while agent access
    /// is on (codes and magic links in a url stay out of `herdr-app ls`
    /// otherwise), and so does a title that is the url (a page with none).
    pub fn json(&self, focused: bool) -> Value {
        let (url, title) = if self.agent_access {
            (self.page.url.clone(), self.page.title.clone())
        } else {
            (super::public_url(&self.page.url), super::public_title(&self.page.title, &self.page.url))
        };
        json!({
            "browser": self.browser, "url": url, "title": title, "profile": if self.agent_profile { "agent" } else { "person" },
            "last_agent": self.last_agent,
            "loading": self.page.loading, "can_go_back": self.page.can_go_back, "can_go_forward": self.page.can_go_forward,
            "space": self.space, "pane": self.pane, "agent_access": self.agent_access, "focused": focused,
            "zoom": super::emulate::shown(self.page.zoom), "viewport": self.page.viewport.as_ref().map(super::emulate::Viewport::json),
        })
    }
}

/// What `reconcile` asks of the native side.
#[derive(Debug, Default, PartialEq)]
pub struct Changes {
    /// (browser, url): open these.
    pub open: Vec<(String, String)>,
    /// Close these (their block is gone).
    pub close: Vec<String>,
}

#[derive(Default)]
pub struct Registry {
    /// In the order they opened.
    entries: Vec<Entry>,
    /// The highest `bN` issued or seen in a block this run: new ids go above
    /// it, so a closed browser's id never names another page (agents hold ids).
    highest: u64,
}

/// The page a browser with no saved url opens on.
pub const BLANK: &str = "about:blank";

impl Registry {
    /// Makes the open browsers match the blocks. A block whose browser isn't
    /// open (new, or restored from a saved layout) opens at its saved url; an
    /// open browser with no block left closes. A moved block (another tab,
    /// another space) only updates where the browser is.
    pub fn reconcile(&mut self, blocks: &[Block]) -> Changes {
        let mut changes = Changes::default();
        let mut seen: Vec<&str> = Vec::new();
        for b in blocks {
            self.highest = self.highest.max(id_number(&b.browser).unwrap_or(0));
            // The first block with an id owns the browser.
            if seen.contains(&b.browser.as_str()) {
                continue;
            }
            seen.push(&b.browser);
            match self.entries.iter_mut().find(|e| e.browser == b.browser) {
                Some(e) => {
                    e.node = b.node.clone();
                    e.space = b.space.clone();
                    e.pane = b.pane.clone();
                }
                None => {
                    let url = if b.url.trim().is_empty() { BLANK.to_owned() } else { b.url.clone() };
                    self.entries.push(Entry {
                        browser: b.browser.clone(),
                        page: Page { url: url.clone(), loading: true, ..Page::default() },
                        node: b.node.clone(),
                        space: b.space.clone(),
                        pane: b.pane.clone(),
                        agent_access: false,
                        agent_profile: b.agent_profile,
                        last_agent: None,
                        focused_at: 0,
                    });
                    changes.open.push((b.browser.clone(), url));
                }
            }
        }
        self.entries.retain(|e| {
            let keep = seen.contains(&e.browser.as_str());
            if !keep {
                changes.close.push(e.browser.clone());
            }
            keep
        });
        changes
    }

    /// A new browser id, `b1`, `b2`, ...: short, so people and agents can
    /// type it, and never one this run has given out or seen in a block, so
    /// an id someone still holds can't name another page. `taken`: ids in
    /// blocks the registry hasn't seen yet.
    pub fn issue<'a>(&mut self, taken: impl IntoIterator<Item = &'a str>) -> String {
        let highest = taken.into_iter().filter_map(id_number).fold(self.highest, u64::max);
        self.highest = highest + 1;
        format!("b{}", self.highest)
    }

    pub fn get(&self, browser: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.browser == browser)
    }

    pub fn list(&self) -> &[Entry] {
        &self.entries
    }

    /// The browser a node shows, if that node owns it.
    pub fn owned_by(&self, node: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.node == node)
    }

    /// The page reported a change. True when its url changed (the layout's
    /// saved url follows it).
    pub fn page(&mut self, browser: &str, page: Page) -> bool {
        let Some(e) = self.entries.iter_mut().find(|e| e.browser == browser) else { return false };
        let moved = e.page.url != page.url && !page.url.is_empty();
        let url = if page.url.is_empty() { std::mem::take(&mut e.page.url) } else { page.url.clone() };
        e.page = Page { url, ..page };
        moved
    }

    /// Turns agent access on or off; false when no such browser.
    pub fn set_access(&mut self, browser: &str, on: bool) -> bool {
        let Some(e) = self.entries.iter_mut().find(|e| e.browser == browser) else { return false };
        e.agent_access = on;
        true
    }

    pub fn attached(&mut self, browser: &str, terminal: &str) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.browser == browser) {
            e.last_agent = Some(terminal.to_owned());
        }
    }

    /// The page took the keyboard.
    pub fn focused(&mut self, browser: &str, now: u64) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.browser == browser) {
            e.focused_at = now;
        }
    }
}

/// Every browser block: the tabs' panes (with their space), then the shown
/// layout's own blocks (on every space, so none). An agent-profile block
/// whose id isn't shaped like Herdr's (`herdr_id`) is left out, so it never
/// opens and never reaches the consent dialog. Only the shape is checked:
/// a block may use an id Herdr never gave out.
pub fn blocks(model: &Model, chrome: &Node) -> Vec<Block> {
    let mut out = Vec::new();
    for space in &model.spaces {
        for tab in &space.tabs {
            for n in tab.root.walk() {
                if let Some(Content::Browser { browser, url, profile }) = n.content() {
                    out.push(Block {
                        browser: browser.clone(),
                        url: url.clone(),
                        agent_profile: is_agent(profile.as_deref()),
                        node: n.id.clone(),
                        space: Some(space.id.clone()),
                        pane: Some(n.id.clone()),
                    });
                }
            }
        }
    }
    for n in chrome.walk() {
        if let Some(Content::Browser { browser, url, profile }) = n.content() {
            out.push(Block { browser: browser.clone(), url: url.clone(), agent_profile: is_agent(profile.as_deref()), node: n.id.clone(), space: None, pane: None });
        }
    }
    // An agent-profile block with an id not shaped like Herdr's never opens.
    out.retain(may_open);
    out
}

/// The number in an id Herdr gives out (`b12` → 12).
fn id_number(id: &str) -> Option<u64> {
    herdr_id(id).then(|| id[1..].parse().ok()).flatten()
}

/// Whether `id` has the shape of an id Herdr gives out: `b1` to
/// `b999999999`. Only such a browser may open on the agent profile, so the
/// native consent dialog, which names the browser, never shows an id an
/// agent wrote into a layout block as a sentence of its own.
pub fn herdr_id(id: &str) -> bool {
    id.strip_prefix('b').is_some_and(|n| (1..=9).contains(&n.len()) && !n.starts_with('0') && n.bytes().all(|c| c.is_ascii_digit()))
}

/// A block that may open: an agent-profile block needs a Herdr-shaped id
/// (`herdr_id`); a person-profile block may use any id (a layout's `side`).
fn may_open(b: &Block) -> bool {
    !b.agent_profile || herdr_id(&b.browser)
}

/// What `browser.open`/`browser.navigate` accept: an http(s) url,
/// `about:blank`, or a host (`example.com`, `localhost:3000`). Everything
/// else fails the one URL gate (`agent::allowed_url`): `file:`, `data:`,
/// `chrome:`, `javascript:` and the rest never open.
pub fn normalize_url(url: &str) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("the url is empty".into());
    }
    if url.starts_with('/') {
        return Err("file pages don't open in Herdr's browser: serve the folder over http (just browser-fixture serves scripts/)".into());
    }
    let scheme = url.split_once(':').map(|(s, _)| s.to_ascii_lowercase());
    let url = match scheme.as_deref() {
        Some("javascript") => return Err("javascript: urls are not opened".into()),
        Some("http" | "https" | "about") => url.to_owned(),
        // `localhost:3000`, `127.0.0.1:8080`: a host and port, not a scheme.
        _ if is_local(url) => format!("http://{url}"),
        Some(s) if !url[s.len() + 1..].starts_with(|c: char| c.is_ascii_digit()) && !s.contains('.') => url.to_owned(),
        _ => format!("https://{url}"),
    };
    if super::agent::allowed_url(&url) {
        Ok(url)
    } else {
        Err(format!("only http, https and about:blank pages open in Herdr's browser, not {}", super::agent::loggable(&url)))
    }
}

/// The url `browser.open_external` hands to the person's default browser: the
/// page's own url as the engine reports it, http or https only, with nothing
/// `open` could read as a flag or a second argument.
pub fn external_url(url: &str) -> Result<String, String> {
    let scheme = url.split_once("://").map(|(s, _)| s.to_ascii_lowercase());
    if !matches!(scheme.as_deref(), Some("http" | "https")) {
        return Err("only http and https pages open in the default browser".into());
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the page's url has spaces or control characters; it isn't opened".into());
    }
    Ok(url.to_owned())
}

fn is_local(url: &str) -> bool {
    let host = url.split(['/', ':']).next().unwrap_or_default();
    host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host.ends_with(".localhost")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Axis;
    use crate::model::Tab;

    fn browser_pane(id: &str, browser: &str, url: &str) -> Node {
        Node::block(id, Content::Browser { browser: browser.into(), url: url.into(), profile: None })
    }

    fn block(browser: &str, url: &str, node: &str, space: Option<&str>) -> Block {
        Block { browser: browser.into(), url: url.into(), node: node.into(), space: space.map(Into::into), pane: space.map(|_| node.into()), agent_profile: false }
    }

    #[test]
    fn ls_shows_an_access_off_page_with_no_title_without_its_query() {
        let mut r = Registry::default();
        r.reconcile(&[block("b2", "http://127.0.0.1:18766/", "p2", Some("sp1"))]);
        // A page with no <title>: Chromium titles it with its url, scheme dropped.
        let url = "http://me:pw@127.0.0.1:18766/notitle.html?code=SECRETQ2#fragSECRET";
        r.page("b2", Page { url: url.into(), title: "127.0.0.1:18766/notitle.html?code=SECRETQ2".into(), ..Page::default() });
        let off = r.get("b2").unwrap().json(false);
        assert_eq!(off["url"], "http://127.0.0.1:18766/notitle.html");
        assert_eq!(off["title"], "127.0.0.1:18766/notitle.html");
        assert!(!off.to_string().contains("SECRET") && !off.to_string().contains("pw@"), "{off}");
        // Access on: the agent's page, as it is.
        r.set_access("b2", true);
        let on = r.get("b2").unwrap().json(false);
        assert_eq!((on["url"].as_str(), on["title"].as_str()), (Some(url), Some("127.0.0.1:18766/notitle.html?code=SECRETQ2")));
    }

    #[test]
    fn ls_shows_a_pages_zoom_and_viewport() {
        let mut r = Registry::default();
        r.reconcile(&[block("b1", "https://a/", "p1", Some("sp1"))]);
        let e = r.get("b1").unwrap().json(false);
        assert_eq!((e["zoom"].clone(), e["viewport"].clone()), (serde_json::json!(100), Value::Null));
        let phone = super::super::emulate::viewport(&serde_json::json!({ "preset": "phone" })).unwrap();
        r.page("b1", Page { url: "https://a/".into(), zoom: 200.0 / 3.0, viewport: phone, ..Page::default() });
        let e = r.get("b1").unwrap().json(false);
        assert_eq!(e["zoom"], 67);
        assert_eq!(e["viewport"], serde_json::json!({ "width": 390, "height": 844, "mobile": true, "preset": "phone" }));
    }

    #[test]
    fn a_block_without_an_open_browser_opens_at_its_saved_url() {
        let mut r = Registry::default();
        // A restored layout: the browser isn't open yet.
        let changes = r.reconcile(&[block("b1", "https://example.com/a", "p1", Some("sp1"))]);
        assert_eq!(changes, Changes { open: vec![("b1".into(), "https://example.com/a".into())], close: vec![] });
        assert_eq!(r.get("b1").map(|e| e.page.url.as_str()), Some("https://example.com/a"));
        // Nothing changed: nothing to do.
        assert_eq!(r.reconcile(&[block("b1", "https://example.com/a", "p1", Some("sp1"))]), Changes::default());
        // A block saved without a url opens on a blank page.
        let changes = r.reconcile(&[block("b1", "", "p1", Some("sp1")), block("b2", " ", "p2", Some("sp1"))]);
        assert_eq!(changes.open, vec![("b2".into(), BLANK.into())]);
    }

    /// The header and the window's agent strip follow the open browser, not
    /// the block: an agent that edits a layout to drop `profile: "agent"`
    /// keeps the browser in the agent profile with its access and marker.
    #[test]
    fn a_block_edit_changes_neither_the_open_browsers_profile_nor_its_access() {
        let mut r = Registry::default();
        let agent = Block { agent_profile: true, ..block("b1", "https://a/", "p1", Some("sp1")) };
        r.reconcile(&[agent.clone()]);
        assert!(r.set_access("b1", true));
        let edited = Block { agent_profile: false, ..agent };
        assert_eq!(r.reconcile(&[edited]), Changes::default());
        let e = r.get("b1").unwrap();
        assert!(e.agent_profile && e.agent_access, "a layout edit turned the marker's state off");
        // And a person-profile browser doesn't become an agent one by adding the field.
        r.reconcile(&[block("b2", "https://b/", "p2", Some("sp1"))]);
        r.reconcile(&[Block { agent_profile: true, ..block("b2", "https://b/", "p2", Some("sp1")) }]);
        assert!(!r.get("b2").unwrap().agent_profile);
    }

    #[test]
    fn a_gone_block_closes_its_browser_and_a_moved_one_keeps_it() {
        let mut r = Registry::default();
        r.reconcile(&[block("b1", "https://a/", "p1", Some("sp1")), block("b2", "https://b/", "p2", Some("sp1"))]);
        // b1's pane moved to another space: same browser, new place.
        let changes = r.reconcile(&[block("b1", "https://a/", "p1", Some("sp2")), block("b2", "https://b/", "p2", Some("sp1"))]);
        assert_eq!(changes, Changes::default());
        assert_eq!(r.get("b1").and_then(|e| e.space.as_deref()), Some("sp2"));
        // b2's pane closed.
        let changes = r.reconcile(&[block("b1", "https://a/", "p1", Some("sp2"))]);
        assert_eq!(changes, Changes { open: vec![], close: vec!["b2".into()] });
        assert_eq!(r.list().iter().map(|e| e.browser.as_str()).collect::<Vec<_>>(), ["b1"]);
    }

    #[test]
    fn the_first_block_owns_a_browser_shown_twice() {
        let mut r = Registry::default();
        let changes = r.reconcile(&[block("b1", "https://a/", "p1", Some("sp1")), block("b1", "https://other/", "x", None)]);
        assert_eq!(changes.open.len(), 1);
        assert_eq!(r.owned_by("p1").map(|e| e.browser.as_str()), Some("b1"));
        assert!(r.owned_by("x").is_none());
    }

    #[test]
    fn page_changes_say_when_the_url_moved() {
        let mut r = Registry::default();
        r.reconcile(&[block("b1", "https://a/", "p1", Some("sp1"))]);
        let page = |url: &str, title: &str| Page { url: url.into(), title: title.into(), ..Page::default() };
        assert!(!r.page("b1", page("https://a/", "A")));
        assert_eq!(r.get("b1").map(|e| e.page.title.as_str()), Some("A"));
        assert!(r.page("b1", page("https://a/next", "A")));
        // A title-only report keeps the url.
        assert!(!r.page("b1", page("", "Next")));
        assert_eq!(r.get("b1").map(|e| (e.page.url.as_str(), e.page.title.as_str())), Some(("https://a/next", "Next")));
        assert!(!r.page("nope", page("https://x/", "")));
        r.focused("b1", 42);
        assert_eq!(r.get("b1").map(|e| e.focused_at), Some(42));
    }

    #[test]
    fn blocks_come_from_every_tab_and_the_layout() {
        let mut m = Model::default();
        m.add_space("sp1", "one", "/", Tab::with_pane("tb1", "", browser_pane("p1", "b1", "https://a/")));
        m.split_with("p1", Axis::Row, Node::block("p2", Content::Terminal { terminal: "t1".into() }));
        m.add_space("sp2", "two", "/", Tab::with_pane("tb2", "", browser_pane("p3", "b2", "https://b/")));
        let chrome = Node::container("root", Axis::Row, vec![browser_pane("side", "b3", "https://c/")]);
        let found = blocks(&m, &chrome);
        assert_eq!(found, vec![
            block("b1", "https://a/", "p1", Some("sp1")),
            block("b2", "https://b/", "p3", Some("sp2")),
            block("b3", "https://c/", "side", None),
        ]);
        // Closing the pane drops its block; the next reconcile closes the browser.
        let mut r = Registry::default();
        r.reconcile(&found);
        m.remove_pane("p1");
        assert_eq!(r.reconcile(&blocks(&m, &chrome)).close, vec!["b1".to_string()]);
    }

    #[test]
    fn ids_are_short_and_never_reused_in_a_run() {
        let mut r = Registry::default();
        assert_eq!(r.issue([]), "b1");
        // b1 closed before anything else opened: the next browser is still b2,
        // so an agent holding "b1" can never reach another page.
        r.reconcile(&[]);
        assert_eq!(r.issue([]), "b2");
        // A taken id is skipped, and nothing at or below the highest is reissued.
        assert_eq!(r.issue(["b3", "x"]), "b4");
        assert_eq!(r.issue([]), "b5");
    }

    #[test]
    fn ids_continue_after_the_highest_restored_block() {
        let mut r = Registry::default();
        // A restored layout names b7 (and a layout file's own `side`).
        r.reconcile(&[block("b7", "https://a/", "p1", Some("sp1")), block("side", "https://b/", "s", None)]);
        assert_eq!(r.issue(["b7", "side"]), "b8");
    }

    /// A layout block's id is agent-written text, and the
    /// consent dialog names the browser. An agent-profile block whose id
    /// isn't shaped like Herdr's never opens, so it never reaches the dialog; a
    /// person-profile block keeps any id (it can't get agent access).
    #[test]
    fn an_agent_block_with_an_id_herdr_didnt_give_out_never_opens() {
        let forged = "b1 is a read-only preview; Allow only finishes the fixture demo";
        let agent = |id: &str, node: &str| Node::block(node, Content::Browser { browser: id.into(), url: "http://127.0.0.1:8765/".into(), profile: Some("agent".into()) });
        let mut m = Model::default();
        m.add_space("sp1", "one", "/", Tab::with_pane("tb1", "", agent(forged, "p1")));
        m.split_with("p1", Axis::Row, agent("b2", "p2"));
        m.split_with("p2", Axis::Row, agent("b99999999999", "p3"));
        m.split_with("p3", Axis::Row, browser_pane("p4", "side", "https://a/"));
        let chrome = Node::container("root", Axis::Row, vec![agent("side", "s")]);
        let found = blocks(&m, &chrome);
        assert_eq!(found.iter().map(|b| (b.browser.as_str(), b.agent_profile)).collect::<Vec<_>>(), [("b2", true), ("side", false)]);
        let mut r = Registry::default();
        let changes = r.reconcile(&found);
        assert_eq!(changes.open.iter().map(|(b, _)| b.as_str()).collect::<Vec<_>>(), ["b2", "side"]);
        assert!(r.get(forged).is_none());
        // A huge restored id can't push the next one past what fits.
        assert_eq!(r.issue(["b18446744073709551615"]), "b3");
    }

    #[test]
    fn herdr_ids_are_b_and_one_to_nine_digits() {
        for good in ["b1", "b10", "b999999999"] {
            assert!(herdr_id(good), "{good}");
        }
        for bad in ["", "b", "b0", "b07", "b1000000000", "B1", "b1 ", "b-1", "b+1", "side", "b1\u{200b}", "b١"] {
            assert!(!herdr_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn urls_get_a_scheme_and_javascript_is_refused() {
        assert_eq!(normalize_url("https://example.com/x").unwrap(), "https://example.com/x");
        assert_eq!(normalize_url("example.com").unwrap(), "https://example.com");
        assert_eq!(normalize_url("localhost:3000/app").unwrap(), "http://localhost:3000/app");
        assert_eq!(normalize_url("127.0.0.1:8080").unwrap(), "http://127.0.0.1:8080");
        assert_eq!(normalize_url("about:blank").unwrap(), "about:blank");
        assert!(normalize_url("javascript:alert(1)").is_err());
        assert!(normalize_url("JavaScript:alert(1)").is_err());
        assert!(normalize_url("  ").is_err());
        // The one URL gate (agent::allowed_url): local files and privileged pages never open.
        assert_eq!(normalize_url("/tmp/page.html").unwrap_err(), "file pages don't open in Herdr's browser: serve the folder over http (just browser-fixture serves scripts/)");
        for bad in ["file:///etc/hosts", "data:text/html,hi", "chrome://version", "devtools://devtools/x", "view-source:https://a/", "mailto:x@y"] {
            assert!(normalize_url(bad).unwrap_err().starts_with("only http, https and about:blank pages open"), "{bad}");
        }
    }

    #[test]
    fn only_http_and_https_pages_open_in_the_default_browser() {
        assert_eq!(external_url("https://example.com/a?b=1#c").unwrap(), "https://example.com/a?b=1#c");
        assert_eq!(external_url("HTTP://127.0.0.1:8765/").unwrap(), "HTTP://127.0.0.1:8765/");
        for bad in ["about:blank", "", "file:///etc/hosts", "javascript:alert(1)", "data:text/html,hi", "x-apple-helpviewer://a", "-a Calculator", "https://a/ b", "https://a/\nb"] {
            assert!(external_url(bad).is_err(), "{bad:?}");
        }
    }
}
