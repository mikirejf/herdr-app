//! The CDP subset agent-browser sends, translated for a WKWebView: what each
//! allowed method does there (`plan`), and the CDP shapes of what comes
//! back (frames, events, the accessibility tree). Pure: no WebKit here, so
//! every mapping is unit-tested; webkit.rs runs the steps.
//!
//! The proxy (agent.rs `Link`) already checked the token, the allowlist and
//! the URL gate before a message gets here. Methods missing from `plan` get
//! CDP's "wasn't found" error: on WebKit, nothing else exists.

// Default builds compile this only for its tests; the browser-webkit feature uses it.
#![cfg_attr(not(all(target_os = "macos", feature = "browser-webkit")), allow(dead_code))]

use serde_json::{Map, Value, json};

/// The page's one frame (WebKit reports no frame ids to its host).
pub const FRAME: &str = "F1";
/// The page world's one execution context.
pub const CONTEXT: i64 = 1;
/// The largest console message relayed to an agent, in bytes of JSON.
pub const MAX_CONSOLE: usize = 64 << 10;
/// The most device pixels one screenshot may have.
pub const MAX_SHOT_PIXELS: f64 = 32.0 * 1024.0 * 1024.0;

/// What one allowed CDP method does on a WKWebView.
#[derive(Debug, PartialEq)]
pub enum Step {
    /// Answer at once.
    Reply(Value),
    /// Answer with an error.
    Fail(String),
    /// Call `__cdp[op](params)` in the isolated world (webkit.js) and answer with what it returns.
    World(&'static str),
    /// `Runtime.evaluate` with `returnByValue`: in the page's own world, so
    /// page globals are visible as in CDP's default context (`spliced`).
    PageEval,
    /// `Accessibility.getFullAXTree`: the isolated world's raw tree, shaped by `ax_tree`.
    AxTree,
    /// Load this url; answered once it finished loading (so `open` reports the new title).
    Navigate(String),
    /// Reload; answered at once (agent-browser waits for the load event itself).
    Reload,
    Mouse(Mouse),
    Key(Key),
    InsertText(String),
    Screenshot(Shot),
    /// The page lays out at this size (None: it fills its pane again). Size only:
    /// WebKit has no public pixel-ratio, mobile or touch emulation.
    Viewport(Option<(u32, u32)>),
    /// None: WebKit's own user agent again.
    UserAgent(Option<String>),
}

/// What `plan` needs to know about the page.
pub struct Ctx<'a> {
    pub url: &'a str,
    /// Counts committed navigations: CDP's `loaderId`.
    pub loader: u64,
    pub user_agent: &'a str,
    /// A mouse button is down (a move is then a drag).
    pub buttons_down: bool,
}

/// What one allowed CDP method does.
pub fn plan(method: &str, params: &Value, ctx: &Ctx) -> Step {
    let s = |k: &str| params.get(k).and_then(Value::as_str);
    let n = |k: &str| params.get(k).and_then(Value::as_f64);
    match method {
        "Browser.getVersion" => Step::Reply(json!({
            "protocolVersion": "1.3", "product": "WebKit/Herdr", "revision": "", "userAgent": ctx.user_agent, "jsVersion": "JavaScriptCore"
        })),
        "Page.enable" | "Page.disable" | "Runtime.enable" | "Runtime.disable" | "Network.enable" | "Network.disable" | "DOM.enable"
        | "DOM.disable" | "Accessibility.enable" | "Accessibility.disable" | "Log.enable" | "Log.disable" | "Inspector.enable"
        | "Runtime.runIfWaitingForDebugger" | "Page.bringToFront" | "Page.setLifecycleEventsEnabled" | "Page.stopLoading"
        | "Emulation.setFocusEmulationEnabled" => Step::Reply(json!({})),
        "Page.getFrameTree" => Step::Reply(json!({ "frameTree": { "frame": frame(ctx.url, ctx.loader), "childFrames": [] } })),
        "Page.navigate" => match s("url") {
            Some(url) if !url.is_empty() => Step::Navigate(url.to_owned()),
            _ => Step::Fail("Cannot navigate to invalid URL".into()),
        },
        "Page.reload" => Step::Reload,
        "Page.captureScreenshot" => Step::Screenshot(Shot::of(params)),
        "Page.getLayoutMetrics" => Step::World("layoutMetrics"),
        "Page.handleJavaScriptDialog" => Step::Fail("No dialog is showing".into()),
        "Runtime.evaluate" if params.get("returnByValue").and_then(Value::as_bool) == Some(true) => Step::PageEval,
        "Runtime.evaluate" => Step::World("evaluate"),
        "Runtime.callFunctionOn" => Step::World("callFunctionOn"),
        "Runtime.releaseObject" => Step::World("releaseObject"),
        "Runtime.releaseObjectGroup" => Step::World("releaseObjectGroup"),
        "Accessibility.getFullAXTree" => Step::AxTree,
        "DOM.resolveNode" => Step::World("resolveNode"),
        "DOM.getBoxModel" => Step::World("getBoxModel"),
        "DOM.scrollIntoViewIfNeeded" => Step::World("scrollIntoViewIfNeeded"),
        "DOM.getDocument" => Step::World("getDocument"),
        "DOM.querySelector" => Step::World("querySelector"),
        "DOM.querySelectorAll" => Step::World("querySelectorAll"),
        "DOM.describeNode" => Step::World("describeNode"),
        "DOM.focus" => Step::World("focus"),
        "Input.dispatchMouseEvent" => match mouse(params, ctx.buttons_down) {
            Some(m) if m.kind == MouseKind::Wheel => Step::World("wheel"),
            Some(m) => Step::Mouse(m),
            None => Step::Fail(format!("Invalid mouse event type {}", s("type").unwrap_or_default())),
        },
        "Input.dispatchKeyEvent" => match key(params) {
            Some(k) => Step::Key(k),
            // `char` events: the keyDown already typed the text.
            None => Step::Reply(json!({})),
        },
        "Input.insertText" => Step::InsertText(s("text").unwrap_or_default().to_owned()),
        "Emulation.setDeviceMetricsOverride" => match (n("width"), n("height")) {
            (Some(w), Some(h)) if w >= 1.0 && h >= 1.0 => Step::Viewport(Some((w.min(10_000.0) as u32, h.min(10_000.0) as u32))),
            _ => Step::Viewport(None),
        },
        "Emulation.clearDeviceMetricsOverride" => Step::Viewport(None),
        "Emulation.setUserAgentOverride" | "Network.setUserAgentOverride" => Step::UserAgent(s("userAgent").filter(|u| !u.is_empty()).map(str::to_owned)),
        other => Step::Fail(format!("'{other}' wasn't found")),
    }
}

