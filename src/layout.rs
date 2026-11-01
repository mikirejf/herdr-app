//! Layout primitives: a tree of blocks and containers.
//! A node is a block (terminal, mod, or the active tab) or a
//! container (row, column, stack). Pure data + tree operations, so the
//! rules are testable without a window.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What a block shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Terminal { terminal: String },
    Mod {
        #[serde(rename = "mod")]
        name: String,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        props: Value,
    },
    /// A tab's terminals as it arranges them (`"active"`: the selected one),
    /// each pane wrapped as `pane` says.
    Context {
        /// `context`: 0.1.0's word, in layouts made then.
        #[serde(alias = "context")]
        tab: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pane: Option<PaneFrame>,
    },
    /// An agent's terminal, full size (`"focused"`: the selected agent).
    Agent { agent: String },
}

/// Which side of its node a floating block sits on. Left/right make it
/// vertical, top/bottom horizontal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Top,
    #[default]
    Right,
    Bottom,
    Left,
}

impl Edge {
    pub fn vertical(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }
}

/// Where along its edge a floating block sits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    Start,
    #[default]
    Center,
    End,
}

/// A block floating over a node, snapped to one of its edges. People drag it
/// to another edge or corner; it sizes to its content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Float {
    pub id: String,
    pub content: Content,
    #[serde(default)]
    pub edge: Edge,
    #[serde(default)]
    pub align: Align,
    /// On a side edge: a drawer showing this many px next to the edge until
    /// hovered, then sliding fully open. None: always fully shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peek: Option<f32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Size {
    Px { px: f32 },
    Weight { weight: f32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Line {
    #[serde(default = "one")]
    pub width: f32,
    #[serde(default = "border_token")]
    pub color: String,
}

fn one() -> f32 {
    1.0
}
fn border_token() -> String {
    "border".into()
}

/// What a node is painted with: a color (theme name or `#rrggbb[aa]`), a
/// two-color gradient at an angle, or an image from the app's home folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Background {
    Color(String),
    Gradient {
        gradient: [String; 2],
        /// Degrees, CSS-style: 180 runs top to bottom.
        #[serde(default = "down")]
        angle: f32,
    },
    Image {
        image: String,
        #[serde(default)]
        fit: Fit,
    },
}

fn down() -> f32 {
    180.0
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fit {
    #[default]
    Cover,
    Contain,
}

/// Space inside a node's edge, px: one number, or `[vertical, horizontal]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Padding {
    All(f32),
    Axes([f32; 2]),
}

impl Padding {
    /// (vertical, horizontal)
    pub fn axes(self) -> (f32, f32) {
        match self {
            Padding::All(v) => (v, v),
            Padding::Axes([y, x]) => (y, x),
        }
    }
}

/// Layout look cascades down the tree: the nearest node setting a field wins.
/// Unset root fields fall back to settings.json.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Look {
    /// A preset or a file in themes/, or one for light and one for dark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeChoice>,
    /// Any theme color by name, `#rrggbb` or `#rrggbbaa`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colors: Option<std::collections::BTreeMap<String, String>>,
    /// The terminals' font and size (px at zoom 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f32>,
    /// The mods' font.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_font: Option<String>,
    /// Space between a terminal's edge and its text, px: [x, y].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_padding: Option<[f32; 2]>,
    /// 0..1: below 1, what's behind a terminal shows through its background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_opacity: Option<f32>,
    /// Terminal cell rounding, width, line height and dim text, as in settings.json.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_grid: Option<crate::settings::CellGrid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub faint_opacity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_thicken: Option<u8>,
}

/// A layout's theme: one name, or a pair the system's appearance (or the
/// person's `appearance`) picks from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ThemeChoice {
    One(String),
    Pair { light: String, dark: String },
}

impl ThemeChoice {
    pub fn pick(&self, dark: bool) -> &str {
        match self {
            ThemeChoice::One(n) => n,
            ThemeChoice::Pair { light, dark: d } => {
                if dark {
                    d
                } else {
                    light
                }
            }
        }
    }
}

