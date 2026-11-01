//! Which browsers the window has open, and what each one shows: pure data,
//! no CEF, so the bookkeeping is testable without a window.
//!
//! The layout is the source of truth at start: a browser block (a pane in a
//! tab, or a block in the shown layout) says a browser with that id should
//! be open with those tabs. `reconcile` compares the blocks with what is open
//! and says which tabs to open and close; from then on the registry holds
//! the tabs and the window writes them back to the block (`Registry::saved`).
//!
//! A browser has an ordered list of tabs and one active. Each tab is its own
//! engine page with an id unique in the app run (`b1:3`, see [`tab_id`]),
//! created lazily: only the active tab opens, the others when first selected.
//! A tab with no url and no page is empty (the "new tab" state).

// Default builds compile these rules only for their tests; the browser feature uses them.
#![cfg_attr(not(all(target_os = "macos", feature = "browser")), allow(dead_code))]

use crate::layout::{Content, Node};
use crate::model::Model;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

/// A browser block somewhere in the window's world.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub browser: String,
    /// The layout's copy of its tabs' urls (`""`: an empty tab) and the shown one:
    /// a browser that isn't open opens like this.
    pub tabs: Vec<String>,
    pub active: usize,
    /// The layout node showing it (its pane id, for a pane).
    pub node: String,
    /// The space and pane, for a browser in a tab; None for a layout block.
    pub space: Option<String>,
    pub pane: Option<String>,
}

/// What the page in an open tab says about itself.
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

/// One tab of a browser.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    /// Unique in the app run (`b1:3`); not saved.
    pub id: String,
    pub page: Page,
    /// The engine has been asked for this tab's page (or a popup reserved it).
    /// A hidden tab that is opened keeps running until it closes.
    pub opened: bool,
}

impl Tab {
    fn new(browser: &str, url: &str) -> Tab {
        Tab { id: tab_id(browser), page: Page { url: url.to_owned(), ..Page::default() }, opened: false }
    }

    /// Nothing to show: no page and no url to open (the "Browse with your agents" state).
    pub fn is_empty(&self) -> bool {
        !self.opened && self.page.url.trim().is_empty()
    }

    fn json(&self) -> Value {
        json!({ "id": self.id, "url": self.page.url, "title": self.page.title, "loading": self.page.loading })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub browser: String,
    /// In order; never empty.
    pub tabs: Vec<Tab>,
    /// The shown tab; always in range.
    pub active: usize,
    /// The node that shows it. Another block with the same id shows a placeholder.
    pub node: String,
    pub space: Option<String>,
    pub pane: Option<String>,
    /// When the page last took the keyboard, ms since the epoch (0: never).
    pub focused_at: u64,
}

impl Entry {
    /// The shown tab.
    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    /// What the shown tab's page says.
    pub fn page(&self) -> &Page {
        &self.tab().page
    }

    /// One browser as mods and `herdr-app ls` see it: the shown tab's page at
    /// the top, then every tab.
    pub fn json(&self, focused: bool) -> Value {
        let page = self.page();
        json!({
            "browser": self.browser, "url": page.url, "title": page.title,
            "loading": page.loading, "can_go_back": page.can_go_back, "can_go_forward": page.can_go_forward,
            "space": self.space, "pane": self.pane, "focused": focused,
            "zoom": super::emulate::shown(page.zoom), "viewport": page.viewport.as_ref().map(super::emulate::Viewport::json),
            "tabs": self.tabs.iter().map(Tab::json).collect::<Vec<_>>(), "active": self.active,
        })
    }

    /// Takes the tab at `index` out. The right neighbour becomes the shown one,
    /// else the left; the last tab leaves one empty tab. Returns the closed tab.
    fn remove_tab(&mut self, index: usize) -> Tab {
        let closed = self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.tabs.push(Tab::new(&self.browser, ""));
            self.active = 0;
        } else if index < self.active {
            self.active -= 1;
        } else if index == self.active {
            self.active = index.min(self.tabs.len() - 1);
        }
        closed
    }