/// CDP's answer to one request (`id`: the proxy's), or its error.
pub fn answer(id: i64, result: Result<Value, String>) -> Value {
    match result {
        Ok(r) => json!({ "id": id, "result": r }),
        Err(e) => json!({ "id": id, "error": { "code": -32000, "message": e } }),
    }
}

/// The name an agent's spliced code's outcome goes by (`spliced`). Every
/// name the WebKit call declares starts `__herdr_`, so the agent's code,
/// which sees them, doesn't take one for a page global.
pub const GOT: &str = "__herdr_got";

/// Script that runs an agent's `Runtime.evaluate` expression (or
/// `callFunctionOn` function) as part of a `callAsyncJavaScript` body.
/// WebKit compiles that body itself, so a page's CSP doesn't stop it: no
/// string eval, which a CSP without `unsafe-eval` refuses. It leaves
/// `{ ok: value }` or `{ err: thrown }` in `GOT`, with `this` the global
/// object as at a script's top level. A statement list is no expression:
/// the body then doesn't compile (`is_syntax_error`), nothing ran, and the
/// caller runs the code as a script instead. The newlines keep a trailing
/// `//` comment from eating the rest.
pub fn spliced(code: &str) -> String {
    format!("const {GOT} = await (async function () {{ try {{ return {{ ok: (\n{code}\n) }}; }} catch (e) {{ return {{ err: e }}; }} }}).call(globalThis);\n")
}

/// WebKit's message for a call whose script didn't compile.
pub fn is_syntax_error(message: &str) -> bool {
    message.starts_with("SyntaxError")
}

/// `Runtime.evaluate`'s answer for code WebKit ran as a script: its
/// completion value, by value (None: undefined).
pub fn script_result(value: Option<Value>) -> Value {
    let remote = match value {
        None => json!({ "type": "undefined" }),
        Some(Value::Null) => json!({ "type": "object", "subtype": "null", "value": null }),
        Some(Value::String(s)) => json!({ "type": "string", "value": s }),
        Some(Value::Bool(b)) => json!({ "type": "boolean", "value": b }),
        Some(Value::Number(n)) => json!({ "type": "number", "value": n, "description": n.to_string() }),
        Some(v) => json!({ "type": "object", "value": v }),
    };
    json!({ "result": remote })
}

/// `Runtime.evaluate`'s answer for a script whose value WebKit can't pass
/// back (a node, a function, a promise, a date).
pub fn script_opaque() -> Value {
    json!({ "result": { "type": "object", "description": "a value WebKit can't return by value" } })
}

/// `Runtime.evaluate`'s answer when the code threw (or didn't compile).
pub fn thrown(message: &str) -> Value {
    let error = json!({ "type": "object", "subtype": "error", "description": message });
    json!({ "result": error, "exceptionDetails": { "exceptionId": 1, "text": "Uncaught", "lineNumber": 0, "columnNumber": 0, "exception": error } })
}

/// `Page.navigate`'s answer once the load ended: an error text if it failed.
pub fn navigated(loader: u64, error: Option<&str>) -> Value {
    let mut r = json!({ "frameId": FRAME, "loaderId": format!("L{loader}") });
    if let Some(e) = error {
        r["errorText"] = json!(e);
    }
    r
}

// ---- input --------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mods {
    pub alt: bool,
    pub ctrl: bool,
    pub cmd: bool,
    pub shift: bool,
}

