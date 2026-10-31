// Agents in grouped or priority views; parents take their most urgent sub-agent's place.
// A viewed done agent stays in Done until left. Each row is tinted by its status and
// leads with its agent.focus number (cmd+1..9); the focused one is outlined in the accent.

import { View, div, svg } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { agents, terminals, panes, focused, spaces, run, theme, shared, setShared, setAgentOrder, openOverlay, closeOverlay, props, window, layout } from "herdr";
import { readable, over } from "./contrast.js";

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
const SECTION = { 4: "Needs you", 3: "Done", 2: "Working", 1: "Idle" };
const SECTION_STATUS = { 4: "blocked", 3: "done", 2: "working" };

function duration(ms) {
  const m = Math.floor(ms / 60000);
  if (m < 1) return "<1m";
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  return h < 24 ? `${h}h` : `${Math.floor(h / 24)}d`;
}

// Row geometry: the number column, then the name; sub-agents' tree lines start
// under the number and their names line up with the parent's.
const ROW_PX = 8;
const NUMBER_W = 26;
const GAP = 10;
const NAME_X = ROW_PX + NUMBER_W + GAP;
const SUB_X = ROW_PX + NUMBER_W / 2 - 1;

const spinner = (t) => svg("mods/workspace-chrome/spinner.svg").size(13).flex_none().text_color(t.working).spin(900);

