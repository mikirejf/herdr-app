// A browser pane's header (the pane mod's, for a pane with PaneInfo.browser):
// back, forward, reload, the address, zoom, the viewport, open in the default
// browser, DevTools. Only commands any mod may run. The address is what
// browsers() gives. Enter
// goes there and hands the keyboard back to the page; Esc puts the page's
// address back. Zoom (- NN% +, the percent resets it) and the viewport button
// (Fit, Phone, Tablet in turn) show while the header is hovered, or while they
// aren't at their defaults; open in the default browser shows on hover.
// A narrow bar keeps the address's room: no - and +, the zoom as a bare
// number only when it isn't 100, the viewport as its icon only. Only the
// pointer reports the bar's width, so a bar counts as narrow until the
// pointer first crosses it.
// Above the bar, a row of tabs (TAB_ROW tall; the app makes a browser pane's
// header that much taller): each shows its title, an x on hover and on the
// shown tab, and a + opens a new one; a "..." at the row's right end opens the
// browser menu (browser-menu.js). A new empty tab gives the address field the keyboard.

import { div, svg } from "gpui-kit";
import { h_flex, InputState, Input } from "gpui-base";
import { browsers, run, openOverlay } from "herdr";
import { MENU_WIDTH, MENU_HEIGHT } from "./browser-menu.js";

/** What the person typed, as a url: with a scheme as is; a host gets https://
 *  (http:// for localhost and 127.0.0.1); anything else is a search. */
