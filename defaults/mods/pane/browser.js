// A browser pane's header (the pane mod's, for a pane with PaneInfo.browser):
// back, forward, reload, the address, zoom, the viewport, open in the default
// browser, DevTools. Only commands any mod may run. The address is what
// browsers() gives (no query or fragment unless agent access is on). Enter
// goes there and hands the keyboard back to the page; Esc puts the page's
// address back. Zoom (- NN% +, the percent resets it) and the viewport button
// (Fit, Phone, Tablet in turn) show while the header is hovered, or while they
// aren't at their defaults; open in the default browser shows on hover.
// A narrow bar keeps the address's room: no - and +, the zoom as a bare
// number only when it isn't 100, the viewport as its icon only. Only the
// pointer reports the bar's width, so a bar counts as narrow until the
// pointer first crosses it.

import { div, svg } from "gpui-kit";
import { h_flex, InputState, Input } from "gpui-base";
import { browsers, run } from "herdr";

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
