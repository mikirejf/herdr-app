// Agents in grouped or priority views; parents take their most urgent sub-agent's place.
// A viewed done agent stays in Done until left.

import { View, div, image, svg } from "gpui-kit";
// ---- readable colors: WCAG contrast, and a text color pushed until it reaches a minimum.
const hexOf = (c) => { c = String(c).replace("#", ""); if (c.length === 3) c = [...c].map((x) => x + x).join(""); const n = parseInt(c.slice(0, 6), 16); return [(n >> 16) & 255, (n >> 8) & 255, n & 255, c.length >= 8 ? parseInt(c.slice(6, 8), 16) / 255 : 1]; };
const toHex = (v) => "#" + v.slice(0, 3).map((x) => Math.round(Math.max(0, Math.min(255, x))).toString(16).padStart(2, "0")).join("");
const over = (top, under) => { const a = hexOf(top), u = hexOf(under); return toHex([0, 1, 2].map((i) => a[i] * a[3] + u[i] * (1 - a[3]))); };
const mix = (a, b, k) => { const x = hexOf(a), y = hexOf(b); return toHex([0, 1, 2].map((i) => x[i] * k + y[i] * (1 - k))); };
const lum = (c) => { const v = hexOf(c).slice(0, 3).map((x) => { x /= 255; return x <= 0.03928 ? x / 12.92 : Math.pow((x + 0.055) / 1.055, 2.4); }); return 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2]; };
const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
function readable(t, rec) {
  const base = over(t.chrome, t.canvas);
  const solid = (bg) => over(bg ?? t.chrome, base);
  const ink = (fg, bg, min = 4.5) => {
    const b = solid(bg);
    const start = over(fg, b);
    const toward = lum(b) < 0.4 ? "#ffffff" : "#000000";
    let c = start;
    for (let k = 0.05; k <= 1.0001 && ratio(c, b) < min; k += 0.05) c = mix(toward, start, k);
    rec.low = Math.min(rec.low, ratio(c, b));
    return c;
  };
  // Dark or light digit on a fill, whichever reads better.
  const on = (fill) => { const f = solid(fill); const dark = over(t.canvas, f); const c = ratio(dark, f) >= ratio("#ffffff", f) ? dark : "#ffffff"; rec.low = Math.min(rec.low, ratio(c, f)); return c; };
  return { ink, on, solid, P: (bg) => ink(t.foreground, bg, 7), S: (bg) => ink(t.muted, bg, 4.5), C: (c, bg) => ink(c, bg, 4.5) };
}

import { v_flex, h_flex } from "gpui-base";
import { fonts, agents, terminals, panes, focused, spaces, run, theme, shared, setShared, setAgentOrder, openOverlay, closeOverlay, props, window, layout } from "herdr";

function find(node, id) {
  if (node?.id === id) return node;
  for (const child of node?.children ?? []) {
    const found = find(child, id);
    if (found) return found;
  }
  return null;
}

// The sidebar's rail goes by its width, not a flag: dragging its edge narrows it too.
const narrow = () => (find(layout(), "sidebar")?.size?.px ?? Infinity) < 160;

// To the rail (on macOS wide enough for the traffic lights), or back to the width it had.
function toggleSidebar() {
  const width = find(layout(), "sidebar")?.size?.px;
  if (!narrow()) setShared("sidebar.width", width);
  // Mods edited before the rail went by width still read the flag.
  setShared("sidebar.collapsed", !narrow());
  run("layout.resize", { id: "sidebar", px: narrow() ? (shared("sidebar.width") ?? 268) : window()?.platform === "macos" ? 76 : 44 });
}

const clean = (s) => (s ?? "").replace(/^[^\p{L}\p{N}]+/u, "").trim();

function lead(tab, all = panes()) {
  const mine = all.filter(p => p.tab === tab.id && p.terminal);
  return mine.find(p => p.program?.agent) ?? mine.find(p => p.focused) ?? mine[0];
}