/// How a focused node (one containing the focused block) looks instead.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FocusLook {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border: Option<Line>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HeaderContent {
    /// `"tabs"`: Herdr's plain built-in tab strip (fallback).
    Builtin(String),
    Mod {
        #[serde(rename = "mod")]
        name: String,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        props: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Header {
    pub content: HeaderContent,
    /// `always` (default) or `hover`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reveal: Option<String>,
    /// Height in px (default 30).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f32>,
    /// `top` (default) or `bottom`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<String>,
}

impl Header {
    pub fn hover(&self) -> bool {
        self.reveal.as_deref() == Some("hover")
    }

    pub fn bottom(&self) -> bool {
        self.position.as_deref() == Some("bottom")
    }
}

/// How a layout wraps each pane of the tab it shows: a look, and a
/// header mod per pane (told its `pane` and `terminal` in props).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<Header>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Background>,
    /// `sm`, `md` or `lg`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border: Option<Line>,
    /// The border while the pane has focus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<Line>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,
    /// Space between panes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<f32>,
    /// Frame a tab's only pane too (default: only when it's split).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lone: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Kind {
    Block {
        content: Content,
    },
    Row {
        children: Vec<Node>,
    },
    Column {
        children: Vec<Node>,
    },
    Stack {
        children: Vec<Node>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header: Option<Header>,
    },
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Who draws the window's title bar (the root node says).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Titlebar {
    /// The system's (on macOS, an empty strip the window buttons sit in).
    #[default]
    System,
    /// None: the layout draws its own, with `window_drag` nodes.
    #[serde(rename = "none")]
    Layout,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClonedFrom {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    #[serde(default)]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloned_from: Option<ClonedFrom>,
    #[serde(flatten)]
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Size>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
    /// A name for people: tab captions use it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Background>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border: Option<Line>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,
    /// `sm`, `md` or `lg`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub padding: Option<Padding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<FocusLook>,
    /// Theme, fonts, terminal look for everything inside (cascading).
    #[serde(flatten)]
    pub look: Look,
    /// row/column: space between children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<f32>,
    /// row/column: one line between children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub divider: Option<Line>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub movable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resizable: Option<bool>,
    /// Blocks floating over this node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub floating: Vec<Float>,
    /// Root only: `"system"` (default) or `"none"` (the layout draws its own).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub titlebar: Option<Titlebar>,
    /// Root only, macOS with `"titlebar": "none"`: where the window buttons sit, px `[x, y]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traffic_lights: Option<[f32; 2]>,
    /// Root only: a picture of the layout for the settings screen (svg or png,
    /// relative to the app's folder: `layouts/<name>.svg`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// Root only: the terminal right-click menu's mod (default: `menu`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_menu: Option<String>,
    /// Dragging this node moves the window; a double-click maximizes it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub window_drag: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Row,
    Column,
}

impl Node {
    pub fn new(id: impl Into<String>, kind: Kind) -> Node {
        Node {
            id: id.into(),
            cloned_from: None,
            kind,
            size: None,
            min: None,
            max: None,
            hidden: false,
            title: None,
            background: None,
            border: None,
            radius: None,
            shadow: None,
            padding: None,
            focus: None,
            look: Look::default(),
            gap: None,
            divider: None,
            movable: None,
            closable: None,
            resizable: None,
            floating: Vec::new(),
            titlebar: None,
            traffic_lights: None,
            preview: None,
            terminal_menu: None,
            window_drag: false,
        }
    }

    pub fn block(id: impl Into<String>, content: Content) -> Node {
        Node::new(id, Kind::Block { content })
    }

    pub fn container(id: impl Into<String>, axis: Axis, children: Vec<Node>) -> Node {
        Node::new(id, match axis {
            Axis::Row => Kind::Row { children },
            Axis::Column => Kind::Column { children },
        })
    }

    pub fn children(&self) -> &[Node] {
        match &self.kind {
            Kind::Row { children } | Kind::Column { children } | Kind::Stack { children, .. } => children,
            Kind::Block { .. } => &[],
        }
    }

    pub fn children_mut(&mut self) -> Option<&mut Vec<Node>> {
        match &mut self.kind {
            Kind::Row { children } | Kind::Column { children } | Kind::Stack { children, .. } => Some(children),
            Kind::Block { .. } => None,
        }
    }

    pub fn axis(&self) -> Option<Axis> {
        match self.kind {
            Kind::Row { .. } => Some(Axis::Row),
            Kind::Column { .. } => Some(Axis::Column),
            _ => None,
        }
    }

    pub fn content(&self) -> Option<&Content> {
        match &self.kind {
            Kind::Block { content } => Some(content),
            _ => None,
        }
    }

    pub fn terminal(&self) -> Option<&str> {
        match self.content() {
            Some(Content::Terminal { terminal }) => Some(terminal),
            _ => None,
        }
    }

    /// A pane in a tab: a terminal, or a mod beside them (a viewer, a board).
    pub fn is_pane(&self) -> bool {
        matches!(self.content(), Some(Content::Terminal { .. } | Content::Mod { .. }))
    }

    pub fn closable(&self) -> bool {
        self.closable.unwrap_or(true)
    }

    pub fn resizable(&self) -> bool {
        self.resizable.unwrap_or(true)
    }

    /// Gives every node without an id one, using `fresh`.
    pub fn fill_ids(&mut self, fresh: &mut impl FnMut() -> String) {
        if self.id.is_empty() {
            self.id = fresh();
        }
        if let Some(c) = self.children_mut() {
            for n in c {
                n.fill_ids(fresh);
            }
        }
    }

    /// Every node, depth first (self included).
    pub fn walk(&self) -> Vec<&Node> {
        let mut out = vec![self];
        for c in self.children() {
            out.extend(c.walk());
        }
        out
    }

    pub fn find(&self, id: &str) -> Option<&Node> {
        if self.id == id {
            return Some(self);
        }
        self.children().iter().find_map(|c| c.find(id))
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut Node> {
        if self.id == id {
            return Some(self);
        }
        self.children_mut()?.iter_mut().find_map(|c| c.find_mut(id))
    }

    pub fn find_float_mut(&mut self, id: &str) -> Option<&mut Float> {
        if let Some(i) = self.floating.iter().position(|f| f.id == id) {
            return self.floating.get_mut(i);
        }
        self.children_mut()?.iter_mut().find_map(|c| c.find_float_mut(id))
    }

    /// The ids from the root down to `id` (inclusive).
    pub fn path_to(&self, id: &str) -> Option<Vec<String>> {
        if self.id == id {
            return Some(vec![self.id.clone()]);
        }
        self.children().iter().find_map(|c| c.path_to(id)).map(|mut p| {
            p.insert(0, self.id.clone());
            p
        })
    }

    pub fn parent_of(&self, id: &str) -> Option<&Node> {
        let p = self.path_to(id)?;
        (p.len() >= 2).then(|| self.find(&p[p.len() - 2])).flatten()
    }

    /// Terminal blocks in tree order.
    pub fn terminals(&self) -> Vec<&Node> {
        self.walk().into_iter().filter(|n| n.terminal().is_some()).collect()
    }

    /// Pane blocks in tree order.
    pub fn panes(&self) -> Vec<&Node> {
        self.walk().into_iter().filter(|n| n.is_pane()).collect()
    }

    /// Visible blocks (hidden nodes and inactive stack children skipped).
    pub fn visible_blocks(&self) -> Vec<&Node> {
        if self.hidden {
            return vec![];
        }
        match &self.kind {
            Kind::Block { .. } => vec![self],
            Kind::Stack { children, active, .. } => match active_child(children, active.as_deref()) {
                Some(c) => c.visible_blocks(),
                None => vec![],
            },
            Kind::Row { children } | Kind::Column { children } => children.iter().flat_map(|c| c.visible_blocks()).collect(),
        }
    }

    /// Removes the node `id`. A row/column left with one child is replaced by
    /// that child (which takes its slot's size); stacks are kept even empty.
    pub fn remove(&mut self, id: &str) -> Option<Node> {
        let removed = remove_inner(self, id)?;
        collapse(self);
        Some(removed)
    }

    /// Puts `new` beside `target` along `axis` (after it). Reuses the parent
    /// if it already runs along that axis, else wraps the target.
    pub fn split(&mut self, target: &str, axis: Axis, new: Node, container_id: String) -> bool {
        if let Some(parent) = self.parent_of(target).map(|p| p.id.clone())
            && let Some(p) = self.find_mut(&parent)
            && p.axis() == Some(axis)
        {
            let c = p.children_mut().unwrap();
            let i = c.iter().position(|n| n.id == target).unwrap();
            // The new pane takes half of the target's share.
            let share = match c[i].size {
                Some(Size::Weight { weight }) => weight,
                None => 1.0,
                Some(Size::Px { .. }) => 1.0,
            };
            if !matches!(c[i].size, Some(Size::Px { .. })) {
                c[i].size = Some(Size::Weight { weight: share / 2.0 });
            }
            let mut new = new;
            new.size = Some(Size::Weight { weight: share / 2.0 });
            c.insert(i + 1, new);
            return true;
        }
        let Some(t) = self.find_mut(target) else { return false };
        let old = std::mem::replace(t, Node::new(container_id, Kind::Row { children: vec![] }));
        let mut old = old;
        let slot_size = old.size.take();
        let mut wrapper = Node::container(t.id.clone(), axis, vec![old, new]);
        wrapper.size = slot_size;
        *t = wrapper;
        true
    }

    /// Makes `child` the visible child of `stack`.
    pub fn select(&mut self, stack: &str, child: &str) -> bool {
        if let Some(s) = self.find_mut(stack)
            && let Kind::Stack { children, active, .. } = &mut s.kind
            && children.iter().any(|c| c.id == child)
        {
            *active = Some(child.to_owned());
            return true;
        }
        false
    }

    /// The visible child of a stack.
    pub fn active_child(&self) -> Option<&Node> {
        match &self.kind {
            Kind::Stack { children, active, .. } => active_child(children, active.as_deref()),
            _ => None,
        }
    }

    /// Gives every pane of the tree the same share of its row or column. A run of
    /// same-axis containers is one row of n segments, so three panes side by side
    /// get a third each. Fixed (px) and hidden children keep what they have.
    pub fn equalize(&mut self) {
        let axis = self.axis().filter(|_| self.children().len() > 1);
        if let Some(children) = self.children_mut() {
            for child in children.iter_mut() {
                if let Some(axis) = axis
                    && !child.hidden
                    && !matches!(child.size, Some(Size::Px { .. }))
                {
                    child.size = Some(Size::Weight { weight: child.segments(axis) as f32 });
                }
                child.equalize();
            }
        }
    }

    /// Moves the divider on `pane`'s edge one step: 5% of its row (`Axis::Row`) or
    /// column, right or down when `forward`. The divider is the nearest one on the
    /// pane's side in that direction, else the one on its other side (the last pane
    /// of a row moves its left divider). Each pane of the pair keeps 10% to 90% of
    /// their shared space; fixed (px) and hidden children are never resized.
    pub fn resize(&mut self, pane: &str, axis: Axis, forward: bool) -> bool {
        let Some(path) = self.path_to(pane) else { return false };
        let weight = |n: &Node| match n.size {
            Some(Size::Weight { weight }) => Some(weight),
            None => Some(1.0),
            Some(Size::Px { .. }) => None,
        };
        let flexible = |n: &Node| !n.hidden && weight(n).is_some();
        let divider = |toward_end: bool| {
            path.windows(2).rev().find_map(|pair| {
                let container = self.find(&pair[0]).filter(|c| c.axis() == Some(axis))?;
                let children = container.children();
                let i = children.iter().position(|c| c.id == pair[1]).filter(|&i| flexible(&children[i]))?;
                let j = if toward_end {
                    (i + 1..children.len()).find(|&j| !children[j].hidden)?
                } else {
                    (0..i).rev().find(|&j| !children[j].hidden)?
                };
                flexible(&children[j]).then(|| (pair[0].clone(), i.min(j), i.max(j)))
            })
        };
        let Some((container, a, b)) = divider(forward).or_else(|| divider(!forward)) else { return false };
        let children = self.find_mut(&container).and_then(Node::children_mut).unwrap();
        let total: f32 = children.iter().filter(|c| !c.hidden).filter_map(weight).sum();
        let (wa, wb) = (weight(&children[a]).unwrap(), weight(&children[b]).unwrap());
        let step = if forward { 0.05 * total } else { -0.05 * total };
        let new_a = (wa + step).clamp(0.1 * (wa + wb), 0.9 * (wa + wb));
        if new_a == wa {
            return false;
        }
        children[a].size = Some(Size::Weight { weight: new_a });
        children[b].size = Some(Size::Weight { weight: wa + wb - new_a });
        true
    }

    fn segments(&self, axis: Axis) -> usize {
        if self.axis() != Some(axis) {
            return 1;
        }
        self.children().iter().filter(|c| !c.hidden && !matches!(c.size, Some(Size::Px { .. }))).map(|c| c.segments(axis)).sum()
    }

    /// Swaps two nodes' places in the tree (sizes stay with the places).
    pub fn swap(&mut self, a: &str, b: &str) -> bool {
        let (Some(na), Some(nb)) = (self.find(a).cloned(), self.find(b).cloned()) else { return false };
        if na.walk().iter().any(|n| n.id == b) || nb.walk().iter().any(|n| n.id == a) {
            return false;
        }
        let (sa, sb) = (na.size, nb.size);
        if let Some(x) = self.find_mut(a) {
            *x = Node { size: sa, ..nb.clone() };
            x.id = format!("\u{0}swap-{b}");
        }
        if let Some(y) = self.find_mut(b) {
            *y = Node { size: sb, ..na };
        }
        if let Some(x) = self.find_mut(&format!("\u{0}swap-{b}")) {
            x.id = b.to_owned();
        }
        true
    }
}

fn active_child<'a>(children: &'a [Node], active: Option<&str>) -> Option<&'a Node> {
    let visible = || children.iter().filter(|c| !c.hidden);
    active.and_then(|a| visible().find(|c| c.id == a)).or_else(|| visible().next())
}