/// CDP's modifier bits: 1 Alt, 2 Control, 4 Meta (Command), 8 Shift.
pub fn mods(bits: i64) -> Mods {
    Mods { alt: bits & 1 != 0, ctrl: bits & 2 != 0, cmd: bits & 4 != 0, shift: bits & 8 != 0 }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MouseKind {
    Down,
    Up,
    Move,
    /// A move with a button down.
    Drag,
    Wheel,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Button {
    Left,
    Right,
    Other,
}

/// One `Input.dispatchMouseEvent`, at (x, y) in the page's CSS pixels from its top left.
#[derive(Clone, Debug, PartialEq)]
pub struct Mouse {
    pub kind: MouseKind,
    pub x: f64,
    pub y: f64,
    pub button: Button,
    pub clicks: isize,
    pub mods: Mods,
    pub dx: f64,
    pub dy: f64,
}

pub fn mouse(p: &Value, buttons_down: bool) -> Option<Mouse> {
    let n = |k: &str| p.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(0.0);
    let kind = match p.get("type").and_then(Value::as_str)? {
        "mousePressed" => MouseKind::Down,
        "mouseReleased" => MouseKind::Up,
        "mouseMoved" if buttons_down => MouseKind::Drag,
        "mouseMoved" => MouseKind::Move,
        "mouseWheel" => MouseKind::Wheel,
        _ => return None,
    };
    let button = match p.get("button").and_then(Value::as_str) {
        Some("right") => Button::Right,
        Some("middle" | "back" | "forward") => Button::Other,
        _ => Button::Left,
    };
    let clicks = p.get("clickCount").and_then(Value::as_i64).unwrap_or(1).clamp(0, 3) as isize;
    Some(Mouse { kind, x: n("x"), y: n("y"), button, clicks, mods: mods(p.get("modifiers").and_then(Value::as_i64).unwrap_or(0)), dx: n("deltaX"), dy: n("deltaY") })
}

/// One `Input.dispatchKeyEvent` as a macOS key event.
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub up: bool,
    /// The macOS virtual key code (0 when unknown: the text still types).
    pub code: u16,
    /// `characters`.
    pub chars: String,
    /// `charactersIgnoringModifiers`.
    pub plain: String,
    pub mods: Mods,
}

/// Named keys: CDP `key` → (macOS key code, the characters AppKit reports).
const NAMED: [(&str, u16, &str); 16] = [
    ("Enter", 36, "\r"),
    ("Tab", 48, "\t"),
    ("Backspace", 51, "\u{7f}"),
    ("Escape", 53, "\u{1b}"),
    ("Delete", 117, "\u{f728}"),
    (" ", 49, " "),
    ("ArrowLeft", 123, "\u{f702}"),
    ("ArrowRight", 124, "\u{f703}"),
    ("ArrowDown", 125, "\u{f701}"),
    ("ArrowUp", 126, "\u{f700}"),
    ("Home", 115, "\u{f729}"),
    ("End", 119, "\u{f72b}"),
    ("PageUp", 116, "\u{f72c}"),
    ("PageDown", 121, "\u{f72d}"),
    ("Space", 49, " "),
    ("Return", 36, "\r"),
];

/// Letters and digits: ANSI key codes (kVK_ANSI_*).
fn letter_code(c: char) -> Option<u16> {
    const KEYS: &str = "asdfhgzxcv_bqweryt123465=97-80]ou[ip_lj'k;\\,/nm.";
    let c = c.to_ascii_lowercase();
    if c == '_' {
        return None;
    }
    KEYS.chars().position(|k| k == c).map(|i| i as u16)
}

/// None: a `char` event (the keyDown before it typed the text), or no type.
pub fn key(p: &Value) -> Option<Key> {
    let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default();
    let up = match s("type") {
        "keyUp" => true,
        "keyDown" | "rawKeyDown" => false,
        _ => return None,
    };
    let name = s("key");
    let mut chars = s("text").to_owned();
    let mut code = 0;
    if let Some((_, c, text)) = NAMED.iter().find(|(n, ..)| *n == name) {
        code = *c;
        // Enter's text is "\r" in CDP too; AppKit's characters are what moves the caret.
        if chars.is_empty() || name == "Enter" || name == "Return" {
            chars = (*text).to_owned();
        }
    } else if name.chars().count() == 1 {
        let c = name.chars().next().unwrap_or_default();
        code = letter_code(c).unwrap_or(0);
        if chars.is_empty() && !up {
            chars = name.to_owned();
        }
    }
    if up && chars.is_empty() {
        chars = name.chars().take(1).collect();
    }
    let plain = chars.to_lowercase();
    Some(Key { up, code, chars, plain, mods: mods(p.get("modifiers").and_then(Value::as_i64).unwrap_or(0)) })
}

// ---- screenshots ------------------------------------------------------------------

/// `Page.captureScreenshot`: a clip in CSS pixels (None: the visible page),
/// and whether it may reach past the visible page (`captureBeyondViewport`).
#[derive(Clone, Debug, PartialEq)]
pub struct Shot {
    pub clip: Option<(f64, f64, f64, f64)>,
    pub beyond: bool,
    pub jpeg: bool,
}

impl Shot {
    fn of(p: &Value) -> Shot {
        let clip = p.get("clip").and_then(|c| {
            let n = |k: &str| c.get(k).and_then(Value::as_f64).filter(|v| v.is_finite());
            let (w, h) = (n("width")?, n("height")?);
            (w > 0.0 && h > 0.0).then(|| (n("x").unwrap_or(0.0).max(0.0), n("y").unwrap_or(0.0).max(0.0), w.min(20_000.0), h.min(20_000.0)))
        });
        Shot {
            clip,
            beyond: p.get("captureBeyondViewport").and_then(Value::as_bool).unwrap_or(false),
            jpeg: p.get("format").and_then(Value::as_str) == Some("jpeg"),
        }
    }

    /// Whether the snapshot, in device pixels at `zoom`, is past what one
    /// screenshot may cost (`MAX_SHOT_PIXELS`, about 128 MB as RGBA).
    pub fn too_big(&self, view: (f64, f64), zoom: f64) -> bool {
        let (w, h) = match self.clip {
            Some((_, _, w, h)) => (w, h),
            None => view,
        };
        w * h * zoom * zoom > MAX_SHOT_PIXELS
    }

    /// The size the page must be laid out at for the clip to be on it
    /// (None: it already is, at `view`).
    pub fn grow_to(&self, view: (f64, f64)) -> Option<(f64, f64)> {
        let (x, y, w, h) = self.clip?;
        (self.beyond && (x + w > view.0 + 0.5 || y + h > view.1 + 0.5)).then(|| ((x + w).max(view.0), (y + h).max(view.1)))
    }
}

// ---- frames and events ----------------------------------------------------------------

/// `scheme://host[:port]`, as CDP's `securityOrigin`.
fn origin(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
            let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
            format!("{scheme}://{host}")
        }
        None => "://".into(),
    }
}

