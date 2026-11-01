// The "..." menu of a browser pane's tab row (opened by browser.js as a popup of
// this mod with props { menu: "browser", browser }): Save screenshot, the Cookies
// section, Clear browsing data (after a confirmation). Up/Down move, Enter runs,
// Esc closes, as in the menu mod.

import { div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { run, props, closeOverlay, openOverlay } from "herdr";

/** The popup's size (px): browser.js places it under its button. */
export const MENU_WIDTH = 210;
export const MENU_HEIGHT = 112;

/** What the menu lists, in order: a row (label, run, danger) or a dim section label. */
export function menuItems(browser) {
  return [
    { label: "Save screenshot", run: () => run("browser.screenshot", { browser }) },
    { section: "Cookies" },
    {
      label: "Clear browsing data",
      danger: true,
      run: () =>
        openOverlay({
          mod: "confirm",
          kind: "dialog",
          width: 380,
          height: 150,
          props: {
            title: "Clear browsing data?",
            detail: "You'll be signed out of every site in the Herdr browser.",
            command: { name: "browser.clear_data", args: {} },
          },
        }),
    },
  ];
}

export default class BrowserMenu {
  constructor(cx) {
    this.at = 0;
    this.handle = cx.focus_handle();
    this.handle.focus();
  }

  rows() {
    return menuItems(props()?.browser).filter((it) => !it.section);
  }

  choose(i) {
    const it = this.rows()[i];
    if (!it) return;
    closeOverlay(props()?.overlay);
    it.run();
  }

  render(t) {
    const rows = this.rows();
    let row = -1;
    return v_flex()
      .id("browser-menu")
      .track_focus(this.handle)
      .w_full()
      .min_h(0)
      .p(5)
      .bg(t.popover)
      .text_size(12.8)
      .on_key_down((e, cx) => {
        if (e.key === "down") this.at = (this.at + 1) % rows.length;
        else if (e.key === "up") this.at = (this.at + rows.length - 1) % rows.length;
        else if (e.key === "enter") this.choose(this.at);
        else if (e.key === "escape") closeOverlay(props().overlay);
        else return;
        cx.notify();
      })
      .children(
        menuItems(props()?.browser).map((it) => {
          if (it.section) return div().px(8).pt(8).pb(2).text_size(11).text_color(t.dim).child(it.section);
          const i = ++row;
          return h_flex()
            .id(`bm-${i}`)
            .h(31)
            .px(8)
            .rounded(7)
            .items_center()
            .text_color(it.danger ? t.ansi1 : t.foreground)
            .when(i === this.at, (el) => el.bg(t.selected))
            .hover((s) => s.bg(t.hover))
            .on_click((_e, cx) => { cx.stop_propagation(); this.choose(i); })
            .child(it.label);
        }),
      );
  }
}