function title(tab, l) {
  const prog = l?.program?.name;
  const live = clean(l?.title);
  const telling = live && live !== prog && !/^[~/]|^[A-Za-z]:\\/.test(live) && !live.includes("@") && live.toLowerCase() !== l?.program?.key;
  return tab.name === prog && telling ? live : tab.name;
}
const RANK = { blocked: 4, done: 3, working: 2, idle: 1 };
// A sub-agent without a pane isn't in agents(): its raw word, error counting as
// blocked. It's never done: it reports to its parent, and the parent's done is the news.
const kidStatus = (k, listed) => {
  const s = listed.get(k.address) ?? k.status;
  return s === "error" ? "blocked" : s === "done" ? "idle" : s;
};
const SORTS = [
  { id: "priority", label: "Priority", icon: "clock" },
  { id: "grouped", label: "Grouped by space", icon: "layers" },
];
const sortOf = () => SORTS.find((s) => s.id === shared("agents.sort")) ?? SORTS[0];
// Digits that line up: a monospace face (mods can't ask for tabular figures).
const mono = () => (fonts().includes("SF Mono") ? "SF Mono" : "Menlo");
const VARIANT = "a";
const PAD = 10;
const NUMW = { a: 20, b: 16, c: 0 }[VARIANT];
const GAP = 8;
const NAME_X = PAD + NUMW + GAP;
const SECTION = { 4: "Needs you", 3: "Done", 2: "Working", 1: "Idle" };
const SECTION_STATUS = { 4: "blocked", 3: "done", 2: "working" };

function duration(ms) {
  const m = Math.floor(ms / 60000);
  if (m < 1) return "<1m";
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  return h < 24 ? `${h}h` : `${Math.floor(h / 24)}d`;
}

// The agent's icon from the app's program table: a one-color glyph or a picture.
function icon(p, size, color) {
  if (!p?.icon) return null;
  return p.tint ? svg(p.icon).size(size).flex_none().text_color(color) : image(p.icon).w(size).h(size).flex_none();
}

export default class Agents extends View {
  init(_props, cx) {
    if (props()?.sortMenu) return;
    this.folded = new Set();
    // Each agent's last state and when it changed (ms), kept across restarts.
    this.changed = JSON.parse(localStorage.getItem("changed") ?? "{}");
    this.pins = JSON.parse(localStorage.getItem("pins") ?? "[]");
    cx.timer.every(30000, (cx) => cx.notify());
  }

  // Stamps agents whose state changed. Done and idle count as one: looking at a
  // finished agent turns done into idle, and looking isn't the agent doing anything.
  track(list, now) {
    let dirty = false;
    for (const a of list) {
      const s = a.status === "done" ? "idle" : a.status;
      if (this.changed[a.address]?.s === s) continue;
      this.changed[a.address] = { s, at: now };
      dirty = true;
    }
    if (!dirty) return;
    // Gone and old: the list alone is empty right after a start.
    const listed = new Set(list.map((a) => a.address));
    for (const [k, c] of Object.entries(this.changed)) if (!listed.has(k) && now - c.at > 30 * 86400000) delete this.changed[k];
    localStorage.setItem("changed", JSON.stringify(this.changed));
  }

  setPins(pins) {
    this.pins = pins;
    localStorage.setItem("pins", JSON.stringify(pins));
  }

  // Drops pins of agents that ended. Only for machines the list has heard from:
  // right after a start, or while a machine connects, its agents aren't listed yet.
  prunePins(all) {
    const machine = (addr) => (addr.includes("/") ? addr.split("/")[0] : "");
    const heard = new Set(all.map((k) => machine(k.address)));
    const live = new Set(all.filter((k) => k.state === "live").map((k) => k.address));
    const kept = this.pins.filter((p) => !heard.has(machine(p)) || live.has(p));
    if (kept.length !== this.pins.length) this.setPins(kept);
  }

  since(a) {
    return this.changed[a.address]?.at ?? 0;
  }

  // Looking at a done agent makes it idle; it stays with Done until you leave
  // it rather than drop away under you. Any other change moves it at once.
  hold(list) {
    const cur = list.find((a) => a.focused);
    const same = cur && cur.address === this.held?.address;
    // Arriving, or any change but done turning idle, sets what it was.
    if (!same || cur.status !== "idle") this.held = cur ? { address: cur.address, rank: RANK[cur.status] ?? 0 } : null;
  }

  rank(a) {
    const sawDone = a.address === this.held?.address && this.held.rank === RANK.done && a.status === "idle";
    return sawDone ? RANK.done : RANK[a.status] ?? 0;
  }