    fn index_of(&self, tab: &str) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == tab)
    }
}

/// What `reconcile` asks of the native side.
#[derive(Debug, Default, PartialEq)]
pub struct Changes {
    /// (tab, url): open these.
    pub open: Vec<(String, String)>,
    /// Close these tabs (closed, or their browser's block is gone).
    pub close: Vec<String>,
}

#[derive(Default)]
pub struct Registry {
    /// In the order they opened.
    entries: Vec<Entry>,
    /// The highest `bN` issued or seen in a block this run: new ids go above
    /// it, so a closed browser's id never names another page (agents hold ids).
    highest: u64,
    /// Opened tabs closed since the last `reconcile`.
    closing: Vec<String>,
}

/// The page a browser with no saved url opens on.
pub const BLANK: &str = "about:blank";

static TAB_NUMBER: AtomicU64 = AtomicU64::new(0);

/// A new tab id for `browser`, `b1:3`: the number counts up for the whole run
/// and across browsers, so a closed tab's id never names another page.
pub fn tab_id(browser: &str) -> String {
    format!("{browser}:{}", TAB_NUMBER.fetch_add(1, Ordering::Relaxed) + 1)
}

/// The browser a tab id belongs to (ids come from [`tab_id`]).
pub fn browser_of(tab: &str) -> &str {
    tab.rsplit_once(':').map_or(tab, |(browser, _)| browser)
}

