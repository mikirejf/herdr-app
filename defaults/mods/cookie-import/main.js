// Import cookies from another browser on this Mac, in two dialogs: pick a
// browser profile (step 1), then the sites to bring over (step 2). Opened as
// a modal dialog by the browser pane's "..." menu (`mod.open cookie-import`
// works too). The lists come from the app's own binary (`__cookie-sources`,
// `__cookie-sites`, JSON on stdout); nothing is read until Import is pressed in
// step 2, which runs browser.import_cookies. Each step is its own dialog, so
// each has its size: props { step, sources, source (an id), sites } carry
// the state between them.

import { View, div, svg } from "gpui-kit";
import { v_flex, h_flex, InputState, Input } from "gpui-base";
import { run, exec, props, closeOverlay, openOverlay, theme } from "herdr";

const I = (name) => `mods/workspace-chrome/${name}.svg`;

const STEP1 = { width: 440, height: 270 };
const STEP2 = { width: 460, height: 540 };

const HELP =
  "You'll pick which sites to bring over. Nothing leaves this computer. For Chrome, Brave, Arc and Edge, macOS may ask for your Mac login password to unlock their cookies: enter it and choose Allow.";

/** Runs one of the app's cookie subcommands and parses its JSON. */
async function ask(verb, ...args) {
  const r = await exec("sh", ["-c", `"$HERDR_APP_EXE" ${verb} "$@"`, "sh", ...args]);
  if (r.code !== 0) throw new Error(r.stderr.trim() || "it didn't answer");
  return JSON.parse(r.stdout);
}

export default class CookieImport extends View {
  init(_props, cx) {
    const p = props() ?? {};
    this.step = p.step === 2 ? 2 : 1;
    this.handle = cx.focus_handle();
    this.error = null;
    this.busy = false;
    if (this.step === 1) {
      this.sources = p.sources ?? null;
      this.sourceId = p.source ?? null;
      this.listOpen = false;
      this.handle.focus();
      if (!this.sources) {
        ask("__cookie-sources").then(
          (sources) => { this.sources = sources; this.sourceId ??= sources[0]?.id ?? null; cx.notify(); },
          (e) => { this.sources = []; this.error = String(e.message ?? e); cx.notify(); },
        );
      } else this.sourceId ??= this.sources[0]?.id ?? null;
    } else {
      this.sites = p.sites ?? [];
      this.checked = new Set(this.sites.map((s) => s.domain));
      this.filter = "";
      this.input = InputState.new({ placeholder: "Filter sites" });
      this.input.on("change", (_e, cx) => { this.filter = this.input.value(); cx.notify(); });
      this.input.on("submit", (_e, cx) => this.doImport());
      this.inputFocused = true;
      this.input.on("focus", (_e, cx) => { this.inputFocused = true; cx.notify(); });
      this.input.on("blur", (_e, cx) => { this.inputFocused = false; cx.notify(); });
      this.input.focus();
      cx.timer.after(600, () => this.input.focus());
    }
  }

  close() {
    closeOverlay(props().overlay);
  }

  source() {
    return (this.sources ?? []).find((s) => s.id === this.sourceId);
  }

  // Step 1 -> 2: the chosen profile's sites (reads no cookie values).
  next(cx) {
    const source = this.source();
    if (!source || this.busy) return;
    this.busy = true;
    this.error = null;
    cx.notify();
    ask("__cookie-sites", source.id).then(
      (sites) => {
        this.close();
        openOverlay({ mod: "cookie-import", kind: "dialog", modal: true, ...STEP2, props: { step: 2, sources: this.sources, source, sites } });
      },
      (e) => { this.busy = false; this.error = `Couldn't read ${source.browser}'s sites: ${e.message ?? e}`; cx.notify(); },
    );
  }

  back() {
    const p = props();
    this.close();
    openOverlay({ mod: "cookie-import", kind: "dialog", modal: true, ...STEP1, props: { step: 1, sources: p.sources, source: p.source.id } });
  }

  shown() {
    const f = this.filter.trim().toLowerCase();
    return f ? this.sites.filter((s) => s.domain.toLowerCase().includes(f)) : this.sites;
  }