/// The page's frame, as `Page.getFrameTree` and `Page.frameNavigated` carry it.
pub fn frame(url: &str, loader: u64) -> Value {
    let url = if url.is_empty() { "about:blank" } else { url };
    json!({
        "id": FRAME, "loaderId": format!("L{loader}"), "url": url, "securityOrigin": origin(url), "mimeType": "text/html",
        "domainAndRegistry": "", "secureContextType": if url.starts_with("https:") { "Secure" } else { "InsecureScheme" },
        "crossOriginIsolatedContextType": "NotIsolated", "gatedAPIFeatures": []
    })
}

fn event(method: &str, params: Value) -> Value {
    json!({ "method": method, "params": params })
}

/// A navigation started (`didStartProvisionalNavigation`).
pub fn started() -> Value {
    event("Page.frameStartedLoading", json!({ "frameId": FRAME }))
}

/// A navigation committed: the old document's context goes, the new one comes.
pub fn committed(url: &str, loader: u64) -> Vec<Value> {
    let f = frame(url, loader);
    let context = json!({ "id": CONTEXT, "origin": f["securityOrigin"], "name": "", "uniqueId": format!("ctx-{loader}"),
        "auxData": { "isDefault": true, "type": "default", "frameId": FRAME } });
    vec![
        event("Runtime.executionContextsCleared", json!({})),
        event("Page.frameNavigated", json!({ "frame": f, "type": "Navigation" })),
        event("Runtime.executionContextCreated", json!({ "context": context })),
    ]
}

/// A load finished (`didFinishNavigation`); `ts` in seconds.
pub fn finished(ts: f64) -> Vec<Value> {
    vec![
        event("Page.domContentEventFired", json!({ "timestamp": ts })),
        event("Page.loadEventFired", json!({ "timestamp": ts })),
        event("Page.frameStoppedLoading", json!({ "frameId": FRAME })),
    ]
}

/// One message from the page world's console relay (webkit.rs), as a CDP
/// event: `{"t": "log", "a": ["text", …]}` for a console call, `{"err":
/// "message", "src": url, "line": n}` for an uncaught error. The page can
/// post these itself, so anything else, and anything past `MAX_CONSOLE`, is
/// dropped. `ms`: milliseconds since the epoch.
pub fn console(text: &str, ms: f64) -> Option<Value> {
    if text.len() > MAX_CONSOLE {
        return None;
    }
    let Value::Object(m) = serde_json::from_str::<Value>(text).ok()? else { return None };
    if let Some(err) = m.get("err") {
        let text = err.as_str()?;
        let line = m.get("line").and_then(Value::as_i64).unwrap_or(0);
        let url = m.get("src").and_then(Value::as_str).unwrap_or_default();
        return Some(event(
            "Runtime.exceptionThrown",
            json!({ "timestamp": ms, "exceptionDetails": { "exceptionId": 1, "text": text, "lineNumber": line, "columnNumber": 0, "url": url } }),
        ));
    }
    let kind = match m.get("t").and_then(Value::as_str)? {
        k @ ("log" | "info" | "debug" | "error") => k,
        "warn" => "warning",
        _ => return None,
    };
    let args: Vec<Value> = m.get("a")?.as_array()?.iter().map(|a| json!({ "type": "string", "value": a.as_str().unwrap_or_default() })).collect();
    Some(event("Runtime.consoleAPICalled", json!({ "type": kind, "args": args, "executionContextId": CONTEXT, "timestamp": ms })))
}

// ---- the accessibility tree ----------------------------------------------------------------

/// Each property the isolated world reports, with its CDP value type, in the order they go out.
const AX_PROPS: [(&str, &str); 6] = [
    ("level", "integer"),
    ("checked", "tristate"),
    ("expanded", "booleanOrUndefined"),
    ("selected", "booleanOrUndefined"),
    ("disabled", "boolean"),
    ("required", "boolean"),
];

/// `Accessibility.getFullAXTree`'s answer from the isolated world's raw
/// tree: `{"nodes": [[id, role, name, value|null, {props}, [child ids]], …]}`,
/// parents before children (agent-browser numbers refs in this order). The
/// DOM node's id is both the AX node id and its `backendDOMNodeId`. A node
/// whose shape is off is dropped, never guessed at.
pub fn ax_tree(raw: &Value) -> Result<Value, String> {
    let rows = raw.get("nodes").and_then(Value::as_array).ok_or("no accessibility tree from the page")?;
    let nodes: Vec<Value> = rows.iter().filter_map(ax_node).collect();
    Ok(json!({ "nodes": nodes }))
}