impl Registry {
    /// Makes the open browsers match the blocks. A block whose browser isn't
    /// open (new, or restored from a saved layout) opens with its saved tabs,
    /// the active one only; an open browser with no block left closes. A moved
    /// block (another tab, another space) only updates where the browser is.
    /// Also opens a shown tab that has a url and no page yet (selected for the
    /// first time, or given a url) and closes the tabs closed meanwhile.
    pub fn reconcile(&mut self, blocks: &[Block]) -> Changes {
        let mut changes = Changes { close: std::mem::take(&mut self.closing), ..Changes::default() };
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
                None => self.entries.push(Entry {
                    browser: b.browser.clone(),
                    tabs: b.tabs.iter().map(|url| Tab::new(&b.browser, url)).collect(),
                    active: b.active,
                    node: b.node.clone(),
                    space: b.space.clone(),
                    pane: b.pane.clone(),
                    focused_at: 0,
                }),
            }
        }
        self.entries.retain(|e| {
            let keep = seen.contains(&e.browser.as_str());
            if !keep {
                changes.close.extend(e.tabs.iter().filter(|t| t.opened).map(|t| t.id.clone()));
            }
            keep
        });
        for e in &mut self.entries {
            let tab = &mut e.tabs[e.active];
            if !tab.opened && !tab.page.url.trim().is_empty() {
                tab.opened = true;
                tab.page.loading = true;
                changes.open.push((tab.id.clone(), tab.page.url.clone()));
            }
        }
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

    fn get_mut(&mut self, browser: &str) -> Result<&mut Entry, String> {
        self.entries.iter_mut().find(|e| e.browser == browser).ok_or_else(|| format!("no open browser {browser:?}: see `herdr-app ls`"))
    }

    pub fn list(&self) -> &[Entry] {
        &self.entries
    }

    /// The browser a node shows, if that node owns it.
    pub fn owned_by(&self, node: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.node == node)
    }

    /// The tab with this id, and the browser it is in.
    pub fn tab(&self, tab: &str) -> Option<(&Entry, &Tab)> {
        let e = self.entries.iter().find(|e| e.index_of(tab).is_some())?;
        e.tabs.iter().find(|t| t.id == tab).map(|t| (e, t))
    }

    /// A tab's page reported a change. True when its url changed (the
    /// block's saved urls follow it).
    pub fn page(&mut self, tab: &str, page: Page) -> bool {
        let Some(t) = self.entries.iter_mut().flat_map(|e| e.tabs.iter_mut()).find(|t| t.id == tab) else { return false };
        let moved = t.page.url != page.url && !page.url.is_empty();
        let url = if page.url.is_empty() { std::mem::take(&mut t.page.url) } else { page.url.clone() };
        t.page = Page { url, ..page };
        moved
    }

    /// The page took the keyboard.
    pub fn focused(&mut self, browser: &str, now: u64) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.browser == browser) {
            e.focused_at = now;
        }
    }

    /// A browser's tabs as its block saves them: urls, and the shown one.
    pub fn saved(&self, browser: &str) -> Option<(Vec<String>, usize)> {
        let e = self.get(browser)?;
        Some((e.tabs.iter().map(|t| t.page.url.clone()).collect(), e.active))
    }

    /// Opens a tab after the shown one and shows it. `url` empty: an empty
    /// tab. Returns the new tab's id; `reconcile` opens its page.
    pub fn new_tab(&mut self, browser: &str, url: &str) -> Result<String, String> {
        let e = self.get_mut(browser)?;
        let tab = Tab::new(browser, url);
        let id = tab.id.clone();
        e.tabs.insert(e.active + 1, tab);
        e.active += 1;
        Ok(id)
    }

    /// Shows the tab at `index` (0 = first).
    pub fn select_tab(&mut self, browser: &str, index: usize) -> Result<(), String> {
        let e = self.get_mut(browser)?;
        if index >= e.tabs.len() {
            return Err(format!("browser {browser} has {} tab(s): index {index} is out of range (0 = first)", e.tabs.len()));
        }
        e.active = index;
        Ok(())
    }

    /// Closes the tab at `index` (default: the shown one): see [`Entry::remove_tab`].
    /// Returns the closed tab's id; `reconcile` closes its page.
    pub fn close_tab(&mut self, browser: &str, index: Option<usize>) -> Result<String, String> {
        let e = self.get_mut(browser)?;
        let index = index.unwrap_or(e.active);
        if index >= e.tabs.len() {
            return Err(format!("browser {browser} has {} tab(s): index {index} is out of range (0 = first)", e.tabs.len()));
        }
        Ok(self.drop_tab(browser, index))
    }

    /// A page closed itself (`window.close()`): its tab goes. None: no such tab.
    pub fn close_tab_by_id(&mut self, tab: &str) -> Option<String> {
        let (e, _) = self.tab(tab)?;
        let (browser, index) = (e.browser.clone(), e.index_of(tab)?);
        Some(self.drop_tab(&browser, index))
    }

    fn drop_tab(&mut self, browser: &str, index: usize) -> String {
        let e = self.entries.iter_mut().find(|e| e.browser == browser).expect("checked by the caller");
        let closed = e.remove_tab(index);
        if closed.opened {
            self.closing.push(closed.id.clone());
        }
        closed.id
    }

    /// Gives an empty tab a page to open (typed into its address bar);
    /// `reconcile` opens it if it is the shown one.
    pub fn set_url(&mut self, tab: &str, url: &str) {
        if let Some(t) = self.entries.iter_mut().flat_map(|e| e.tabs.iter_mut()).find(|t| t.id == tab) {
            t.page.url = url.to_owned();
        }
    }

    /// A page opened a popup: the engine has reserved `tab` for it (it exists
    /// already, so it is `opened`). It sits after its opener and is shown.
    pub fn popup(&mut self, opener: &str, tab: &str, url: &str) {
        let Some((e, _)) = self.tab(opener) else { return };
        let (browser, at) = (e.browser.clone(), e.index_of(opener).map_or(0, |i| i + 1));
        let Ok(e) = self.get_mut(&browser) else { return };
        e.tabs.insert(at, Tab { id: tab.to_owned(), page: Page { url: url.to_owned(), loading: true, ..Page::default() }, opened: true });
        e.active = at;
    }

    /// The engine never created a popup's page: its tab goes and the opener's
    /// tab is shown again.
    pub fn popup_aborted(&mut self, opener: &str, tab: &str) {
        let Some((e, _)) = self.tab(tab) else { return };
        let (browser, index) = (e.browser.clone(), e.index_of(tab).unwrap_or_default());
        let e = self.entries.iter_mut().find(|e| e.browser == browser).expect("found above");
        e.remove_tab(index);
        if let Some(i) = e.index_of(opener) {
            e.active = i;
        }
    }
}