  doImport() {
    if (this.checked.size === 0) return;
    const sites = this.sites.filter((s) => this.checked.has(s.domain)).map((s) => s.domain);
    this.close();
    run("browser.import_cookies", { source: props().source.id, sites });
  }

  toggleAll(shown) {
    const all = shown.length > 0 && shown.every((s) => this.checked.has(s.domain));
    for (const s of shown) all ? this.checked.delete(s.domain) : this.checked.add(s.domain);
  }

  box(t, on) {
    return div()
      .flex_none()
      .size(16)
      .rounded(4)
      .border_1()
      .border_color(on ? t.solid : t.input)
      .bg(on ? t.solid : t.background)
      .flex()
      .items_center()
      .justify_center()
      .when(on, (el) => el.child(svg(I("check")).size(11).text_color(t.solid_foreground)));
  }

  button(t, id, label, primary, enabled, onClick) {
    return h_flex()
      .id(id)
      .h(32)
      .px(14)
      .items_center()
      .rounded(8)
      .border_1()
      .border_color(primary ? t.solid : t.input)
      .bg(primary ? t.solid : t.raised)
      .text_color(primary ? t.solid_foreground : t.foreground)
      .text_size(12)
      .when(enabled, (el) => el.cursor_pointer().hover((s) => s.opacity(0.88)).on_click(onClick))
      .when(!enabled, (el) => el.opacity(0.4))
      .child(label);
  }

  title(t, text) {
    return h_flex()
      .items_center()
      .child(div().flex_1().text_size(15).font_semibold().text_color(t.foreground).child(text))
      .child(
        div()
          .id("ci-close")
          .size(24)
          .rounded(6)
          .flex()
          .items_center()
          .justify_center()
          .cursor_pointer()
          .hover((s) => s.bg(t.hover))
          .on_click(() => this.close())
          .child(svg(I("x")).size(13).text_color(t.muted)),
      );
  }

  frame(t, id, onKey) {
    return v_flex()
      .id(id)
      .track_focus(this.handle)
      .on_key_down((e, cx) => {
        if (e.key === "escape") this.close();
        else if (!onKey(e)) return;
        cx.stop_propagation();
      })
      .size_full()
      .min_h(0)
      .p(18)
      .gap(12)
      .bg(t.popover)
      .text_color(t.foreground)
      .text_size(13);
  }

  render(cx) {
    return this.step === 1 ? this.pick(cx) : this.choose(cx);
  }

  option(t, s) {
    return h_flex().gap(8).items_center().min_w(0).child(div().flex_none().child(s.browser)).child(div().truncate().text_color(t.dim).child(s.profile));
  }

  pick(cx) {
    const t = theme();
    const sources = this.sources;
    const source = this.source();
    const none = sources && sources.length === 0;
    const body = v_flex().flex_1().min_h(0).gap(12);
    if (sources === null) body.child(div().text_color(t.dim).child("Looking for browsers…"));
    else if (none && !this.error) {
      body.child(div().line_height("19px").text_color(t.muted).child("None of Chrome, Brave, Arc, Edge or Firefox has cookies on this Mac."));
    } else if (!none) {
      body
        .child(
          v_flex()
            .gap(6)
            .child(div().text_size(12).text_color(t.dim).child("From"))
            .child(
              div()
                .relative()
                .child(
                  h_flex()
                    .id("ci-select")
                    .h(34)
                    .px(10)
                    .gap(8)
                    .items_center()
                    .rounded(8)
                    .border_1()
                    .border_color(this.listOpen ? t.accent : t.input)
                    .bg(t.background)
                    .cursor_pointer()
                    .on_click((_e, cx) => { this.listOpen = !this.listOpen; cx.notify(); })
                    .child(div().flex_1().min_w(0).child(source ? this.option(t, source) : ""))
                    .child(svg(I("chevron-down")).size(13).text_color(t.muted)),
                )
                .when(this.listOpen, (el) =>
                  el.child(
                    v_flex()
                      .id("ci-options")
                      .absolute()
                      .left(0)
                      .right(0)
                      .top(38)
                      .max_h(170)
                      .p(4)
                      .rounded(8)
                      .border_1()
                      .border_color(t.border)
                      .bg(t.popover)
                      .overflow_y_scrollbar()
                      .children(
                        sources.map((s, i) =>
                          h_flex()
                            .id(`ci-opt-${i}`)
                            .h(30)
                            .px(8)
                            .rounded(6)
                            .items_center()
                            .when(s.id === this.sourceId, (e) => e.bg(t.selected))
                            .hover((st) => st.bg(t.hover))
                            .cursor_pointer()
                            .on_click((_e, cx) => { this.sourceId = s.id; this.listOpen = false; cx.notify(); })
                            .child(this.option(t, s)),
                        ),
                      ),
                  ),
                ),
            ),
        )
        .child(div().text_size(12).line_height("18px").text_color(t.dim).child(HELP));
    }
    if (this.error) body.child(div().text_size(12).text_color(t.ansi1).child(this.error));
    return this.frame(t, "cookie-import", (e) => {
      if (e.key !== "enter") return false;
      this.next(cx);
      return true;
    })
      .child(this.title(t, "Import cookies from your browser"))
      .child(body)
      .child(
        h_flex()
          .justify_end()
          .gap(8)
          .child(none ? this.button(t, "ci-cancel", "Close", false, true, () => this.close()) : this.button(t, "ci-cancel", "Cancel", false, true, () => this.close()))
          .when(!none, (el) => el.child(this.button(t, "ci-import", this.busy ? "Reading…" : "Import", true, Boolean(source) && !this.busy, (_e, cx) => this.next(cx)))),
      );
  }