fn remove_inner(node: &mut Node, id: &str) -> Option<Node> {
    let c = node.children_mut()?;
    if let Some(i) = c.iter().position(|n| n.id == id) {
        let removed = c.remove(i);
        // Its share goes back to the neighbor it was split from (the one
        // before it, else the next), not spread over everyone.
        if matches!(node.kind, Kind::Row { .. } | Kind::Column { .. }) {
            let weight = |s: Option<Size>| match s {
                Some(Size::Weight { weight }) => Some(weight),
                None => Some(1.0),
                Some(Size::Px { .. }) => None,
            };
            let c = node.children_mut().unwrap();
            if let Some(gone) = weight(removed.size)
                && let Some(n) = c.get_mut(i.saturating_sub(1))
                && let Some(w) = weight(n.size)
            {
                n.size = Some(Size::Weight { weight: w + gone });
            }
        }
        if let Kind::Stack { children, active, .. } = &mut node.kind
            && active.as_deref() == Some(id)
        {
            // The neighbor that slides into its place (or the last one) becomes active.
            *active = children.get(i).or(children.last()).map(|n| n.id.clone());
        }
        return Some(removed);
    }
    c.iter_mut().find_map(|n| remove_inner(n, id))
}

fn collapse(node: &mut Node) {
    // A stack's children are tabs: they keep their identity even with one
    // pane or none (the owner decides when a tab closes).
    let in_stack = matches!(node.kind, Kind::Stack { .. });
    if let Some(c) = node.children_mut() {
        for n in c.iter_mut() {
            collapse(n);
        }
        if in_stack {
            return;
        }
        // Rows/columns emptied by removals vanish; ones left with one child give way to it.
        c.retain(|n| !(matches!(n.kind, Kind::Row { .. } | Kind::Column { .. }) && n.children().is_empty()));
        for n in c.iter_mut() {
            if matches!(n.kind, Kind::Row { .. } | Kind::Column { .. }) && n.children().len() == 1 {
                let size = n.size;
                let mut only = n.children_mut().unwrap().remove(0);
                only.size = size;
                *n = only;
            }
        }
    }
}