  sortMenu(t) {
    const on = sortOf().id;
    return v_flex()
      .w_full()
      .p(5)
      .bg(t.popover)
      .text_size(12.8)
      .child(div().px(8).pt(4).pb(6).text_size(11).text_color(t.dim).child("Sort agents"))
      .children(SORTS.map((s) => h_flex()
        .id(`sort-${s.id}`)
        .h(31)
        .px(8)
        .gap(9)
        .rounded(7)
        .items_center()
        .text_color(t.foreground)
        .hover((el) => el.bg(t.hover))
        .on_click(() => {
          setShared("agents.sort", s.id);
          closeOverlay(props().overlay);
        })
        .child(svg(`mods/workspace-chrome/${s.icon}.svg`).size(14).flex_none().text_color(t.muted))
        .child(div().flex_1().child(s.label))
        .when(s.id === on, (el) => el.child(svg("mods/workspace-chrome/check.svg").size(14).flex_none().text_color(t.foreground)))));
  }

  peek(addr) {
    run("terminal.peek", { terminal: addr });
  }

  // Live sub-agents of `addr`, depth first, each with the tree lines in front of it.
  tree(all, addr, lead = []) {
    const kids = all.filter((k) => k.parent === addr && k.state === "live");
    return kids.flatMap((k, i) => {
      const last = i === kids.length - 1;
      return [
        { t: k, lines: [...lead, last ? "└" : "├"] },
        ...this.tree(all, k.address, [...lead, last ? "" : "│"]),
      ];
    });
  }

  subRow(t, s, shown, compact) {
    const k = s.t;
    const st = kidStatus(k, this.listed);
    const on = k.address === shown;
    return h_flex()
      .id(`sub-${k.address}`)
      .pl(PAD + NUMW / 2)
      .pr(PAD)
      .rounded(6)
      .h(30)
      .flex_none()
      .gap(0)
      .items_center()
      .text_size(13)
      .when(on, (el) => el.bg(t.raised))
      .hover((s) => s.bg(t.hover))
      .on_click(() => this.peek(k.address))
      .children(s.lines.map((l, j) => div().self_start().mt(-4).w(9).h(19).mr(j === s.lines.length - 1 ? NUMW / 2 + GAP - 9 : 4).flex_none()
        .when(Boolean(l), el => el.border_l(1.5).border_color(t.input))
        .when(l === "└" || l === "├", el => el.border_b(1.5).rounded_bl(6))))
      .child(div().flex_1().truncate().text_color(on ? t.foreground : t.muted).child(k.name || k.template || k.program?.name || k.address))
      .child(this.status(t, { status: st }, compact));
  }

  // compact: a ! for blocked where a header already says "Needs you".
  status(t, a, compact = false) {
    const color = t[a.status] ?? t.dim;
    if (a.status === "working") return svg("mods/workspace-chrome/spinner.svg").size(12).mx(1).flex_none().text_color(t.working).spin(900);
    if (a.status === "blocked" && compact) return div().size(17).flex_none().rounded(4).bg(t.blocked + "26").text_size(12).font_bold().text_color(t.blocked).flex().items_center().justify_center().child("!");
    if (a.status === "blocked") return div().flex_none().px(6).h(20).rounded(5).bg(t.blocked + "26").text_size(12).font_semibold().text_color(t.blocked).flex().items_center().child("Needs you");
    // Idle: a ring; done: a dot.
    if (a.status === "idle") return div().size(7).mx(3).flex_none().rounded(4).border(1.5).border_color(t.idle ?? t.dim);
    return div().size(7).mx(3).flex_none().rounded(4).bg(a.status === "done" ? color : t.dim + "00");
  }

