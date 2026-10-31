//@@ 0
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
//@@ 1
    LISTPX = 10;
    rows = cards((a) => {
      const f = a.focused, bg = f ? over(t.accent + "40", t.raised) : quiet(a) ? mix(t.raised, t.chrome, 0.45) : t.raised;
      return card(a, bg, h_flex().gap(8).items_center().child(numeral(a, bg, statusNum(a, bg))).child(two(a, bg)), scol(a));
    });
//@@ 2
    rows = sectioned((a) => {
      const f = a.focused, tint = tintOf(a), bg = tint ?? (f ? FOC : t.chrome);
      return click(h_flex(), a).h(54).mb(4).px(f ? 6 : 8).gap(10).flex_none().items_center().rounded(8).bg(bg)
        .when(f, (e) => e.border(2).border_color(t.accent)).when(!f, (e) => e.hover((s) => s.bg(tint ? over(scol(a) + "38", t.chrome) : t.hover)))
        .child(numeral(a, bg, statusNum(a, bg))).child(two(a, bg));
    }, (s) => subLine(s, 20, 44));
//@@ 3
    LISTPX = 0;
    rows = flat((a) => {
      const f = a.focused, bg = f ? FOC : t.chrome, subs = kids(a);
      return click(h_flex(), a).h(38).pl(11).pr(14).gap(9).flex_none().items_center().border_l(3).border_color(f ? t.accent : "#00000000")
        .when(f, (e) => e.bg(FOC)).when(!f, (e) => e.hover((s) => s.bg(t.hover)))
        .child(dot(a, bg))
        .child(h_flex().flex_1().min_w(0).gap(6).items_center()
          .child(div().flex_none().text_size(15).font_medium().text_color(quiet(a) ? S(bg) : P(bg)).child(nm(a)))
          .child(div().flex_1().min_w(0).truncate().text_size(12).text_color(S(bg)).child(more(a) ?? "")))
        .when(subs.length > 0, (e) => e.child(fold(a, subs)))
        .child(div().flex_none().text_size(12).font_family(mono()).text_color(S(bg)).child(dur(a)))
        .child(div().w(16).flex_none().text_right().text_size(19).font_bold().font_family(mono()).text_color(quiet(a) ? C(mix(t.accent, t.chrome, 0.7), bg) : C(t.accent, bg)).child(NS(a)));
    }, (s) => subLine(s, 16, 33));
//@@ 4
    LISTPX = 10;
    rows = cards((a) => {
      const f = a.focused, tint = scol(a) ? over(scol(a) + "2e", t.raised) : null;
      const bg = f ? over(t.accent + "40", tint ?? t.raised) : tint ?? (quiet(a) ? mix(t.raised, t.chrome, 0.45) : t.raised);
      return card(a, bg, h_flex().gap(9).items_center()
        .child(dot(a, bg))
        .child(two(a, bg))
        .child(div().w(16).flex_none().text_right().text_size(19).font_semibold().font_family(mono()).text_color(f ? C(t.accent, bg) : quiet(a) ? S(bg) : P(bg)).child(NS(a))), null);
    });
//@@ 5
    LISTPX = 0;
    rows = flat((a) => {
      const f = a.focused, tint = tintOf(a, "18"), bg = f ? over(t.accent + "30", tint ?? t.chrome) : tint ?? t.chrome;
      return click(h_flex(), a).h(56).mb(2).pl(9).pr(14).gap(10).flex_none().items_center().bg(bg).border_l(3).border_color(f ? t.accent : "#00000000")
        .when(!f, (e) => e.hover((s) => s.bg(over(t.hover, bg))))
        .child(numeral(a, bg, quiet(a) ? C(mix(t.accent, t.chrome, 0.7), bg) : C(t.accent, bg))).child(two(a, bg));
    }, (s) => subLine(s, 22, 48));
//@@ 6
    rows = sectioned((a) => {
      const f = a.focused, bg = f ? FOC : t.chrome, c = scol(a);
      return click(h_flex(), a).h(54).mb(2).pr(10).gap(10).flex_none().items_center().rounded(8)
        .when(f, (e) => e.pl(8).bg(FOC).border(2).border_color(t.accent))
        .when(!f, (e) => e.pl(7).border_l(3).border_color(c ?? "#00000000").hover((s) => s.bg(t.hover)))
        .child(two(a, bg, { lead: dot(a, bg) }))
        .child(div().w(28).flex_none().text_right().text_size(24).font_weight(f ? 600 : 400).text_color(statusNum(a, bg)).child(NS(a)));
    }, (s) => subLine(s, 14, 38));
//@@ 7
    LISTPX = 0;
    rows = flat((a) => {
      const f = a.focused, tint = tintOf(a, "1f"), bg = f ? over(t.accent + "30", tint ?? t.chrome) : tint ?? t.chrome, subs = kids(a);
      return click(h_flex(), a).h(38).mb(2).pl(f ? 8 : 10).pr(14).gap(10).flex_none().items_center().bg(bg)
        .when(f, (e) => e.border(2).border_color(t.accent)).when(!f, (e) => e.hover((s) => s.bg(over(t.hover, bg))))
        .child(div().w(14).flex_none().text_size(15).font_bold().font_family(mono()).text_color(quiet(a) ? C(mix(t.accent, t.chrome, 0.7), bg) : C(t.accent, bg)).child(NS(a)))
        .child(dot(a, bg))
        .child(h_flex().flex_1().min_w(0).gap(6).items_center()
          .child(div().flex_none().text_size(15).font_medium().text_color(quiet(a) ? S(bg) : P(bg)).child(nm(a)))
          .child(div().flex_1().min_w(0).truncate().text_size(12).text_color(S(bg)).child(more(a) ?? "")))
        .when(subs.length > 0, (e) => e.child(fold(a, subs)))
        .child(div().flex_none().text_size(12).font_family(mono()).text_color(S(bg)).child(dur(a)));
    }, (s) => subLine(s, 16, 56));
//@@ 8
    LISTPX = 10;
    rows = cards((a) => {
      const f = a.focused, bg = f ? over(t.accent + "40", t.raised) : quiet(a) ? mix(t.raised, t.chrome, 0.45) : t.raised;
      return card(a, bg, h_flex().gap(8).items_center().child(numeral(a, bg, statusNum(a, bg))).child(two(a, bg)), scol(a));
    });
