import { View, div, svg } from "gpui-kit";
import { h_flex } from "gpui-base";
import { spaces, agents, theme, run, window, keys } from "herdr";

export default class WorkspaceStatus extends View {
  // Keys as this platform writes them: "cmd-shift-p" is ⌘⇧P, "ctrl-space" ⌃Space, "?" stays "?".
  keyLabel(keys) {
    const mac = window()?.platform === "macos";
    const mod = { cmd: mac ? "⌘" : "Ctrl+", ctrl: mac ? "⌃" : "Ctrl+", shift: mac ? "⇧" : "Shift+", alt: mac ? "⌥" : "Alt+", option: mac ? "⌥" : "Alt+" };
    return keys.split(" ").map((stroke) => {
      const [, mods, key] = stroke.match(/^((?:[a-z]+-)*)(.+)$/i);
      const held = mods.split("-").filter(Boolean);
      const name = key.length > 1 ? key[0].toUpperCase() + key.slice(1) : held.length ? key.toUpperCase() : key;
      return held.map((p) => mod[p.toLowerCase()] ?? p).join("") + name;
    }).join(" ");
  }

  // The palette key as actually bound, not a hardcoded one; without a
  // binding the hint stays useful ("commands") instead of lying.
  hint(t) {
    const b = keys().find((k) => k.section === "global" && k.action === "app.palette");
    return b ? `${this.keyLabel(b.keys)}  commands` : "commands";
  }

  render() {
    const t = theme();
    const space = spaces().find(s => s.active);
    const list = agents();
    const needs = list.filter(a => a.status === "blocked").length;
    const label = (icon, text) => h_flex().gap(6).items_center().child(svg(`mods/workspace-chrome/${icon}.svg`).size(13)).child(text);
    // A multi-key shortcut is half typed (ctrl-b, waiting for z): as Herdr's PREFIX mode.
    const pending = window()?.pending_keys ?? [];
    const zoomed = space?.tabs.find(tb => tb.active)?.zoomed;
    const badge = text => div().px(6).rounded(3).bg(t.accent).text_color(t.chrome).font_weight(700).child(text);
    const hints = pairs => pairs.map(([k, what]) => h_flex().gap(4).child(div().text_color(t.foreground).child(k)).child(what));
    // What the next key can do, as Herdr's prefix bar: only keys actually bound after this prefix.
    const typed = pending.join(" ");
    const next = action => keys().find(k => k.section === "global" && k.action === action
      && k.keys.startsWith(typed + " ") && !k.keys.slice(typed.length + 1).includes(" "));
    const prefixHints = pending.length === 0 ? [] : [["esc", "cancel"], [this.keyLabel(typed), "send prefix"],
      ...[["pane.resize_mode", "resize"], ["app.help", "keybinds"]].flatMap(([action, what]) => {
        const b = next(action);
        return b ? [[b.keys.slice(typed.length + 1), what]] : [];
      })];
    return h_flex().size_full().px(14).gap(16).items_center().bg(t.chrome).border_t(1).border_color(t.border).text_size(11.8).text_color(t.muted)
      .when(pending.length > 0, el => el.child(h_flex().gap(6).items_center()
        .child(badge("PREFIX"))
        .children(hints(prefixHints))))
      .when(Boolean(window()?.resize_mode), el => el.child(h_flex().gap(6).items_center()
        .child(badge("RESIZE"))
        .children(hints([["h/l", "width"], ["j/k", "height"], ["esc", "done"]]))))
      .when(Boolean(zoomed), el => el.child(badge("ZOOM")))
      .child(label(space?.machine ? "server" : "monitor", space?.machine ?? "This computer"))
      .when(Boolean(space?.branch), el => el.child(label("branch", space.branch)))
      .child(h_flex().gap(6).items_center().child(div().size(6).rounded(3).bg(t.dim)).child(`${list.length} agents`)
        .when(needs > 0, el => el.child(div().text_color(t.blocked).child(`· ${needs} needs you`))))
      .child(div().flex_1())
      .child(div().id("palette").text_color(t.dim).on_click(() => run("app.palette")).child(this.hint(t)));
  }
}