  choose(cx) {
    const t = theme();
    const source = props().source;
    const shown = this.shown();
    const allShown = shown.length > 0 && shown.every((s) => this.checked.has(s.domain));
    return this.frame(t, "cookie-sites", () => false)
      .child(this.title(t, "Choose sites to bring over"))
      .child(div().text_size(12).text_color(t.dim).child(`Sites with cookies in your ${source.browser} profile “${source.profile}”.`))
      .child(
        Input.new(this.input)
          .h(34)
          .flex_none()
          .px(10)
          .items_center()
          .rounded(8)
          .border_1()
          .border_color(this.inputFocused ? t.accent : t.input)
          .bg(t.background)
          .text_size(13)
          .text_color(t.foreground),
      )
      .child(
        h_flex()
          .id("ci-all")
          .flex_none()
          .h(30)
          .px(8)
          .gap(10)
          .items_center()
          .rounded(6)
          .cursor_pointer()
          .hover((s) => s.bg(t.hover))
          .on_click((_e, cx) => { this.toggleAll(shown); cx.notify(); })
          .child(this.box(t, allShown))
          .child("Select all"),
      )
      .child(
        v_flex()
          .id("ci-sites")
          .flex_1()
          .min_h(0)
          .overflow_y_scrollbar()
          .rounded(8)
          .border_1()
          .border_color(t.border)
          .p(4)
          .children(
            shown.length === 0
              ? [div().p(10).text_color(t.dim).child("No site matches")]
              : shown.map((s, i) => {
                  const on = this.checked.has(s.domain);
                  return h_flex()
                    .id(`ci-site-${i}`)
                    .flex_none()
                    .h(30)
                    .px(8)
                    .gap(10)
                    .items_center()
                    .rounded(6)
                    .cursor_pointer()
                    .hover((st) => st.bg(t.hover))
                    .on_click((_e, cx) => { on ? this.checked.delete(s.domain) : this.checked.add(s.domain); cx.notify(); })
                    .child(this.box(t, on))
                    .child(div().flex_1().min_w(0).truncate().child(s.domain))
                    .child(div().flex_none().text_size(11.5).text_color(t.dim).child(String(s.count)));
                }),
          ),
      )
      .child(
        h_flex()
          .flex_none()
          .items_center()
          .child(div().flex_1().text_size(12).text_color(t.dim).child(`${this.checked.size} of ${this.sites.length} selected`))
          .child(
            h_flex()
              .gap(8)
              .child(this.button(t, "ci-back", "Back", false, true, () => this.back()))
              .child(this.button(t, "ci-go", "Import", true, this.checked.size > 0, () => this.doImport())),
          ),
      );
  }
}