fn ax_node(row: &Value) -> Option<Value> {
    let [id, role, name, value, props, kids] = row.as_array()?.as_slice() else { return None };
    let id = id.as_i64().filter(|i| *i > 0)?;
    let kids: Vec<Value> = kids.as_array()?.iter().filter_map(Value::as_i64).map(|k| json!(k.to_string())).collect();
    let mut node = Map::new();
    node.insert("nodeId".into(), json!(id.to_string()));
    node.insert("ignored".into(), json!(false));
    node.insert("role".into(), json!({ "type": "role", "value": role.as_str()? }));
    node.insert("name".into(), json!({ "type": "computedString", "value": name.as_str().unwrap_or_default() }));
    let properties: Vec<Value> = AX_PROPS
        .iter()
        .filter_map(|(key, kind)| {
            let v = props.get(*key)?;
            let v = match *kind {
                "integer" => json!(v.as_i64()?),
                "tristate" => json!(match v { Value::Bool(b) => b.to_string(), Value::String(s) if ["true", "false", "mixed"].contains(&s.as_str()) => s.clone(), _ => return None }),
                _ => json!(v.as_bool()?),
            };
            Some(json!({ "name": key, "value": { "type": kind, "value": v } }))
        })
        .collect();
    if !properties.is_empty() {
        node.insert("properties".into(), json!(properties));
    }
    if let Some(v) = value.as_str() {
        node.insert("value".into(), json!({ "type": "string", "value": v }));
    }
    node.insert("childIds".into(), json!(kids));
    node.insert("backendDOMNodeId".into(), json!(id));
    Some(Value::Object(node))
}

// ---- the page in its block ---------------------------------------------------------------

/// Where the page's view goes in its block's container (x, y, width, height
/// in points, AppKit's bottom-left origin): filling it, or, for a viewport
/// `size` in CSS pixels, that size times the page's zoom (so the page lays
/// out at exactly that many CSS pixels), centred across and with its top at
/// the container's top. A viewport bigger than the block runs past its
/// right and bottom edges (the container clips it).
pub fn page_frame(room: (f64, f64), size: Option<(f64, f64)>, zoom: f64) -> (f64, f64, f64, f64) {
    let Some((w, h)) = size else { return (0.0, 0.0, room.0, room.1) };
    let zoom = if zoom.is_finite() && zoom > 0.0 { zoom } else { 1.0 };
    let (w, h) = (w * zoom, h * zoom);
    (((room.0 - w) / 2.0).max(0.0).round(), room.1 - h, w, h)
}

// ---- the URL gate on navigations --------------------------------------------------------------