  render(cx) {
    const t = theme();
    if (props()?.sortMenu) return this.sortMenu(t);
    const collapsed = narrow();
    const sort = sortOf();
    let list = agents();
    this.listed = new Map(list.map((a) => [a.address, a.status]));
    const all = terminals();
    const shown = focused()?.agent;
    const now = Date.now();
    this.track(list, now);
    this.hold(list);
    const byPriority = sort.id === "priority";
    const urgency = new Map(list.map((a) => [a.address, Math.max(this.rank(a), ...this.tree(all, a.address).map((s) => RANK[kidStatus(s.t, this.listed)] ?? 0))]));
    const rankOf = (a) => urgency.get(a.address);
    if (byPriority) list = [...list].sort((x, y) => rankOf(y) - rankOf(x) || this.since(y) - this.since(x) || y.seq - x.seq);
    this.prunePins(all);
    const pinned = this.pins.map((p) => list.find((a) => a.address === p)).filter(Boolean);
    const rest = list.filter((a) => !this.pins.includes(a.address));
    list = [...pinned, ...rest];
    setAgentOrder(list.map((a) => a.address));
    // Its agent.focus index, the number cmd+N goes by.
    let position = new Map(list.map((a, i) => [a.address, i + 1]));

    const withKids = (a, i) => {
      const subs = collapsed ? [] : this.tree(all, a.address);
      const r = row(a, subs);
      if (this.folded.has(a.address)) return [r];
      return [r, ...subs.map((s) => this.subRow(t, s, shown, byPriority))];
    };

    // The fold chevron; folded, the count too, colored by the most urgent one.
    const fold = (a, subs) => {
      const { S, C } = readable(t, { low: Infinity });
      const sts = subs.map((s) => kidStatus(s.t, this.listed));
      const top = sts.reduce((m, s) => ((RANK[s] ?? 0) > (RANK[m] ?? 0) ? s : m), null);
      const open = !this.folded.has(a.address);
      return div()
        .id(`fold-${a.address}`)
        .h(20)
        .px(3)
        .gap(3)
        .flex()
        .items_center()
        .rounded(4)
        .text_size(12)
        .text_color(open ? S() : C(t[top] ?? t.muted))
        .cursor_pointer()
        .hover((s) => s.bg(t.hover).text_color(t.foreground))
        .on_click((_e, cx) => {
          cx.stop_propagation();
          open ? this.folded.add(a.address) : this.folded.delete(a.address);
          cx.notify();
        })
        .child(svg(`mods/workspace-chrome/chevron-${open ? "down" : "right"}.svg`).size(11).flex_none())
        .when(!open, (el) => el.child(String(subs.length)));
    };

    // A row reads "space \u00b7 detail": the detail is a tab someone named or the
    // agent's own title, never an automatic tab number unless two rows would
    // otherwise read the same.
    const tabs = spaces().flatMap(s => s.tabs);
    const label = (a) => {
      const tab = tabs.find(tab => tab.id === a.tab);
      return tab ? title(tab, lead(tab)) : a.title || a.program?.name || "";
    };
    const detail = new Map(list.map(a => [a.address, /^\d+$/.test(label(a)) ? null : label(a)]));
    const seen = {};
    for (const a of list) { const k = `${a.space_name}\u0000${detail.get(a.address) ?? ""}`; seen[k] = (seen[k] ?? 0) + 1; }
    for (const a of list) {
      if (detail.get(a.address) == null && seen[`${a.space_name}\u0000`] > 1) detail.set(a.address, label(a));
    }

    // ---- explore prelude
    const rec = { low: Infinity };
    const { ink, on, solid, P, S, C } = readable(t, rec);
    let LISTPX = 8;
    let rows;
    const FOC = over(t.accent + "33", t.chrome);
    const st = (a) => (a.status === "error" ? "blocked" : a.status);
    const scol = (a) => (st(a) === "idle" ? null : t[st(a)]);
    const quiet = (a) => st(a) === "idle" && !a.focused;
    const N = (a) => position.get(a.address);
    const NS = (a) => (N(a) <= 9 ? String(N(a)) : "");
    const nm = (a) => a.space_name || label(a);
    const more = (a) => detail.get(a.address);
    const dur = (a) => { const w = this.since(a); return w ? duration(now - w) : ""; };
    const spinner = () => svg("mods/workspace-chrome/spinner.svg").size(13).flex_none().text_color(t.working).spin(900);
    const click = (el, a) => el.id(`ag-${a.address}`).cursor_pointer().on_click(() => run("pane.focus", { pane: a.pane }));
    const kids = (a) => (collapsed ? [] : this.tree(all, a.address));
    const subsOf = (a) => (this.folded.has(a.address) ? [] : kids(a));
    const glyph = (a, bg) => st(a) === "working" ? spinner() : div().flex_none().text_size(12).text_color(st(a) === "idle" ? S(bg) : C(scol(a), bg)).child(st(a) === "idle" ? "\u25cb" : "\u25cf");
    // A sub-agent: tree lines from `indent`, its name at `nameX`.
    const subLine = (s, indent, nameX, bg = t.chrome) => {
      const k = s.t, kst = kidStatus(k, this.listed), n = s.lines.length;
      return h_flex().id(`sub-${k.address}`).h(30).flex_none().pl(indent).pr(10).items_center().text_size(13).cursor_pointer()
        .hover((e) => e.bg(t.hover)).on_click(() => this.peek(k.address))
        .children(s.lines.map((l, j) => div().self_start().mt(-4).w(9).h(19).mr(j === n - 1 ? Math.max(4, nameX - indent - 9 * n - 4 * (n - 1)) : 4).flex_none()
          .when(Boolean(l), (e) => e.border_l(1.5).border_color(t.input)).when(l === "\u2514" || l === "\u251c", (e) => e.border_b(1.5).rounded_bl(6))))
        .child(div().flex_1().min_w(0).truncate().text_color(S(bg)).child(k.name || k.template || k.program?.name || k.address))
        .child(kst === "working" ? spinner() : this.status(t, { status: kst }, true));
    };
    let groups = pinned.length ? [{ id: "pinned", label: "Pinned", status: null, members: pinned }] : [];
    for (const a of rest) {
      const r = rankOf(a) || RANK.idle;
      const id = byPriority ? `sec-${r}` : `sp-${a.space}`;
      if (groups.at(-1)?.id === id) groups.at(-1).members.push(a);
      else groups.push({ id, label: byPriority ? SECTION[r] : a.space_name || "", status: byPriority ? SECTION_STATUS[r] : null, members: [a] });
    }
    const header = (g, px = 10, lead = null) => {
      const open = !this.folded.has(g.id);
      return h_flex().id(g.id).h(lead ? 30 : 34).when(!lead, (e) => e.pt(8)).flex_none().items_center().cursor_pointer()
        .on_click((_e, cx) => { open ? this.folded.add(g.id) : this.folded.delete(g.id); cx.notify(); })
        .when(Boolean(lead), (e) => e.child(lead))
        .child(h_flex().flex_1().min_w(0).px(px).gap(6).items_center().text_size(12.5).font_medium()
          .when(Boolean(g.mark), (e) => e.child(div().size(10).rounded(3).flex_none().bg(g.mark)))
          .child(div().truncate().text_color(g.status ? C(t[g.status]) : S()).child(g.label))
          .child(div().text_color(S()).child(String(g.members.length)))
          .child(div().flex_1())
          .when(!open, (e) => e.child(svg("mods/workspace-chrome/chevron-right.svg").size(12).flex_none().text_color(S()))));
    };
    const sectioned = (rowFn, subFn, lead) => groups.flatMap((g) => [header(g, 10, lead ? lead(null) : null), ...(this.folded.has(g.id) ? [] : g.members.flatMap((a) => [rowFn(a), ...subsOf(a).map((s) => subFn(s, a))]))]);
    const flat = (rowFn, subFn) => list.flatMap((a) => [rowFn(a), ...subsOf(a).map((s) => subFn(s, a))]);
    const rail = (a) => click(h_flex(), a).h(32).px(6).gap(6).flex_none().items_center().when(a.focused, (e) => e.bg(FOC))
      .child(div().flex_1().text_right().text_size(14).font_bold().text_color(a.focused ? C(t.accent, FOC) : P()).child(NS(a)))
      .child(h_flex().w(14).flex_none().justify_center().child(glyph(a, a.focused ? FOC : t.chrome)));
    const dot = (a, bg) => st(a) === "working" ? spinner() : scol(a) ? div().size(9).mx(2).rounded(5).flex_none().bg(scol(a)) : div().size(9).mx(2).rounded(5).flex_none().border(1.5).border_color(S(bg));
    const tintOf = (a, k = "24") => (scol(a) ? over(scol(a) + k, t.chrome) : null);
    const numeral = (a, bg, color) => div().w(26).flex_none().text_center().text_size(24).font_weight(a.focused ? 600 : 400).text_color(color).child(NS(a));
    const statusNum = (a, bg) => (a.focused ? C(t.accent, bg) : scol(a) ? C(scol(a), bg) : S(bg));
    const two = (a, bg, { lead = null, end = null, durInline = true } = {}) => {
      const subs = kids(a);
      return v_flex().flex_1().min_w(0).gap(1)
        .child(h_flex().gap(7).items_center().when(Boolean(lead), (e) => e.child(lead))
          .child(div().flex_1().min_w(0).truncate().text_size(15).font_semibold().text_color(quiet(a) ? S(bg) : P(bg)).child(nm(a)))
          .when(subs.length > 0, (e) => e.child(fold(a, subs)))
          .when(!lead && st(a) === "working", (e) => e.child(spinner())))
        .child(h_flex().gap(6).text_size(12).text_color(S(bg)).when(Boolean(lead), (e) => e.pl(20))
          .child(div().flex_1().min_w(0).truncate().child(more(a) ?? a.program?.name ?? ""))
          .when(durInline, (e) => e.child(div().flex_none().font_family(mono()).child(dur(a)))));
    };
    const card = (a, bg, body, edge) => {
      const f = a.focused;
      return click(v_flex(), a).mb(6).flex_none().rounded(8).bg(bg).pr(10).py(8).gap(2)
        .when(f, (e) => e.pl(8).border(2).border_color(t.accent))
        .when(!f, (e) => e.pl(edge ? 7 : 10).when(Boolean(edge), (x) => x.border_l(3).border_color(edge)).hover((s) => s.bg(over(t.hover, bg))))
        .child(body)
        .children(subsOf(a).map((s) => h_flex().id(`sub-${s.t.address}`).pl(36).gap(6).items_center().text_size(13).on_click((_e, cx) => { cx.stop_propagation(); this.peek(s.t.address); })
          .child(div().text_color(S(bg)).child("\u2514"))
          .child(div().flex_1().min_w(0).truncate().text_color(S(bg)).child(s.t.name || s.t.program?.name || s.t.address))
          .child(kidStatus(s.t, this.listed) === "working" ? spinner() : this.status(t, { status: kidStatus(s.t, this.listed) }, true))));
    };
    const cards = (one) => groups.flatMap((g) => [header(g, 4), ...(this.folded.has(g.id) ? [] : g.members.map(one))]);
    LISTPX = 10;
    rows = cards((a) => {
      const f = a.focused, tint = scol(a) ? over(scol(a) + "2e", t.raised) : null;
      const bg = f ? over(t.accent + "40", tint ?? t.raised) : tint ?? (quiet(a) ? mix(t.raised, t.chrome, 0.45) : t.raised);
      return card(a, bg, h_flex().gap(9).items_center()
        .child(dot(a, bg))
        .child(two(a, bg))
        .child(div().w(16).flex_none().text_right().text_size(19).font_semibold().font_family(mono()).text_color(f ? C(t.accent, bg) : quiet(a) ? S(bg) : P(bg)).child(NS(a))), null);
    });
    if (collapsed) rows = list.map(rail);
    console.log("LOW", "4", rec.low.toFixed(2));
    return v_flex()
      .size_full()
      .min_h(0)
      .bg(t.chrome)
      .border_t(1)
      .border_color(t.border)
      .text_size(14)
      .when(!collapsed, (el) =>
        el.child(
          h_flex()
            .px(16)
            .h(41)
            .pt(14)
            .pb(5)
            .flex_none()
            .items_center()
            .justify_between()
            .child(div().text_size(12.5).font_medium().text_color(S()).child("Agents"))
            .child(
              div()
                .id("ag-sort")
                .text_size(11)
                .text_color(t.dim)
                .hover((s) => s.text_color(t.foreground))
                .on_click(() => openOverlay({ mod: "agents", kind: "popup", width: 200, height: 140, props: { sortMenu: true } }))
                .tooltip(`Sort agents: ${sort.label}`)
                .child(svg(`mods/workspace-chrome/${sort.icon}.svg`).size(14)),
            ),
        ),
      )
      .when(list.length === 0 && !collapsed, (el) =>
        el.child(div().px(12).text_size(12).text_color(t.dim).child("No agent status yet. Agents show up here when they report their status. Older versions and agents inside tmux may not.")),
      )
      .child(v_flex().id("ag-list").flex_1().min_h(0).px(collapsed ? 0 : LISTPX).overflow_y_scrollbar().mode("always").children(rows))
      .when(collapsed, (el) =>
        el.child(
          h_flex()
            .h(28)
            .items_center()
            .justify_center()
            .child(
              div()
                .id("collapse")
                .text_size(13)
                .text_color(t.dim)
                .hover((s) => s.text_color(t.foreground))
                .on_click(toggleSidebar)
                .child("»"),
            ),
        ),
      );
  }
}