/// Every browser block: the tabs' panes (with their space), then the shown
/// layout's own blocks (on every space, so none). A block may use an id
/// Herdr never gave out (a layout's `side`).
pub fn blocks(model: &Model, chrome: &Node) -> Vec<Block> {
    let mut out = Vec::new();
    for space in &model.spaces {
        for tab in &space.tabs {
            for n in tab.root.walk() {
                if let Some(Content::Browser { browser, tabs, active }) = n.content() {
                    out.push(Block {
                        browser: browser.clone(),
                        tabs: tabs.clone(),
                        active: *active,
                        node: n.id.clone(),
                        space: Some(space.id.clone()),
                        pane: Some(n.id.clone()),
                    });
                }
            }
        }
    }
    for n in chrome.walk() {
        if let Some(Content::Browser { browser, tabs, active }) = n.content() {
            out.push(Block { browser: browser.clone(), tabs: tabs.clone(), active: *active, node: n.id.clone(), space: None, pane: None });
        }
    }
    out
}
/// The number in an id Herdr gives out (`b12` → 12).
fn id_number(id: &str) -> Option<u64> {
    herdr_id(id).then(|| id[1..].parse().ok()).flatten()
}

/// Whether `id` has the shape of an id Herdr gives out: `b1` to
/// `b999999999`; only these move the counter new ids come from.
fn herdr_id(id: &str) -> bool {
    id.strip_prefix('b').is_some_and(|n| (1..=9).contains(&n.len()) && !n.starts_with('0') && n.bytes().all(|c| c.is_ascii_digit()))
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
    use crate::model::Tab as ModelTab;

    fn browser_pane(id: &str, browser: &str, url: &str) -> Node {
        Node::block(id, Content::browser(browser, url))
    }

    fn block(browser: &str, url: &str, node: &str, space: Option<&str>) -> Block {
        blocks_of(browser, &[url], 0, node, space)
    }

    fn blocks_of(browser: &str, urls: &[&str], active: usize, node: &str, space: Option<&str>) -> Block {
        Block { browser: browser.into(), tabs: urls.iter().map(|u| (*u).into()).collect(), active, node: node.into(), space: space.map(Into::into), pane: space.map(|_| node.into()) }
    }

    fn urls(r: &Registry, browser: &str) -> Vec<String> {
        r.get(browser).unwrap().tabs.iter().map(|t| t.page.url.clone()).collect()
    }

    /// A browser with tabs at these urls, the first shown, all opened.
    fn browser_with(urls: &[&str]) -> Registry {
        let mut r = Registry::default();
        r.reconcile(&[blocks_of("b1", urls, 0, "p1", Some("sp1"))]);
        for i in 0..urls.len() {
            r.select_tab("b1", i).unwrap();
            r.reconcile(&[blocks_of("b1", urls, 0, "p1", Some("sp1"))]);
        }
        r.select_tab("b1", 0).unwrap();
        r
    }

    #[test]
    fn ls_shows_a_pages_url_and_title_as_they_are() {
        let mut r = Registry::default();
        r.reconcile(&[block("b2", "http://127.0.0.1:18766/", "p2", Some("sp1"))]);
        let tab = r.get("b2").unwrap().tab().id.clone();
        let url = "http://me:pw@127.0.0.1:18766/notitle.html?code=1#frag";
        r.page(&tab, Page { url: url.into(), title: "127.0.0.1:18766/notitle.html?code=1".into(), ..Page::default() });
        let e = r.get("b2").unwrap().json(false);
        assert_eq!((e["url"].as_str(), e["title"].as_str()), (Some(url), Some("127.0.0.1:18766/notitle.html?code=1")));
    }

    #[test]
    fn ls_shows_a_pages_zoom_and_viewport() {
        let mut r = Registry::default();
        r.reconcile(&[block("b1", "https://a/", "p1", Some("sp1"))]);
        let tab = r.get("b1").unwrap().tab().id.clone();
        let e = r.get("b1").unwrap().json(false);
        assert_eq!((e["zoom"].clone(), e["viewport"].clone()), (serde_json::json!(100), Value::Null));
        let phone = super::super::emulate::viewport(&serde_json::json!({ "preset": "phone" })).unwrap();
        r.page(&tab, Page { url: "https://a/".into(), zoom: 200.0 / 3.0, viewport: phone, ..Page::default() });
        let e = r.get("b1").unwrap().json(false);
        assert_eq!(e["zoom"], 67);
        assert_eq!(e["viewport"], serde_json::json!({ "width": 390, "height": 844, "mobile": true, "preset": "phone" }));
    }

    #[test]
    fn ls_lists_every_tab_and_the_top_level_fields_describe_the_shown_one() {
        let mut r = Registry::default();
        r.reconcile(&[blocks_of("b1", &["https://a/", "https://b/"], 1, "p1", Some("sp1"))]);
        let (a, b) = (r.get("b1").unwrap().tabs[0].id.clone(), r.get("b1").unwrap().tabs[1].id.clone());
        r.page(&b, Page { url: "https://b/".into(), title: "B".into(), loading: false, ..Page::default() });
        let e = r.get("b1").unwrap().json(false);
        assert_eq!((e["url"].as_str(), e["title"].as_str(), e["active"].as_u64()), (Some("https://b/"), Some("B"), Some(1)));
        assert_eq!(e["tabs"], serde_json::json!([
            { "id": a, "url": "https://a/", "title": "", "loading": false },
            { "id": b, "url": "https://b/", "title": "B", "loading": false },
        ]));
        assert!(a != b && a.starts_with("b1:") && b.starts_with("b1:"));
        assert_eq!(browser_of(&a), "b1");
    }

    #[test]
    fn a_block_without_an_open_browser_opens_its_shown_tab_at_its_saved_url() {
        let mut r = Registry::default();
        // A restored layout: the browser isn't open yet.
        let changes = r.reconcile(&[block("b1", "https://example.com/a", "p1", Some("sp1"))]);
        let tab = r.get("b1").unwrap().tab().id.clone();
        assert_eq!(changes, Changes { open: vec![(tab, "https://example.com/a".into())], close: vec![] });
        assert_eq!(r.get("b1").map(|e| e.page().url.as_str()), Some("https://example.com/a"));
        // Nothing changed: nothing to do.
        assert_eq!(r.reconcile(&[block("b1", "https://example.com/a", "p1", Some("sp1"))]), Changes::default());
    }

    #[test]
    fn a_tab_saved_without_a_url_is_empty_and_opens_no_page() {
        let mut r = Registry::default();
        let changes = r.reconcile(&[block("b1", "", "p1", Some("sp1")), block("b2", " ", "p2", Some("sp1"))]);
        assert_eq!(changes, Changes::default());
        assert!(r.get("b1").unwrap().tab().is_empty() && r.get("b2").unwrap().tab().is_empty());
    }

    #[test]
    fn a_restored_browser_creates_only_its_shown_tab() {
        let mut r = Registry::default();
        let b = blocks_of("b1", &["https://a/", "https://b/", "https://c/"], 1, "p1", Some("sp1"));
        let changes = r.reconcile(&[b.clone()]);
        let e = r.get("b1").unwrap();
        assert_eq!(changes.open, vec![(e.tabs[1].id.clone(), "https://b/".to_owned())]);
        assert_eq!(e.tabs.iter().map(|t| t.opened).collect::<Vec<_>>(), [false, true, false]);
        // A tab is created the first time it is shown, and not again.
        r.select_tab("b1", 2).unwrap();
        let changes = r.reconcile(&[b.clone()]);
        let c = r.get("b1").unwrap().tabs[2].id.clone();
        assert_eq!(changes.open, vec![(c, "https://c/".to_owned())]);
        r.select_tab("b1", 1).unwrap();
        r.select_tab("b1", 2).unwrap();
        assert_eq!(r.reconcile(&[b]), Changes::default());
    }

    #[test]
    fn a_gone_block_closes_its_opened_tabs_and_a_moved_one_keeps_them() {
        let mut r = Registry::default();
        r.reconcile(&[block("b1", "https://a/", "p1", Some("sp1")), blocks_of("b2", &["https://b/", "https://c/"], 0, "p2", Some("sp1"))]);
        let b2_open = r.get("b2").unwrap().tabs[0].id.clone();
        // b1's pane moved to another space: same browser, new place.
        let changes = r.reconcile(&[block("b1", "https://a/", "p1", Some("sp2")), blocks_of("b2", &["https://b/", "https://c/"], 0, "p2", Some("sp1"))]);
        assert_eq!(changes, Changes::default());
        assert_eq!(r.get("b1").and_then(|e| e.space.as_deref()), Some("sp2"));
        // b2's pane closed: only the tab that has a page needs closing.
        let changes = r.reconcile(&[block("b1", "https://a/", "p1", Some("sp2"))]);
        assert_eq!(changes, Changes { open: vec![], close: vec![b2_open] });
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
        let tab = r.get("b1").unwrap().tab().id.clone();
        let page = |url: &str, title: &str| Page { url: url.into(), title: title.into(), ..Page::default() };
        assert!(!r.page(&tab, page("https://a/", "A")));
        assert_eq!(r.get("b1").map(|e| e.page().title.as_str()), Some("A"));
        assert!(r.page(&tab, page("https://a/next", "A")));
        // A title-only report keeps the url.
        assert!(!r.page(&tab, page("", "Next")));
        assert_eq!(r.get("b1").map(|e| (e.page().url.as_str(), e.page().title.as_str())), Some(("https://a/next", "Next")));
        assert!(!r.page("nope", page("https://x/", "")));
        r.focused("b1", 42);
        assert_eq!(r.get("b1").map(|e| e.focused_at), Some(42));
    }

    #[test]
    fn a_new_tab_opens_after_the_shown_one_and_is_shown() {
        let mut r = browser_with(&["https://a/", "https://b/"]);
        let id = r.new_tab("b1", "").unwrap();
        assert_eq!(urls(&r, "b1"), ["https://a/", "", "https://b/"]);
        assert_eq!(r.get("b1").unwrap().active, 1);
        assert_eq!(r.get("b1").unwrap().tab().id, id);
        // Empty: no page to open.
        assert!(r.get("b1").unwrap().tab().is_empty());
        assert_eq!(r.reconcile(&[block("b1", "", "p1", Some("sp1"))]), Changes::default());
        // With a url: its page opens at the next reconcile.
        let id = r.new_tab("b1", "https://c/").unwrap();
        assert_eq!(r.reconcile(&[block("b1", "", "p1", Some("sp1"))]).open, vec![(id, "https://c/".to_owned())]);
        assert!(r.new_tab("nope", "").is_err());
    }

    #[test]
    fn typing_a_url_into_an_empty_tab_opens_its_page() {
        let mut r = browser_with(&["https://a/"]);
        let id = r.new_tab("b1", "").unwrap();
        r.set_url(&id, "https://typed/");
        let changes = r.reconcile(&[block("b1", "", "p1", Some("sp1"))]);
        assert_eq!(changes.open, vec![(id.clone(), "https://typed/".to_owned())]);
        assert!(!r.get("b1").unwrap().tab().is_empty());
    }

    #[test]
    fn selecting_a_tab_out_of_range_is_refused() {
        let mut r = browser_with(&["https://a/", "https://b/"]);
        assert!(r.select_tab("b1", 2).unwrap_err().contains("2 tab(s)"));
        r.select_tab("b1", 1).unwrap();
        assert_eq!(r.get("b1").unwrap().active, 1);
    }

    #[test]
    fn closing_the_shown_tab_shows_its_right_neighbour_else_its_left() {
        let mut r = browser_with(&["https://a/", "https://b/", "https://c/"]);
        r.select_tab("b1", 1).unwrap();
        r.close_tab("b1", None).unwrap();
        assert_eq!((urls(&r, "b1"), r.get("b1").unwrap().tab().page.url.clone()), (vec!["https://a/".into(), "https://c/".into()], "https://c/".to_owned()));
        // The last one has no right neighbour: its left one.
        r.close_tab("b1", None).unwrap();
        assert_eq!((urls(&r, "b1"), r.get("b1").unwrap().active), (vec!["https://a/".into()], 0));
    }

    #[test]
    fn closing_another_tab_keeps_the_shown_one() {
        let mut r = browser_with(&["https://a/", "https://b/", "https://c/"]);
        r.select_tab("b1", 2).unwrap();
        // Left of the shown tab: the shown tab moves one place left, still the same page.
        r.close_tab("b1", Some(0)).unwrap();
        assert_eq!((r.get("b1").unwrap().active, r.get("b1").unwrap().tab().page.url.clone()), (1, "https://c/".to_owned()));
        // Right of it.
        r.select_tab("b1", 0).unwrap();
        r.close_tab("b1", Some(1)).unwrap();
        assert_eq!((urls(&r, "b1"), r.get("b1").unwrap().active), (vec!["https://b/".into()], 0));
        assert!(r.close_tab("b1", Some(5)).is_err());
    }

    #[test]
    fn closing_the_last_tab_leaves_one_empty_tab() {
        let mut r = browser_with(&["https://a/"]);
        let old = r.get("b1").unwrap().tab().id.clone();
        r.close_tab("b1", None).unwrap();
        let e = r.get("b1").unwrap();
        assert_eq!((e.tabs.len(), e.active), (1, 0));
        assert!(e.tab().is_empty());
        assert_ne!(e.tab().id, old);
        // The closed page closes at the next reconcile.
        assert_eq!(r.reconcile(&[block("b1", "", "p1", Some("sp1"))]), Changes { open: vec![], close: vec![old] });
    }

    #[test]
    fn only_the_closed_tabs_page_closes() {
        let mut r = browser_with(&["https://a/", "https://b/"]);
        let (a, b) = (r.get("b1").unwrap().tabs[0].id.clone(), r.get("b1").unwrap().tabs[1].id.clone());
        r.close_tab("b1", Some(1)).unwrap();
        let changes = r.reconcile(&[blocks_of("b1", &["https://a/"], 0, "p1", Some("sp1"))]);
        assert_eq!(changes.close, vec![b]);
        assert!(r.tab(&a).is_some());
        // Closing a tab that never had a page asks the engine for nothing.
        let mut r = Registry::default();
        r.reconcile(&[blocks_of("b1", &["https://a/", "https://b/"], 0, "p1", Some("sp1"))]);
        r.close_tab("b1", Some(1)).unwrap();
        assert_eq!(r.reconcile(&[block("b1", "https://a/", "p1", Some("sp1"))]).close, Vec::<String>::new());
    }

    #[test]
    fn the_block_saves_every_tabs_url_and_the_shown_one() {
        let mut r = browser_with(&["https://a/", "https://b/"]);
        r.new_tab("b1", "").unwrap();
        assert_eq!(r.saved("b1"), Some((vec!["https://a/".into(), String::new(), "https://b/".into()], 1)));
        assert_eq!(r.saved("nope"), None);
    }

    #[test]
    fn a_popup_is_a_shown_tab_after_its_opener_that_already_has_a_page() {
        let mut r = browser_with(&["https://a/", "https://b/"]);
        let opener = r.get("b1").unwrap().tabs[0].id.clone();
        r.popup(&opener, "b1:900", "https://popup/");
        let e = r.get("b1").unwrap();
        assert_eq!((e.tabs.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), e.active), ([opener.as_str(), "b1:900", e.tabs[2].id.as_str()].to_vec(), 1));
        assert!(e.tab().opened && !e.tab().is_empty());
        // The engine made the page itself: reconcile has nothing to open.
        assert_eq!(r.reconcile(&[block("b1", "", "p1", Some("sp1"))]), Changes::default());
        // An empty popup (window.open() starts blank) is not the empty state.
        r.popup("b1:900", "b1:901", "");
        assert!(!r.get("b1").unwrap().tab().is_empty());
        // A popup of a gone tab is dropped.
        r.popup("b1:nope", "b1:902", "https://x/");
        assert!(r.tab("b1:902").is_none());
    }

    #[test]
    fn an_aborted_popup_leaves_no_tab_and_shows_the_opener_again() {
        let mut r = browser_with(&["https://a/"]);
        let opener = r.get("b1").unwrap().tab().id.clone();
        r.popup(&opener, "b1:900", "https://popup/");
        r.popup_aborted(&opener, "b1:900");
        let e = r.get("b1").unwrap();
        assert_eq!((e.tabs.len(), e.tab().id.as_str()), (1, opener.as_str()));
    }

    #[test]
    fn a_page_closing_itself_closes_its_tab_not_the_browser() {
        let mut r = browser_with(&["https://a/", "https://b/"]);
        let (a, b) = (r.get("b1").unwrap().tabs[0].id.clone(), r.get("b1").unwrap().tabs[1].id.clone());
        assert_eq!(r.close_tab_by_id(&a), Some(a.clone()));
        assert_eq!(urls(&r, "b1"), ["https://b/"]);
        assert_eq!(r.close_tab_by_id(&a), None);
        assert_eq!(r.reconcile(&[block("b1", "https://b/", "p1", Some("sp1"))]).close, vec![a]);
        assert!(r.tab(&b).is_some());
    }

    #[test]
    fn blocks_come_from_every_tab_and_the_layout() {
        let mut m = Model::default();
        m.add_space("sp1", "one", "/", ModelTab::with_pane("tb1", "", browser_pane("p1", "b1", "https://a/")));
        m.split_with("p1", Axis::Row, Node::block("p2", Content::Terminal { terminal: "t1".into() }));
        m.add_space("sp2", "two", "/", ModelTab::with_pane("tb2", "", browser_pane("p3", "b2", "https://b/")));
        let chrome = Node::container("root", Axis::Row, vec![browser_pane("side", "b3", "https://c/")]);
        let found = blocks(&m, &chrome);
        assert_eq!(found, vec![
            block("b1", "https://a/", "p1", Some("sp1")),
            block("b2", "https://b/", "p3", Some("sp2")),
            block("b3", "https://c/", "side", None),
        ]);
        // Closing the pane drops its block; the next reconcile closes the browser's tabs.
        let mut r = Registry::default();
        let first = r.reconcile(&found);
        m.remove_pane("p1");
        assert_eq!(r.reconcile(&blocks(&m, &chrome)).close, vec![first.open[0].0.clone()]);
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
    fn tab_ids_are_never_reused_and_name_their_browser() {
        let (a, b) = (tab_id("b1"), tab_id("b1"));
        assert_ne!(a, b);
        assert_eq!((browser_of(&a), browser_of(&b)), ("b1", "b1"));
        // A layout may name a browser anything, a colon included.
        assert_eq!(browser_of(&tab_id("side:left")), "side:left");
    }

    #[test]
    fn ids_continue_after_the_highest_restored_block() {
        let mut r = Registry::default();
        // A restored layout names b7 (and a layout file's own `side`).
        r.reconcile(&[block("b7", "https://a/", "p1", Some("sp1")), block("side", "https://b/", "s", None)]);
        assert_eq!(r.issue(["b7", "side"]), "b8");
    }
    #[test]
    fn a_huge_restored_id_cannot_push_the_next_one_past_what_fits() {
        let mut r = Registry::default();
        assert_eq!(r.issue(["b18446744073709551615", "b999999999"]), "b1000000000");
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