/// Whether WebKit may load `url` into the page (`decidePolicyForNavigationAction`).
/// `target`: the frame it loads in: Some(true) the main frame, Some(false) a
/// subframe, None a new window (the UI delegate decides those, as popups).
/// The main frame gets the one URL gate (`agent::allowed_url`); a subframe
/// isn't gated, as with CEF.
pub fn may_load(url: &str, target: Option<bool>) -> bool {
    match target {
        Some(true) => super::agent::allowed_url(url),
        Some(false) | None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx<'static> {
        Ctx { url: "http://127.0.0.1:8765/browser-fixture.html", loader: 3, user_agent: "UA", buttons_down: false }
    }

    #[test]
    fn each_method_agent_browser_sends_maps_to_one_step() {
        let c = ctx();
        let p = json!({});
        assert_eq!(plan("Page.enable", &p, &c), Step::Reply(json!({})));
        assert_eq!(plan("Runtime.runIfWaitingForDebugger", &p, &c), Step::Reply(json!({})));
        assert_eq!(plan("Page.reload", &p, &c), Step::Reload);
        assert_eq!(plan("Runtime.evaluate", &json!({ "expression": "1", "returnByValue": true }), &c), Step::PageEval);
        assert_eq!(plan("Runtime.evaluate", &json!({ "expression": "document.body" }), &c), Step::World("evaluate"));
        assert_eq!(plan("Runtime.callFunctionOn", &p, &c), Step::World("callFunctionOn"));
        assert_eq!(plan("DOM.resolveNode", &p, &c), Step::World("resolveNode"));
        assert_eq!(plan("DOM.getBoxModel", &p, &c), Step::World("getBoxModel"));
        assert_eq!(plan("Accessibility.getFullAXTree", &p, &c), Step::AxTree);
        assert_eq!(plan("Page.getLayoutMetrics", &p, &c), Step::World("layoutMetrics"));
        assert_eq!(plan("Input.insertText", &json!({ "text": "Ada" }), &c), Step::InsertText("Ada".into()));
        assert_eq!(plan("Page.navigate", &json!({ "url": "https://example.test/" }), &c), Step::Navigate("https://example.test/".into()));
        assert_eq!(plan("Page.navigate", &json!({}), &c), Step::Fail("Cannot navigate to invalid URL".into()));
        let v = plan("Browser.getVersion", &p, &c);
        let Step::Reply(v) = v else { panic!("{v:?}") };
        assert_eq!(v["userAgent"], "UA");
        assert_eq!(v["protocolVersion"], "1.3");
    }

    #[test]
    fn a_method_webkit_cannot_do_does_not_exist() {
        // Allowed by the proxy, but not translated: CDP's own "wasn't found".
        assert_eq!(plan("Network.getResponseBody", &json!({}), &ctx()), Step::Fail("'Network.getResponseBody' wasn't found".into()));
        assert_eq!(plan("Page.printToPDF", &json!({}), &ctx()), Step::Fail("'Page.printToPDF' wasn't found".into()));
    }

    #[test]
    fn the_frame_tree_names_the_page_and_its_loader() {
        let Step::Reply(v) = plan("Page.getFrameTree", &json!({}), &ctx()) else { panic!() };
        let f = &v["frameTree"]["frame"];
        assert_eq!(f["id"], "F1");
        assert_eq!(f["loaderId"], "L3");
        assert_eq!(f["url"], "http://127.0.0.1:8765/browser-fixture.html");
        assert_eq!(f["securityOrigin"], "http://127.0.0.1:8765");
        assert_eq!(frame("", 1)["url"], "about:blank");
        assert_eq!(frame("https://u:p@a.test/x?y", 1)["securityOrigin"], "https://a.test");
        assert_eq!(frame("https://a.test/", 1)["secureContextType"], "Secure");
    }

    #[test]
    fn mouse_events_keep_their_kind_button_and_modifiers() {
        let m = mouse(&json!({ "type": "mousePressed", "x": 40.5, "y": 12, "button": "left", "clickCount": 1, "modifiers": 8 }), false).unwrap();
        assert_eq!((m.kind, m.button, m.clicks, m.x, m.y), (MouseKind::Down, Button::Left, 1, 40.5, 12.0));
        assert_eq!(m.mods, Mods { shift: true, ..Mods::default() });
        // A move with a button down is a drag.
        assert_eq!(mouse(&json!({ "type": "mouseMoved", "x": 1, "y": 1 }), true).unwrap().kind, MouseKind::Drag);
        assert_eq!(mouse(&json!({ "type": "mouseMoved", "x": 1, "y": 1 }), false).unwrap().kind, MouseKind::Move);
        assert_eq!(mouse(&json!({ "type": "mouseReleased", "button": "right" }), true).unwrap().button, Button::Right);
        // A wheel runs as a scroll in the isolated world; garbage is refused.
        assert_eq!(plan("Input.dispatchMouseEvent", &json!({ "type": "mouseWheel", "deltaY": 200 }), &ctx()), Step::World("wheel"));
        assert!(matches!(plan("Input.dispatchMouseEvent", &json!({ "type": "touch" }), &ctx()), Step::Fail(_)));
        // NaN and huge click counts don't reach AppKit.
        let m = mouse(&json!({ "type": "mousePressed", "x": "x", "clickCount": 99 }), false).unwrap();
        assert_eq!((m.x, m.clicks), (0.0, 3));
        assert_eq!(mods(15), Mods { alt: true, ctrl: true, cmd: true, shift: true });
    }

    #[test]
    fn keys_carry_macos_key_codes_and_the_characters_appkit_reports() {
        let k = key(&json!({ "type": "keyDown", "key": "Enter", "text": "\r" })).unwrap();
        assert_eq!((k.up, k.code, k.chars.as_str()), (false, 36, "\r"));
        let k = key(&json!({ "type": "keyDown", "key": "Tab" })).unwrap();
        assert_eq!((k.code, k.chars.as_str()), (48, "\t"));
        let k = key(&json!({ "type": "keyDown", "key": "a", "text": "a" })).unwrap();
        assert_eq!((k.code, k.chars.as_str()), (0, "a"));
        let k = key(&json!({ "type": "keyDown", "key": "Q", "text": "Q", "modifiers": 8 })).unwrap();
        assert_eq!((k.code, k.chars.as_str(), k.plain.as_str(), k.mods.shift), (12, "Q", "q", true));
        assert_eq!(key(&json!({ "type": "rawKeyDown", "key": "x" })).unwrap().code, 7);
        assert_eq!(key(&json!({ "type": "keyDown", "key": "7" })).unwrap().code, 26);
        assert_eq!(key(&json!({ "type": "keyUp", "key": "ArrowLeft" })).unwrap().chars, "\u{f702}");
        // `char` events type nothing of their own.
        assert_eq!(key(&json!({ "type": "char", "text": "a" })), None);
        assert_eq!(plan("Input.dispatchKeyEvent", &json!({ "type": "char", "text": "a" }), &ctx()), Step::Reply(json!({})));
        // An unknown named key keeps its text and no code.
        assert_eq!(key(&json!({ "type": "keyDown", "key": "F13", "text": "" })).unwrap().code, 0);
    }

    #[test]
    fn screenshots_grow_the_page_only_for_a_clip_past_it() {
        let Step::Screenshot(s) = plan("Page.captureScreenshot", &json!({ "format": "png" }), &ctx()) else { panic!() };
        assert_eq!((s.clip, s.beyond, s.jpeg), (None, false, false));
        assert_eq!(s.grow_to((800.0, 600.0)), None);
        let full = json!({ "format": "jpeg", "captureBeyondViewport": true, "clip": { "x": 0, "y": 0, "width": 800, "height": 2400, "scale": 1 } });
        let Step::Screenshot(s) = plan("Page.captureScreenshot", &full, &ctx()) else { panic!() };
        assert!(s.jpeg);
        assert_eq!(s.grow_to((800.0, 600.0)), Some((800.0, 2400.0)));
        // Inside the page: no growing, beyond or not.
        assert_eq!(Shot { clip: Some((0.0, 0.0, 800.0, 600.0)), beyond: true, jpeg: false }.grow_to((800.0, 600.0)), None);
        // A zero or broken clip is no clip.
        let Step::Screenshot(s) = plan("Page.captureScreenshot", &json!({ "clip": { "width": 0, "height": 5 } }), &ctx()) else { panic!() };
        assert_eq!(s.clip, None);
    }

    #[test]
    fn a_screenshot_past_the_pixel_limit_is_refused() {
        let shot = |w: f64, h: f64| Shot { clip: Some((0.0, 0.0, w, h)), beyond: true, jpeg: false };
        // A long full page at 2x is fine; the old 20,000 x 20,000 clip is not.
        assert!(!shot(1280.0, 6000.0).too_big((1280.0, 800.0), 2.0));
        assert!(shot(20_000.0, 20_000.0).too_big((1280.0, 800.0), 1.0));
        // Zoom counts: the same clip doubles each side at 2x.
        assert!(!shot(4096.0, 4096.0).too_big((1280.0, 800.0), 1.0));
        assert!(shot(4096.0, 4096.0).too_big((1280.0, 800.0), 2.0));
        // No clip: the visible page.
        assert!(!Shot { clip: None, beyond: false, jpeg: false }.too_big((1280.0, 800.0), 2.0));
    }

    #[test]
    fn device_metrics_become_a_size_only_viewport() {
        let c = ctx();
        assert_eq!(plan("Emulation.setDeviceMetricsOverride", &json!({ "width": 640, "height": 480, "deviceScaleFactor": 3, "mobile": true }), &c), Step::Viewport(Some((640, 480))));
        assert_eq!(plan("Emulation.setDeviceMetricsOverride", &json!({ "width": 0, "height": 0 }), &c), Step::Viewport(None));
        assert_eq!(plan("Emulation.clearDeviceMetricsOverride", &json!({}), &c), Step::Viewport(None));
        assert_eq!(plan("Emulation.setUserAgentOverride", &json!({ "userAgent": "" }), &c), Step::UserAgent(None));
        assert_eq!(plan("Network.setUserAgentOverride", &json!({ "userAgent": "X" }), &c), Step::UserAgent(Some("X".into())));
    }

    #[test]
    fn answers_and_navigations_have_cdps_shape() {
        assert_eq!(answer(7, Ok(json!({ "a": 1 }))), json!({ "id": 7, "result": { "a": 1 } }));
        assert_eq!(answer(8, Err("nope".into())), json!({ "id": 8, "error": { "code": -32000, "message": "nope" } }));
        assert_eq!(navigated(4, None), json!({ "frameId": "F1", "loaderId": "L4" }));
        assert_eq!(navigated(4, Some("timeout"))["errorText"], "timeout");
    }

    #[test]
    fn agent_script_is_compiled_with_the_call_never_string_evaluated() {
        // A page's CSP without 'unsafe-eval' refuses string eval: the helper
        // must not have any (github.com broke `get title`, `back` and `eval`).
        // Its code, comments cut, as identifier tokens: `(0, eval)(s)` and
        // `globalThis["eval"]` count too.
        let code: String = include_str!("webkit.js").lines().map(|l| l.split("//").next().unwrap_or_default()).collect::<Vec<_>>().join("\n");
        let tokens: Vec<&str> = code.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')).collect();
        assert!(!tokens.contains(&"eval"), "webkit.js evaluates a string");
        assert!(!tokens.contains(&"Function"), "webkit.js compiles a string");
        let s = spliced("document.title // a comment");
        assert!(s.starts_with(&format!("const {GOT} = ")));
        assert!(s.contains("ok: (\ndocument.title // a comment\n)"), "the comment can't eat the closing paren");
        assert!(s.contains(".call(globalThis)"));
        assert!(!s.contains("eval"));
        assert!(is_syntax_error("SyntaxError: Unexpected token ';'"));
        assert!(!is_syntax_error("EvalError: Refused to evaluate a string"));
    }

    #[test]
    fn a_scripts_value_and_its_exception_have_cdps_shape() {
        assert_eq!(script_result(None), json!({ "result": { "type": "undefined" } }));
        assert_eq!(script_result(Some(json!(null)))["result"]["subtype"], "null");
        assert_eq!(script_result(Some(json!(42))), json!({ "result": { "type": "number", "value": 42, "description": "42" } }));
        assert_eq!(script_result(Some(json!("armed")))["result"], json!({ "type": "string", "value": "armed" }));
        assert_eq!(script_result(Some(json!(true)))["result"]["type"], "boolean");
        assert_eq!(script_result(Some(json!({ "a": [1] })))["result"], json!({ "type": "object", "value": { "a": [1] } }));
        assert_eq!(script_opaque()["result"]["type"], "object");
        let t = thrown("SyntaxError: Unexpected end of script");
        assert_eq!(t["exceptionDetails"]["exception"]["description"], "SyntaxError: Unexpected end of script");
        assert_eq!(t["result"]["subtype"], "error");
    }

    #[test]
    fn page_events_follow_the_navigation() {
        let c = committed("http://127.0.0.1:8765/x", 5);
        let methods: Vec<&str> = c.iter().map(|e| e["method"].as_str().unwrap()).collect();
        assert_eq!(methods, ["Runtime.executionContextsCleared", "Page.frameNavigated", "Runtime.executionContextCreated"]);
        assert_eq!(c[2]["params"]["context"]["origin"], "http://127.0.0.1:8765");
        assert_eq!(c[2]["params"]["context"]["auxData"]["isDefault"], true);
        let f = finished(1.5);
        assert_eq!(f[1]["method"], "Page.loadEventFired");
        assert_eq!(started()["params"]["frameId"], "F1");
        // No sessionId here: the proxy's Link tags every page event with "".
        assert!(c.iter().chain(&f).all(|e| e.get("sessionId").is_none()));
    }

    #[test]
    fn console_relay_messages_become_cdp_events_and_nothing_else() {
        let e = console(r#"{"t":"log","a":["wk-console",{"x":1}]}"#, 10.0).unwrap();
        assert_eq!(e["method"], "Runtime.consoleAPICalled");
        assert_eq!(e["params"]["type"], "log");
        assert_eq!(e["params"]["args"], json!([{ "type": "string", "value": "wk-console" }, { "type": "string", "value": "" }]));
        assert_eq!(console(r#"{"t":"warn","a":[]}"#, 0.0).unwrap()["params"]["type"], "warning");
        let x = console(r#"{"err":"Error: boom","src":"http://a.test/x.js","line":3}"#, 1.0).unwrap();
        assert_eq!(x["method"], "Runtime.exceptionThrown");
        assert_eq!(x["params"]["exceptionDetails"]["text"], "Error: boom");
        assert_eq!(x["params"]["exceptionDetails"]["lineNumber"], 3);
        // What a page could post that isn't a relay message: dropped.
        for bad in [r#"{"t":"Target.attachedToTarget","a":[]}"#, r#"{"method":"Page.navigate"}"#, "[1,2]", "not json", r#"{"err":5}"#, r#"{"t":"log"}"#] {
            assert_eq!(console(bad, 0.0), None, "{bad}");
        }
        let big = format!(r#"{{"t":"log","a":["{}"]}}"#, "a".repeat(MAX_CONSOLE));
        assert_eq!(console(&big, 0.0), None);
    }

    #[test]
    fn the_ax_tree_comes_out_in_cdps_shape_in_the_pages_order() {
        let raw = json!({ "nodes": [
            [1, "RootWebArea", "Herdr fixture", null, {}, [2, 3, 5]],
            [2, "heading", "Profile", null, { "level": 2 }, [9]],
            [3, "textbox", "Name", "Ada", { "required": true }, []],
            [5, "checkbox", "Subscribe", null, { "checked": "false", "disabled": false }, []],
            [6, "combobox", "Favorite color", "green", { "expanded": false, "bogus": 1 }, [7]],
            [7, "option", "green", null, { "selected": true }, []],
            [9, "StaticText", "Profile", null, {}, []]
        ]});
        let out = ax_tree(&raw).unwrap();
        let nodes = out["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 7);
        assert_eq!(nodes[0], json!({ "nodeId": "1", "ignored": false, "role": { "type": "role", "value": "RootWebArea" },
            "name": { "type": "computedString", "value": "Herdr fixture" }, "childIds": ["2", "3", "5"], "backendDOMNodeId": 1 }));
        assert_eq!(nodes[1]["properties"], json!([{ "name": "level", "value": { "type": "integer", "value": 2 } }]));
        assert_eq!(nodes[2]["value"], json!({ "type": "string", "value": "Ada" }));
        assert_eq!(nodes[2]["properties"][0], json!({ "name": "required", "value": { "type": "boolean", "value": true } }));
        // checked is a tristate string; a boolean false still goes out.
        assert_eq!(nodes[3]["properties"], json!([
            { "name": "checked", "value": { "type": "tristate", "value": "false" } },
            { "name": "disabled", "value": { "type": "boolean", "value": false } }
        ]));
        // Unknown properties are dropped.
        assert_eq!(nodes[4]["properties"], json!([{ "name": "expanded", "value": { "type": "booleanOrUndefined", "value": false } }]));
        assert_eq!(nodes[5]["properties"][0]["name"], "selected");
        let ids: Vec<&str> = nodes.iter().map(|n| n["nodeId"].as_str().unwrap()).collect();
        assert_eq!(ids, ["1", "2", "3", "5", "6", "7", "9"], "parents before children, as the page sent them");
    }

    #[test]
    fn a_malformed_ax_row_is_dropped_not_guessed() {
        let raw = json!({ "nodes": [
            [1, "RootWebArea", "", null, {}, []],
            [0, "button", "zero id", null, {}, []],
            ["2", "button", "string id", null, {}, []],
            [3, 7, "role not text", null, {}, []],
            [4, "button", "short row"],
            [5, "checkbox", "bad tristate", null, { "checked": "maybe", "level": "x" }, []]
        ]});
        let nodes = ax_tree(&raw).unwrap()["nodes"].as_array().unwrap().clone();
        assert_eq!(nodes.iter().map(|n| n["nodeId"].as_str().unwrap()).collect::<Vec<_>>(), ["1", "5"]);
        assert!(nodes[1].get("properties").is_none());
        assert!(ax_tree(&json!({ "__error": "x" })).is_err());
    }

    #[test]
    fn a_viewport_is_its_css_size_at_the_top_of_its_block() {
        // No viewport: the page fills its block.
        assert_eq!(page_frame((640.0, 480.0), None, 1.0), (0.0, 0.0, 640.0, 480.0));
        // Smaller: centred across, its top at the block's top (bottom-left origin).
        assert_eq!(page_frame((640.0, 480.0), Some((390.0, 300.0)), 1.0), (125.0, 180.0, 390.0, 300.0));
        // Zoomed 110%: 390 CSS pixels are 429 points, so innerWidth stays 390.
        assert_eq!(page_frame((640.0, 900.0), Some((390.0, 844.0)), 1.1).2.round(), 429.0);
        // Bigger than the block: from its left edge, past its bottom (clipped).
        assert_eq!(page_frame((300.0, 400.0), Some((390.0, 844.0)), 1.0), (0.0, -444.0, 390.0, 844.0));
        // A broken zoom counts as 100%.
        assert_eq!(page_frame((640.0, 480.0), Some((100.0, 100.0)), 0.0).2, 100.0);
    }

    #[test]
    fn only_the_main_frame_is_gated() {
        assert!(may_load("https://example.test/", Some(true)));
        assert!(may_load("about:blank", Some(true)));
        for bad in ["file:///etc/passwd", "javascript:alert(1)", "data:text/html,x", "about:srcdoc", "webkit-inspector://x", "ftp://a.test/"] {
            assert!(!may_load(bad, Some(true)), "{bad}");
        }
        // Subframes load what the page asks; a new window goes to the popup rule.
        assert!(may_load("about:srcdoc", Some(false)));
        assert!(may_load("file:///x", None));
    }
}