export function address(text) {
  const s = text.trim();
  if (!s) return null;
  const search = () => "https://duckduckgo.com/?q=" + encodeURIComponent(s);
  if (/\s/.test(s)) return search();
  if (/^[a-z][a-z0-9+.-]*:(?!\d)/i.test(s)) return s;
  const host = s.split(/[/?#]/)[0].replace(/:\d+$/, "").toLowerCase();
  if (host === "localhost" || host === "127.0.0.1") return "http://" + s;
  if (host.includes(".")) return "https://" + s;
  return search();
}

/** The tab row's height (px): the app adds it to a browser pane's header (BROWSER_TABS_HEIGHT in window.rs). */
export const TAB_ROW = 28;

/** What a tab shows: its title; before it has one, the host it loads; "New tab" when empty. */
export function tabTitle(tab) {
  if (!tab.url) return "New tab";
  if (tab.title) return tab.title;
  const host = /^[a-z][a-z0-9+.-]*:\/\/(?:[^/?#@]*@)?([^/?#]*)/i.exec(tab.url)?.[1];
  return host || tab.url;
}

/** Below this bar width (px) the bar is narrow: no - and +, no "%", no viewport name. */
export const NARROW = 340;

/** The least width (px) the address field keeps. */
const ADDRESS_MIN = 64;

/** The viewport button's order: each click moves to the next. */
const VIEWPORTS = ["fit", "phone", "tablet"];

/** The preset after `viewport` (BrowserInfo.viewport); a custom size goes back to fit. */
export function nextViewport(viewport) {
  const now = viewport ? viewport.preset : "fit";
  const i = VIEWPORTS.indexOf(now);
  return i < 0 ? "fit" : VIEWPORTS[(i + 1) % VIEWPORTS.length];
}

/** What the viewport button says: nothing at fit, the preset, or the size. */
export function viewportLabel(viewport) {
  if (!viewport) return null;
  if (viewport.preset) return viewport.preset[0].toUpperCase() + viewport.preset.slice(1);
  return `${viewport.width}×${viewport.height}`;
}

export default class BrowserBar {
  // Created from the pane mod's init: state can't be made during render.
  constructor() {
    this.input = InputState.new({ placeholder: "Search or enter an address" });
    this.editing = false;
    this.shown = null; // the url the field holds while nobody edits it
    this.narrow = true; // narrower than NARROW when last measured; unmeasured counts as narrow
    this.hot = -1; // the tab the pointer is over
    this.blank = null; // the last empty tab that got the keyboard's offer (its id)
    this.input.on("focus", (_e, cx) => {
      this.editing = true;
      this.input.set_selected_range({ start: 0, end: this.input.value().length });
      cx.notify();
    });
    this.input.on("blur", (_e, cx) => { this.editing = false; this.shown = null; cx.notify(); });
    this.input.on("submit", () => this.go());
  }

  go() {
    const url = address(this.input.value());
    if (url) run("browser.navigate", { browser: this.browser, url });
    this.leave();
  }

  // The keyboard goes back to the page; the field shows the page's address again.
  leave() {
    this.shown = null;
    run("pane.focus", { pane: this.pane });
  }

  key(e, cx) {
    if (e.key !== "escape" || !this.editing) return;
    cx.stop_propagation();
    this.input.set_value(this.url);
    this.leave();
  }

  // The field follows the page while nobody edits it (set after render: state can't change during it).
  follow(url, cx) {
    if (this.editing || this.shown === url) return;
    this.shown = url;
    cx.timer.after(0, () => { if (!this.editing && this.shown === url) this.input.set_value(url); });
  }

  // Gives the address field the keyboard when an empty tab shows (a new tab), once per tab.
  offer(b, cx) {
    const tab = b?.tabs?.[b.active];
    if (!tab || tab.url) return;
    if (tab.id === this.blank) return;
    this.blank = tab.id;
    if (b.focused) cx.timer.after(0, () => this.input.focus());
  }

  tab(t, pane, x, i, on) {
    const stop = (_e, cx) => cx.stop_propagation();
    return h_flex()
      .id(`pt-${pane}-${i}`)
      .flex_1()
      .min_w(0)
      .max_w(180)
      .h(22)
      .pl(8)
      .pr(4)
      .gap(6)
      .items_center()
      .rounded(6)
      .border(1)
      .border_color(on ? t.border : t.background)
      .bg(on ? t.raised : t.background)
      .text_size(11.5)
      .text_color(on ? t.foreground : t.muted)
      .hover((s) => s.bg(on ? t.raised : t.hover))
      .on_hover((h, cx) => {
        if (h && this.hot !== i) { this.hot = i; cx.notify(); }
        else if (!h && this.hot === i) { this.hot = -1; cx.notify(); }
      })
      .on_mouse_down("left", (_e, cx) => {
        cx.stop_propagation();
        run("pane.focus", { pane });
        if (!on) run("browser.tab.select", { browser: this.browser, index: i });
      })
      .when(Boolean(x.loading), (el) => el.child(svg("mods/workspace-chrome/spinner.svg").size(10).flex_none().text_color(t.dim).spin(900)))
      .child(div().flex_1().min_w(0).truncate().child(tabTitle(x)))
      .when(on || this.hot === i, (el) => el.child(
        div()
          .id(`pt-x-${pane}-${i}`)
          .flex_none()
          .p(2)
          .rounded(4)
          .text_color(t.dim)
          .hover((s) => s.bg(t.hover).text_color(t.foreground))
          .on_mouse_down("left", stop)
          .on_click((_e, cx) => { cx.stop_propagation(); run("browser.tab.close", { browser: this.browser, index: i }); })
          .child(svg("mods/pane/close.svg").size(12))));
  }

  // The row of tabs above the bar; null where the app has no browser (the pane has no tabs).
  tabs(t, p) {
    const b = browsers().find((x) => x.browser === p.browser);
    const tabs = b?.tabs ?? p.tabs;
    if (!tabs) return null;
    const active = b?.active ?? p.active ?? 0;
    const pane = p.pane;
    this.browser = p.browser;
    let row = h_flex()
      .id(`pt-${pane}`)
      .flex_none()
      .w_full()
      .h(TAB_ROW)
      .pl(6)
      .pr(6)
      .gap(3)
      .items_center();
    tabs.forEach((x, i) => { row = row.child(this.tab(t, pane, x, i, i === active)); });
    return row.child(this.plus(t, p)).child(this.more(t, p));
  }

  // The "..." button: opens the browser menu under it, its right edge on the button's.
  more(t, p) {
    const pane = p.pane;
    return div()
      .id(`pt-more-${pane}`)
      .flex_none()
      // What the tabs leave free goes left of it.
      .ml_auto()
      .p(4)
      .rounded(5)
      .text_color(t.dim)
      .hover((s) => s.bg(t.hover).text_color(t.foreground))
      .on_mouse_down("left", (e, cx) => {
        cx.stop_propagation();
        this.moreBounds = e.bounds;
      })
      .on_click((_e, cx) => {
        cx.stop_propagation();
        const b = this.moreBounds;
        if (!b) return;
        run("pane.focus", { pane });
        openOverlay({
          mod: "pane",
          kind: "popup",
          width: MENU_WIDTH,
          height: MENU_HEIGHT,
          at: { x: b.x + b.width - MENU_WIDTH, y: b.y + b.height + 2 },
          props: { menu: "browser", browser: p.browser },
        });
      })
      .child(svg("mods/pane/more.svg").size(14));
  }

  // The + button: a new tab after the shown one.
  plus(t, p) {
    const pane = p.pane;
    return div()
      .id(`pt-new-${pane}`)
      .flex_none()
      .p(4)
      .rounded(5)
      .text_color(t.dim)
      .hover((s) => s.bg(t.hover).text_color(t.foreground))
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .on_click((_e, cx) => {
        cx.stop_propagation();
        run("pane.focus", { pane });
        run("browser.tab.new", { browser: p.browser });
      })
      .child(svg("mods/pane/plus.svg").size(14));
  }

  button(t, id, icon, command, on, args = {}) {
    const b = div().id(id).p(3).rounded(4).flex_none();
    if (!on) return b.text_color(t.dim).opacity(0.4).child(svg(`mods/pane/${icon}.svg`).size(14));
    return b
      .text_color(t.dim)
      .hover((s) => s.bg(t.hover).text_color(t.foreground))
      .on_click((_e, cx) => {
        cx.stop_propagation();
        run(command, { browser: this.browser, ...args });
      })
      .child(svg(`mods/pane/${icon}.svg`).size(14));
  }

  // The percent (click: back to 100%); a narrow bar drops the "%".
  percent(t, pane, zoom) {
    return div()
      .id(`pb-zoom-${pane}`)
      .flex_none()
      .px(3)
      .rounded(4)
      .text_size(11)
      .text_color(zoom === 100 ? t.dim : t.foreground)
      .hover((s) => s.bg(t.hover))
      .on_click((_e, cx) => {
        cx.stop_propagation();
        run("browser.zoom", { browser: this.browser, step: "reset" });
      })
      .child(this.narrow ? `${zoom}` : `${zoom}%`);
  }

  // Fit, Phone, Tablet in turn; its label says which, unless it's fit.
  viewport(t, pane, viewport) {
    const label = this.narrow ? null : viewportLabel(viewport);
    return h_flex()
      .id(`pb-vp-${pane}`)
      .flex_none()
      .gap(2)
      .items_center()
      .p(3)
      .rounded(4)
      .text_color(viewport ? t.accent : t.dim)
      .hover((s) => s.bg(t.hover).text_color(t.foreground))
      .on_click((_e, cx) => {
        cx.stop_propagation();
        run("browser.viewport", { browser: this.browser, preset: nextViewport(viewport) });
      })
      .child(svg("mods/pane/viewport.svg").size(14))
      .when(Boolean(label), (el) => el.child(div().text_size(11).child(label)));
  }

  render(t, p, cx, hovered) {
    const b = browsers().find((x) => x.browser === p.browser);
    this.browser = p.browser;
    this.pane = p.pane;
    this.url = b?.url ?? p.url ?? "";
    this.follow(this.url, cx);
    this.offer(b, cx);
    const pane = p.pane;
    const zoom = b?.zoom ?? 100;
    const viewport = b?.viewport ?? null;
    return h_flex()
      .id(`pb-${pane}`)
      .flex_1()
      .min_w(0)
      .gap(2)
      .items_center()
      // Only the pointer reports the bar's size: measured while it's over the bar.
      .on_mouse_move((e, cx) => {
        const narrow = e.bounds.width < NARROW;
        if (narrow !== this.narrow) { this.narrow = narrow; cx.notify(); }
      })
      .child(this.button(t, `pb-back-${pane}`, "back", "browser.back", b?.can_go_back))
      .child(this.button(t, `pb-fwd-${pane}`, "forward", "browser.forward", b?.can_go_forward))
      .child(this.button(t, `pb-reload-${pane}`, "reload", "browser.reload", Boolean(b)))
      .child(div()
        .id(`pb-url-${pane}`)
        .flex_1()
        // A floor for the address: in a very narrow pane what's right of it clips first.
        .min_w(ADDRESS_MIN)
        .h(22)
        .mx(4)
        .px(8)
        .flex()
        .items_center()
        .rounded(6)
        .bg(t.raised)
        .border_1()
        .border_color(this.editing ? t.accent : t.border)
        .cursor_text()
        // The header's click focuses the pane, which would take the keyboard from the field.
        .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
        .on_key_down((e, cx) => this.key(e, cx))
        .child(Input.new(this.input).w_full().h_full().text_size(12).text_color(t.foreground)))
      .when(Boolean(b?.loading), (el) => el.child(svg("mods/workspace-chrome/spinner.svg").size(12).flex_none().text_color(t.dim).spin(900)))
      .when(Boolean(b) && hovered && !this.narrow, (el) => el.child(this.button(t, `pb-zout-${pane}`, "zoom-out", "browser.zoom", zoom > 25, { step: "out" })))
      .when(Boolean(b) && (this.narrow ? zoom !== 100 : hovered || zoom !== 100), (el) => el.child(this.percent(t, pane, zoom)))
      .when(Boolean(b) && hovered && !this.narrow, (el) => el.child(this.button(t, `pb-zin-${pane}`, "zoom-in", "browser.zoom", zoom < 500, { step: "in" })))
      .when(Boolean(b) && (hovered || Boolean(viewport)), (el) => el.child(this.viewport(t, pane, viewport)))
      // Pages Herdr's browser can't sign into (passkeys) open in the default browser.
      .when(Boolean(b) && hovered && !this.narrow, (el) => el.child(this.button(t, `pb-ext-${pane}`, "external", "browser.open_external", /^https?:\/\//i.test(b.url))))
      .child(this.button(t, `pb-dev-${pane}`, "devtools", "browser.devtools", Boolean(b)));
  }
}