/// Where a floating block dropped with its center at `(x, y)` (relative to its
/// node, `w` × `h`) settles: the nearest edge, and the third along it.
pub fn snap(x: f32, y: f32, w: f32, h: f32) -> (Edge, Align) {
    let (x, y) = (x.clamp(0.0, w), y.clamp(0.0, h));
    // Distances as shares of the node, so wide windows don't favor top and bottom.
    let (fx, fy) = (x / w.max(1.0), y / h.max(1.0));
    let edge = [(Edge::Left, fx), (Edge::Right, 1.0 - fx), (Edge::Top, fy), (Edge::Bottom, 1.0 - fy)]
        .into_iter()
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(e, _)| e)
        .unwrap_or_default();
    let along = if edge.vertical() { fy } else { fx };
    let align = if along < 1.0 / 3.0 {
        Align::Start
    } else if along > 2.0 / 3.0 {
        Align::End
    } else {
        Align::Center
    };
    (edge, align)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_snap_to_the_nearest_edge_and_third() {
        assert_eq!(snap(10.0, 400.0, 1200.0, 800.0), (Edge::Left, Align::Center));
        assert_eq!(snap(1190.0, 20.0, 1200.0, 800.0), (Edge::Right, Align::Start));
        assert_eq!(snap(600.0, 5.0, 1200.0, 800.0), (Edge::Top, Align::Center));
        assert_eq!(snap(1000.0, 790.0, 1200.0, 800.0), (Edge::Bottom, Align::End));
        assert_eq!(snap(-50.0, 900.0, 1200.0, 800.0).1, Align::End);
    }

    #[test]
    fn floats_round_trip() {
        let json = r#"{ "id": "root", "type": "block", "content": {"agent": "focused"},
          "floating": [ { "id": "team", "content": {"mod": "team"}, "edge": "left" } ] }"#;
        let n: Node = serde_json::from_str(json).unwrap();
        assert_eq!(n.content(), Some(&Content::Agent { agent: "focused".into() }));
        assert_eq!((n.floating[0].edge, n.floating[0].align), (Edge::Left, Align::Center));
        let back: Node = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(back, n);
    }

    fn term(id: &str) -> Node {
        Node::block(id, Content::Terminal { terminal: format!("t-{id}") })
    }

    #[test]
    fn nodes_carry_looks_backgrounds_padding_and_shadows() {
        let json = r##"{ "id": "root", "type": "column", "theme": "ink", "font": "Berkeley Mono", "font_size": 13,
          "terminal_opacity": 0.85, "padding": [16, 12], "shadow": "lg",
          "background": { "gradient": ["#123a6e", "#2a6db3"], "angle": 160 },
          "children": [ { "id": "a", "type": "block", "content": { "mod": "x" }, "background": "surface", "padding": 8 },
                        { "id": "b", "type": "block", "content": { "mod": "y" }, "background": { "image": "bg/sky.jpg" } } ] }"##;
        let n: Node = serde_json::from_str(json).unwrap();
        assert_eq!((n.look.theme.as_ref().map(|t| t.pick(true)), n.look.font_size, n.look.terminal_opacity), (Some("ink"), Some(13.0), Some(0.85)));
        assert_eq!(n.padding.map(Padding::axes), Some((16.0, 12.0)));
        assert!(matches!(n.background, Some(Background::Gradient { angle, .. }) if angle == 160.0));
        assert_eq!(n.find("a").unwrap().background, Some(Background::Color("surface".into())));
        assert!(matches!(&n.find("b").unwrap().background, Some(Background::Image { fit: Fit::Cover, .. })));
        let back: Node = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn tab_blocks_say_how_their_panes_are_framed() {
        let json = r#"{ "id": "main", "type": "block", "content": { "tab": "active",
          "pane": { "header": { "content": { "mod": "pane" }, "height": 28, "reveal": "hover", "position": "bottom" },
                    "radius": 10, "gap": 8, "lone": true, "border": { "width": 1, "color": "border" } } } }"#;
        let n: Node = serde_json::from_str(json).unwrap();
        let Some(Content::Context { pane: Some(f), .. }) = n.content() else { panic!("no frame") };
        let h = f.header.as_ref().unwrap();
        assert!(h.hover() && h.bottom() && f.lone);
        assert_eq!((f.radius, f.gap, h.height), (Some(10.0), Some(8.0), Some(28.0)));
        let back: Node = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn documents_parse_with_types_sizes_and_looks() {
        let json = r#"{ "id": "root", "type": "row", "divider": {"width": 1, "color": "border"},
          "children": [
            { "id": "sidebar", "type": "column", "size": {"px": 260},
              "children": [ { "id": "spaces", "type": "block", "content": {"mod": "spaces"} },
                            { "id": "agents", "type": "block", "content": {"mod": "agents"} } ] },
            { "id": "main", "type": "block", "content": {"tab": "active"} } ] }"#;
        let n: Node = serde_json::from_str(json).unwrap();
        assert_eq!(n.find("sidebar").unwrap().size, Some(Size::Px { px: 260.0 }));
        assert_eq!(n.find("main").unwrap().content(), Some(&Content::Context { tab: "active".into(), pane: None }));
        let back: Node = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn splits_reuse_a_same_axis_parent_and_wrap_otherwise() {
        let mut n = Node::new("tab", Kind::Stack { children: vec![term("a")], active: None, header: None });
        assert!(n.split("a", Axis::Row, term("b"), "r1".into()));
        assert_eq!(n.find("r1").map(|r| r.children().len()), Some(2));
        assert!(n.split("b", Axis::Row, term("c"), "unused".into()));
        assert_eq!(n.find("r1").map(|r| r.children().len()), Some(3));
        assert!(n.split("c", Axis::Column, term("d"), "c1".into()));
        assert_eq!(n.find("c1").map(|c| c.children().iter().map(|x| x.id.clone()).collect::<Vec<_>>()), Some(vec!["c".into(), "d".into()]));
        assert_eq!(n.terminals().iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["a", "b", "c", "d"]);
    }

    #[test]
    fn closing_a_split_gives_its_share_back_to_the_pane_it_came_from() {
        let w = |n: &Node, id: &str| match n.find(id).and_then(|x| x.size) {
            Some(Size::Weight { weight }) => weight,
            _ => 1.0,
        };
        let mut a = term("a");
        a.size = Some(Size::Weight { weight: 0.7 });
        let mut b = term("b");
        b.size = Some(Size::Weight { weight: 0.3 });
        let mut n = Node::container("tab", Axis::Row, vec![a, b]);
        // b split the same way: c takes half of b's share; closing c gives it back.
        n.split("b", Axis::Row, term("c"), "unused".into());
        n.remove("c");
        assert_eq!((w(&n, "a"), w(&n, "b")), (0.7, 0.3));
        // Closing the first one hands its share to the next.
        n.split("a", Axis::Row, term("d"), "unused".into());
        n.remove("a");
        assert_eq!((w(&n, "d"), w(&n, "b")), (0.7, 0.3));
    }

    #[test]
    fn removing_collapses_single_children_but_keeps_stacks_and_the_root() {
        let tab = Node::new("t1", Kind::Row { children: vec![term("a"), term("b")] });
        let mut n = Node::new("tabs", Kind::Stack { children: vec![tab, term("solo")], active: Some("solo".into()), header: None });
        n.remove("b");
        // A stack's child keeps its identity even when one pane is left.
        assert_eq!(n.children()[0].id, "t1");
        assert_eq!(n.children()[0].children().len(), 1);
        n.remove("solo");
        assert_eq!(n.active_child().map(|c| c.id.as_str()), Some("t1"));
        // Inside it, a split left with one pane gives way to it.
        n.split("a", Axis::Column, term("c"), "c1".into());
        n.remove("c");
        assert_eq!(n.find("t1").unwrap().children()[0].id, "a");
        n.remove("t1");
        assert!(matches!(n.kind, Kind::Stack { ref children, .. } if children.is_empty()));
        // The root container itself never gives way (a tab's arrangement keeps its id).
        let mut root = Node::container("ctx", Axis::Row, vec![term("x"), term("y")]);
        root.remove("y");
        assert_eq!((root.id.as_str(), root.children().len()), ("ctx", 1));
    }

    #[test]
    fn stacks_select_and_hidden_children_fall_back() {
        let mut n = Node::new("s", Kind::Stack { children: vec![term("a"), term("b")], active: None, header: None });
        assert!(n.select("s", "b"));
        assert_eq!(n.visible_blocks().iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), ["b"]);
        n.find_mut("b").unwrap().hidden = true;
        assert_eq!(n.active_child().map(|c| c.id.as_str()), Some("a"));
    }

    fn weights(n: &Node, ids: &[&str]) -> Vec<f32> {
        ids.iter()
            .map(|id| match n.find(id).and_then(|x| x.size) {
                Some(Size::Weight { weight }) => weight,
                other => panic!("{id}: {other:?}"),
            })
            .collect()
    }

    fn unequal(id: &str, weight: f32) -> Node {
        let mut n = term(id);
        n.size = Some(Size::Weight { weight });
        n
    }

    #[test]
    fn equalizing_three_side_by_side_panes_gives_thirds() {
        let mut n = Node::container("t", Axis::Row, vec![unequal("a", 0.5), unequal("b", 0.25), unequal("c", 0.25)]);
        n.equalize();
        let w = weights(&n, &["a", "b", "c"]);
        assert_eq!(w[0], w[1]);
        assert_eq!(w[1], w[2]);
    }

    #[test]
    fn equalizing_columns_and_the_panes_stacked_in_one() {
        let right = Node::container("r", Axis::Column, vec![unequal("b", 0.8), unequal("c", 0.2)]);
        let mut n = Node::container("t", Axis::Row, vec![unequal("a", 0.9), right]);
        n.find_mut("r").unwrap().size = Some(Size::Weight { weight: 0.1 });
        n.equalize();
        let w = weights(&n, &["a", "r"]);
        assert_eq!(w[0], w[1]);
        let h = weights(&n, &["b", "c"]);
        assert_eq!(h[0], h[1]);
    }

    #[test]
    fn equalizing_nested_runs_gives_each_run_its_own_equal_shares() {
        // a | (b over (c | d | e)): the inner row is three equal panes.
        let inner = Node::container("in", Axis::Row, vec![unequal("c", 0.7), unequal("d", 0.2), unequal("e", 0.1)]);
        let mid = Node::container("mid", Axis::Column, vec![unequal("b", 0.9), inner]);
        let mut n = Node::container("t", Axis::Row, vec![unequal("a", 0.1), mid]);
        n.equalize();
        let w = weights(&n, &["a", "mid"]);
        assert_eq!(w[0], w[1]);
        let h = weights(&n, &["b", "in"]);
        assert_eq!(h[0], h[1]);
        let c = weights(&n, &["c", "d", "e"]);
        assert_eq!((c[0], c[1]), (c[1], c[2]));
    }

    #[test]
    fn equalizing_a_run_of_same_axis_containers_counts_every_segment() {
        // a | (b | c) as nested rows is three panes side by side, not 1/2, 1/4, 1/4.
        let nested = Node::container("n", Axis::Row, vec![unequal("b", 0.9), unequal("c", 0.1)]);
        let mut n = Node::container("t", Axis::Row, vec![unequal("a", 0.5), nested]);
        n.equalize();
        let w = weights(&n, &["a", "n"]);
        let inner = weights(&n, &["b", "c"]);
        assert_eq!(inner[0], inner[1]);
        assert_eq!(w[1] / w[0], inner[0] + inner[1]);
    }

    #[test]
    fn equalizing_one_pane_changes_nothing() {
        let mut n = Node::container("t", Axis::Row, vec![term("a")]);
        let before = n.clone();
        n.equalize();
        assert_eq!(n, before);
    }

    #[test]
    fn equalizing_leaves_fixed_sizes_alone() {
        let mut side = term("side");
        side.size = Some(Size::Px { px: 200.0 });
        let mut n = Node::container("t", Axis::Row, vec![side, unequal("a", 0.9), unequal("b", 0.1)]);
        n.equalize();
        assert_eq!(n.find("side").unwrap().size, Some(Size::Px { px: 200.0 }));
        let w = weights(&n, &["a", "b"]);
        assert_eq!(w[0], w[1]);
    }

    fn close(n: &Node, ids: &[&str], want: &[f32]) {
        let got: Vec<f32> = ids.iter().map(|id| match n.find(id).unwrap().size {
            Some(Size::Weight { weight }) => weight,
            None => 1.0,
            other => panic!("{id}: {other:?}"),
        }).collect();
        assert!(got.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-4) && got.len() == want.len(), "{got:?} != {want:?}");
    }

    #[test]
    fn resizing_two_side_by_side_panes_moves_their_divider() {
        let mut n = Node::container("t", Axis::Row, vec![term("a"), term("b")]);
        assert!(n.resize("a", Axis::Row, true));
        close(&n, &["a", "b"], &[1.1, 0.9]);
        // The right pane has no divider on its right: right moves its left one.
        assert!(n.resize("b", Axis::Row, true));
        close(&n, &["a", "b"], &[1.2, 0.8]);
        assert!(n.resize("b", Axis::Row, false));
        close(&n, &["a", "b"], &[1.1, 0.9]);
        // No column to resize in.
        assert!(!n.resize("a", Axis::Column, true));
    }

    #[test]
    fn resizing_the_middle_of_three_moves_the_divider_on_that_side() {
        let mut n = Node::container("t", Axis::Row, vec![term("a"), term("b"), term("c")]);
        assert!(n.resize("b", Axis::Row, true));
        close(&n, &["a", "b", "c"], &[1.0, 1.15, 0.85]);
        assert!(n.resize("b", Axis::Row, false));
        close(&n, &["a", "b", "c"], &[0.85, 1.3, 0.85]);
    }

    #[test]
    fn resizing_a_nested_pane_finds_the_container_with_the_divider() {
        // a | (b over c): c resizing left moves the outer divider, down moves b's.
        let right = Node::container("r", Axis::Column, vec![term("b"), term("c")]);
        let mut n = Node::container("t", Axis::Row, vec![term("a"), right]);
        assert!(n.resize("c", Axis::Row, false));
        close(&n, &["a", "r"], &[0.9, 1.1]);
        assert!(n.resize("b", Axis::Column, true));
        close(&n, &["b", "c"], &[1.1, 0.9]);
        // a | (b | c) as nested rows: b's left divider is the outer one.
        let inner = Node::container("in", Axis::Row, vec![term("b"), term("c")]);
        let mut n = Node::container("t", Axis::Row, vec![term("a"), inner]);
        assert!(n.resize("b", Axis::Row, false));
        close(&n, &["a", "in"], &[0.9, 1.1]);
        close(&n, &["b"], &[1.0]);
    }

    #[test]
    fn resizing_one_pane_or_a_fixed_neighbor_changes_nothing() {
        let mut n = Node::container("t", Axis::Row, vec![term("a")]);
        let before = n.clone();
        assert!(!n.resize("a", Axis::Row, true));
        assert_eq!(n, before);
        let mut side = term("side");
        side.size = Some(Size::Px { px: 200.0 });
        let mut n = Node::container("t", Axis::Row, vec![side, term("a")]);
        let before = n.clone();
        assert!(!n.resize("a", Axis::Row, false));
        assert_eq!(n, before);
    }

    #[test]
    fn resizing_stops_at_a_tenth_of_the_pair() {
        let mut n = Node::container("t", Axis::Row, vec![term("a"), term("b")]);
        for _ in 0..30 {
            n.resize("a", Axis::Row, true);
        }
        close(&n, &["a", "b"], &[1.8, 0.2]);
        assert!(!n.resize("a", Axis::Row, true));
    }

    #[test]
    fn swapping_keeps_sizes_with_places() {
        let mut n = Node::container("r", Axis::Row, vec![term("a"), term("b"), term("c")]);
        n.find_mut("a").unwrap().size = Some(Size::Px { px: 100.0 });
        assert!(n.swap("a", "c"));
        let ids: Vec<_> = n.children().iter().map(|c| c.id.clone()).collect();
        assert_eq!(ids, ["c", "b", "a"]);
        assert_eq!(n.children()[0].size, Some(Size::Px { px: 100.0 }));
    }
}
