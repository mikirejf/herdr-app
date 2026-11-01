//! Builds without browsers (not macOS, or no `browser` feature): blocks show
//! a placeholder, commands say why, nothing opens.

use gpui::{AnyElement, App, IntoElement, ParentElement, Styled, Window, div};
use serde_json::Value;

use crate::layout::Node;
use crate::model::Model;

/// Why there's no browser, for commands that need one.
const UNAVAILABLE: &str =
    "browser panes need a macOS build of Herdr with the `browser` feature (just browser-dev); this build shows them as placeholders";

/// Never sent in this build.
pub enum Event {}

#[derive(Default)]
pub struct Browsers;

impl Browsers {
    pub fn new() -> Self {
        Browsers
    }

    pub fn events(&mut self) -> Option<async_channel::Receiver<Event>> {
        None
    }

    pub fn apply(&mut self, event: Event) -> super::Update {
        match event {}
    }

    pub fn prepare_open(&mut self,_url: &str, _model: &Model, _chrome: &Node) -> Result<(String, String), String> {
        Err(UNAVAILABLE.into())
    }

    pub fn reconcile(&mut self, _model: &Model, _chrome: &Node, _window: &mut Window, _cx: &mut App) {}

    pub fn command(&mut self, _name: &str, _browser: &str, _args: &Value) -> Result<Value, String> {
        Err(UNAVAILABLE.into())
    }

    pub fn info(&self, _browser: &str, _focused: bool) -> Option<Value> {
        None
    }

    pub fn list(&self, _focused_pane: Option<&str>) -> Vec<Value> {
        Vec::new()
    }

    pub fn agent_state(&self, _browser: &str) -> Option<(bool, Option<String>)> {
        None
    }

    pub fn agents_on(&self) -> Vec<(String, Option<String>)> {
        Vec::new()
    }

    pub fn set_agent_access(&mut self, _browser: &str, _on: bool, _from: super::Source) -> Result<(), String> {
        Err(UNAVAILABLE.into())
    }

    pub fn attach(&mut self, _terminal: &str, _space: &str, _tab: &str, _wanted: Option<&str>, _check: bool, _tab_of: &dyn Fn(&str) -> Option<String>) -> Result<serde_json::Value, String> {
        Err(UNAVAILABLE.into())
    }

    pub fn keys(&mut self, _page: Option<&str>, _window: &mut Window) {}

    pub fn covered(&self) -> Vec<String> {
        Vec::new()
    }

    /// No pages, so overlays report nothing.
    pub fn cover(&self, _label: String) -> Option<AnyElement> {
        None
    }

    /// No agent browsers, so no dialog.
    pub fn ask(&mut self, _browser: &str) -> Result<super::Ask, String> {
        Err(UNAVAILABLE.into())
    }

    /// Never asked: nothing turns access on.
    pub fn answer(&mut self, _ask: &super::Ask, _reply: super::Reply) -> Result<bool, String> {
        Err(UNAVAILABLE.into())
    }

    pub fn element(&self, _node: &str, browser: &str, url: &str, look: super::Look) -> AnyElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .p_4()
            .bg(look.surface)
            .text_color(look.dim)
            .child(div().text_color(look.text).child(format!("Browser {browser}: {url}")))
            .child(UNAVAILABLE)
            .into_any_element()
    }

    pub fn frame_end(&self) -> Option<AnyElement> {
        None
    }
}