// Status words as the list shows them: error is blocked.
const word = (s) => (s === "error" ? "blocked" : s);

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

  sawDone(a) {
    return a.address === this.held?.address && this.held.rank === RANK.done && a.status === "idle";
  }

  rank(a) {
    return this.sawDone(a) ? RANK.done : RANK[a.status] ?? 0;
  }

  // What the row says the agent is doing, Done hold included.
  word(a) {
    return this.sawDone(a) ? "done" : word(a.status);
  }

  sortMenu(t) {
    const on = sortOf().id;
    const ink = readable(t);
    return v_flex()
      .w_full()
      .p(5)
      .bg(t.popover)
      .text_size(13)
      .child(div().px(8).pt(4).pb(6).text_size(12).text_color(ink.secondary(t.popover)).child("Sort agents"))
      .children(SORTS.map((s) => h_flex()
        .id(`sort-${s.id}`)
        .h(31)
        .px(8)
        .gap(9)
        .rounded(7)
        .items_center()
        .text_color(ink.primary(t.popover))
        .hover((el) => el.bg(t.hover))
        .on_click(() => {
          setShared("agents.sort", s.id);
          closeOverlay(props().overlay);
        })
        .child(svg(`mods/workspace-chrome/${s.icon}.svg`).size(14).flex_none().text_color(ink.secondary(t.popover)))
        .child(div().flex_1().child(s.label))
        .when(s.id === on, (el) => el.child(svg("mods/workspace-chrome/check.svg").size(14).flex_none().text_color(ink.primary(t.popover))))));
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

  // A sub-agent under its parent: the tree lines start under the parent's number,
  // the name lines up with the parent's name.
  subRow(t, ink, s, shown, compact) {
    const k = s.t;
    const on = k.address === shown;
    const bg = on ? t.raised : t.chrome;
    const n = s.lines.length;
    return h_flex()
      .id(`sub-${k.address}`)
      .pl(SUB_X)
      .pr(ROW_PX)
      .rounded(6)
      .h(30)
      .flex_none()
      .items_center()
      .text_size(13)
      .when(on, (el) => el.bg(bg))
      .hover((el) => el.bg(t.hover))
      .on_click(() => this.peek(k.address))
      .children(s.lines.map((l, j) => div().self_start().mt(-4).w(9).h(19).flex_none()
        .mr(j === n - 1 ? Math.max(4, NAME_X - SUB_X - 13 * n + 4) : 4)
        .when(Boolean(l), (el) => el.border_l(1.5).border_color(t.input))
        .when(l === "└" || l === "├", (el) => el.border_b(1.5).rounded_bl(6))))
      .child(div().flex_1().min_w(0).truncate().text_color(on ? ink.primary(bg) : ink.secondary(bg)).child(k.name || k.template || k.program?.name || k.address))
      .child(this.status(t, ink, { status: kidStatus(k, this.listed) }, bg, compact));
  }

  // compact: a ! for blocked where a header already says "Needs you".
  status(t, ink, a, bg, compact = false) {
    const s = word(a.status);
    if (s === "working") return spinner(t);
    if (s === "blocked") {
      const fill = over(t.blocked + "26", ink.solid(bg));
      const badge = div().flex_none().h(20).rounded(5).bg(fill).text_size(12).font_semibold().text_color(ink.tinted(t.blocked, fill)).flex().items_center().justify_center();
      return compact ? badge.w(18).child("!") : badge.px(6).child("Needs you");
    }
    // Idle: a ring; done: a dot.
    if (s === "idle") return div().size(8).mx(3).flex_none().rounded(4).border(1.5).border_color(ink.secondary(bg));
    return div().size(8).mx(3).flex_none().rounded(4).bg(s === "done" ? t.done : "#00000000");
  }

  render(cx) {
    const t = theme();
    if (props()?.sortMenu) return this.sortMenu(t);
    const ink = readable(t);
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
    // Its agent.focus index, the number cmd+N goes by; only 1..9 are shown.
    const position = new Map(list.map((a, i) => [a.address, i + 1]));
    const number = (a) => {
      const n = position.get(a.address);
      return n <= 9 ? String(n) : "";
    };

    const focusFill = over(t.accent + "33", t.chrome);
    // A row's background: its status tinted in (needs you, working, done), else the
    // focus fill, else the sidebar's.
    const tintOf = (a, alpha = "24") => {
      const s = this.word(a);
      return s === "idle" ? null : over(t[s] + alpha, t.chrome);
    };
    const rowBg = (a) => tintOf(a) ?? (a.focused ? focusFill : t.chrome);
    const quiet = (a) => this.word(a) === "idle" && !a.focused;
    const numberColor = (a, bg) => (a.focused ? ink.tinted(t.accent, bg) : this.word(a) === "idle" ? ink.secondary(bg) : ink.tinted(t[this.word(a)], bg));

    // The fold chevron; folded, the count too, colored by the most urgent one.
    const fold = (a, subs, bg) => {
      const sts = subs.map((s) => kidStatus(s.t, this.listed));
      const top = sts.reduce((m, s) => ((RANK[s] ?? 0) > (RANK[m] ?? 0) ? s : m), null);
      const open = !this.folded.has(a.address);
      return div()
        .id(`fold-${a.address}`)
        .h(20)
        .px(3)
        .gap(3)
        .flex()
        .flex_none()
        .items_center()
        .rounded(4)
        .text_size(12)
        .text_color(open || !t[top] ? ink.secondary(bg) : ink.tinted(t[top], bg))
        .cursor_pointer()
        .hover((s) => s.bg(t.hover))
        .on_click((_e, cx) => {
          cx.stop_propagation();
          open ? this.folded.add(a.address) : this.folded.delete(a.address);
          cx.notify();
        })
        .child(svg(`mods/workspace-chrome/chevron-${open ? "down" : "right"}.svg`).size(12).flex_none())
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

    // The rail: the number in its status color, and the status.
    const railRow = (a) => {
      const bg = rowBg(a);
      return h_flex()
        .id(`ag-${a.address}`)
        .h(32)
        .pl(a.focused ? 4 : 6)
        .pr(6)
        .flex_none()
        .items_center()
        .gap(6)
        .bg(bg)
        .when(a.focused, (el) => el.border_l(2).border_color(t.accent))
        .cursor_pointer()
        .hover((s) => s.bg(over(t.hover, bg)))
        .on_click(() => run("pane.focus", { pane: a.pane }))
        .child(div().flex_1().min_w(0).text_right().text_size(15).font_bold().text_color(numberColor(a, bg)).child(number(a)))
        .child(h_flex().w(18).flex_none().items_center().justify_center().child(this.status(t, ink, { status: this.word(a) }, bg, true)));
    };

    // Two lines: the name, then the detail and how long it has been like this.
    const row = (a, subs = []) => {
      const bg = rowBg(a);
      const more = detail.get(a.address);
      // An agent on another machine (`vps/t7`) says which.
      const machine = a.address.includes("/") ? a.address.split("/")[0] : null;
      const when = this.since(a);
      const isPinned = this.pins.includes(a.address);
      const hovered = this.hovered === a.address;
      // Under a space header the space goes without saying; an automatic tab number says nothing.
      // Without a space header above (priority, pinned) the row names its space.
      const full = byPriority || isPinned;
      const name = full ? a.space_name || label(a) : more ?? (/^\d+$/.test(label(a)) ? a.program?.name || label(a) : label(a));
      const second = [machine, full ? more ?? a.program?.name : a.program?.name].filter(Boolean).join(" \u00b7 ");
      const pin = div()
        .id(`pin-${a.address}`)
        .flex_none()
        .p(2)
        .rounded(4)
        .text_color(ink.secondary(bg))
        .cursor_pointer()
        .hover((s) => s.bg(t.hover).text_color(ink.primary(bg)))
        .tooltip(isPinned ? "Unpin" : "Pin to the top")
        .on_click((_e, cx) => {
          cx.stop_propagation();
          this.setPins(isPinned ? this.pins.filter((p) => p !== a.address) : [...this.pins, a.address]);
          // The row moves away from under the pointer, and no hover-out comes.
          this.hovered = null;
          cx.notify();
        })
        .child(svg(`mods/workspace-chrome/${isPinned ? "pin-off" : "pin"}.svg`).size(14));
      return h_flex()
        .id(`ag-${a.address}`)
        .h(54)
        .mb(4)
        .px(a.focused ? ROW_PX - 2 : ROW_PX)
        .gap(GAP)
        .flex_none()
        .rounded(8)
        .items_center()
        .bg(bg)
        .when(a.focused, (el) => el.border(2).border_color(t.accent))
        .cursor_pointer()
        .hover((s) => s.bg(over(t.hover, bg)))
        .on_hover((on, cx) => {
          if (on) this.hovered = a.address;
          else if (this.hovered === a.address) this.hovered = null;
          cx.notify();
        })
        .on_click(() => run("pane.focus", { pane: a.pane }))
        .child(div().w(NUMBER_W).flex_none().text_center().text_size(24).font_weight(a.focused ? 600 : 400).text_color(numberColor(a, bg)).child(number(a)))
        .child(v_flex().flex_1().min_w(0).gap(1)
          .child(h_flex().gap(6).items_center()
            .child(div().flex_1().min_w(0).truncate().text_size(15).font_semibold().text_color(quiet(a) ? ink.secondary(bg) : ink.primary(bg)).child(name))
            .when(subs.length > 0, (el) => el.child(fold(a, subs, bg)))
            .when(hovered, (el) => el.child(pin))
            .when(!hovered && this.word(a) === "working", (el) => el.child(spinner(t))))
          .child(h_flex().gap(6).text_size(12).text_color(ink.secondary(bg))
            .child(div().flex_1().min_w(0).truncate().child(second))
            .when(Boolean(when), (el) => el.child(div().flex_none().child(duration(now - when))))));
    };

    const withKids = (a) => {
      const subs = collapsed ? [] : this.tree(all, a.address);
      const r = collapsed ? railRow(a) : row(a, subs);
      if (this.folded.has(a.address)) return [r];
      return [r, ...subs.map((s) => this.subRow(t, ink, s, shown, byPriority))];
    };

    // Groups in list order: spaces (grouped) or priority sections. Agents of a
    // space are next to each other in agents(), so neighbours make a group.
    const groups = pinned.length ? [{ id: "pinned", label: "Pinned", status: null, members: pinned }] : [];
    for (const a of rest) {
      const r = rankOf(a) || RANK.idle;
      const id = byPriority ? `sec-${r}` : `sp-${a.space}`;
      if (groups.at(-1)?.id === id) groups.at(-1).members.push(a);
      else groups.push({ id, label: byPriority ? SECTION[r] : a.space_name || "", status: byPriority ? SECTION_STATUS[r] : null, members: [a] });
    }

    // A group's header; it folds the group. Folded, it says how many and the most urgent state.
    const header = (g) => {
      const open = !this.folded.has(g.id);
      const top = g.members.reduce((m, a) => ((RANK[a.status] ?? 0) > (RANK[m] ?? 0) ? a.status : m), null);
      return h_flex()
        .id(g.id)
        .px(ROW_PX)
        .pt(8)
        .h(34)
        .flex_none()
        .gap(6)
        .items_center()
        .text_size(12.5)
        .font_medium()
        .cursor_pointer()
        .on_click((_e, cx) => {
          open ? this.folded.add(g.id) : this.folded.delete(g.id);
          cx.notify();
        })
        .child(div().truncate().text_color(g.status ? ink.tinted(t[g.status]) : ink.secondary()).child(g.label))
        .when(byPriority || !open, (el) => el.child(div().text_color(ink.secondary()).child(String(g.members.length))))
        .child(div().flex_1())
        .when(!open, (el) => el
          .when(!byPriority && top && top !== "idle", (el) => el.child(this.status(t, ink, { status: top }, t.chrome, true)))
          .child(svg("mods/workspace-chrome/chevron-right.svg").size(12).flex_none().text_color(ink.secondary())));
    };
    const rows = collapsed
      ? list.flatMap(withKids)
      : groups.flatMap((g) => [header(g), ...(this.folded.has(g.id) ? [] : g.members.flatMap(withKids))]);

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
            .child(div().text_size(12.5).font_medium().text_color(ink.secondary()).child("Agents"))
            .child(
              div()
                .id("ag-sort")
                .text_color(ink.secondary())
                .hover((s) => s.text_color(ink.primary()))
                .on_click(() => openOverlay({ mod: "agents", kind: "popup", width: 200, height: 140, props: { sortMenu: true } }))
                .tooltip(`Sort agents: ${sort.label}`)
                .child(svg(`mods/workspace-chrome/${sort.icon}.svg`).size(14)),
            ),
        ),
      )
      .when(list.length === 0 && !collapsed, (el) =>
        el.child(div().px(12).text_size(12).text_color(ink.secondary()).child("No agent status yet. Agents show up here when they report their status. Older versions and agents inside tmux may not.")),
      )
      .child(v_flex().id("ag-list").flex_1().min_h(0).px(collapsed ? 0 : 8).overflow_y_scrollbar().mode("always").children(rows))
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
                .text_color(ink.secondary())
                .hover((s) => s.text_color(ink.primary()))
                .on_click(toggleSidebar)
                .child("»"),
            ),
        ),
      );
  }
}
