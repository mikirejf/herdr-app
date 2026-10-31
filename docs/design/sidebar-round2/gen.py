import sys, re, json
base = open('/tmp/hk-test/variants/a1/agents.main.js').read()
spb = open('/tmp/hk-test/variants/a1/spaces.main.js').read()
helper = open('/tmp/hk-test/variants/explore/helper.js').read()
prelude = open('/tmp/hk-test/variants/explore/prelude.js').read()
dirs = {}
cur = None
for line in open('/tmp/hk-test/variants/round2/dirs.js').read().splitlines(keepends=True):
    m = re.match(r'//@@ (\d+)', line)
    if m: cur = m.group(1); dirs[cur] = ''; continue
    dirs[cur] += line
def sub(x, old, new):
    assert old in x, old[:90]
    return x.replace(old, new)
common = dirs.pop('0')
for d, body in dirs.items():
    body = common + body
    a = base
    a = sub(a, 'import { View, div, image, svg } from "gpui-kit";', 'import { View, div, image, svg } from "gpui-kit";\n' + helper)
    a = sub(a, '    const position = new Map(', '    let position = new Map(')
    start = a.index('    // Theme files that want')
    end = a.index('    return v_flex()\n      .size_full()\n      .min_h(0)\n      .bg(t.chrome)')
    a = a[:start] + prelude + body + '    if (collapsed) rows = list.map(rail);\n    console.log("LOW", ' + json.dumps(d) + ', rec.low.toFixed(2));\n' + a[end:]
    a = sub(a, '.px(collapsed ? 0 : 8).overflow_y_scrollbar()', '.px(collapsed ? 0 : LISTPX).overflow_y_scrollbar()')
    a = sub(a, '.child(div().text_size(12.5).font_medium().text_color(t.muted).child("Agents"))', '.child(div().text_size(12.5).font_medium().text_color(S()).child("Agents"))')
    a = sub(a, '.text_color(open ? t.dim : (t[top] ?? t.dim))', '.text_color(open ? S() : C(t[top] ?? t.muted))')
    a = a.replace('text_size(10.5)', 'text_size(12)')
    a = sub(a, 'div().size(14).flex_none().rounded(4).bg(t.blocked + "26")', 'div().size(17).flex_none().rounded(4).bg(t.blocked + "26")')
    a = sub(a, 'div().flex_none().px(6).h(18).rounded(5)', 'div().flex_none().px(6).h(20).rounded(5)')
    # fold(): it's defined before the prelude, so give it its own readable()
    a = sub(a, '    const fold = (a, subs) => {\n', '    const fold = (a, subs) => {\n      const { S, C } = readable(t, { low: Infinity });\n')
    open(f'/tmp/hk-test/variants/round2/h{d}/agents.main.js', 'w').write(a)
# spaces: readable text, unmistakable active row
s = spb
s = sub(s, 'import { View, div, svg } from "gpui-kit";', 'import { View, div, svg } from "gpui-kit";\n' + helper)
s = sub(s, '  row({ w, i, g, tree }, t) {\n    const on = w.active;', '  row({ w, i, g, tree }, t) {\n    const on = w.active;\n    const R = readable(t, this.rec);\n    const bg = on ? over(t.accent + "33", t.chrome) : t.chrome;')
s = sub(s, '      .px(10)\n      .h(rowHeight({ tree }))', '      .pl(7)\n      .pr(10)\n      .border_l(3)\n      .border_color(on ? t.accent : "#00000000")\n      .h(rowHeight({ tree }))')
s = sub(s, '.when(on, (el) => el.bg(t.selected))', '.when(on, (el) => el.bg(bg))')
s = sub(s, ".justify_center().text_size(11).font_bold().text_color(\"#ffffff\")", ".justify_center().text_size(12).font_bold().text_color(R.on(fill))")
s = sub(s, '.truncate().font_medium().text_color(t.foreground).child(w.name))', '.truncate().text_size(15).font_weight(on ? 700 : 500).text_color(R.P(bg)).child(w.name))')
s = sub(s, '.child(g ? this.gitTail(g, tree, w.name, t) : div().flex_1())', '.child(g ? this.gitTail(g, tree, w.name, t, R.S(bg)) : div().flex_1())')
s = sub(s, '  gitTail(g, tree, name, t) {', '  gitTail(g, tree, name, t, sec) {')
s = sub(s, '      .text_size(12)\n      .text_color(t.dim)\n      .when(Boolean(where)', '      .text_size(12)\n      .text_color(sec)\n      .when(Boolean(where)')
s = sub(s, '    const t = theme();\n    const collapsed = narrow();', '    const t = theme();\n    this.rec = { low: Infinity };\n    const collapsed = narrow();')
s = sub(s, '.justify_between().text_size(12.5).font_medium().text_color(t.muted)', '.justify_between().text_size(12.5).font_medium().text_color(readable(t, this.rec).S())')
s = sub(s, '        .children(this.grouped(t, collapsed)));\n  }', '        .children(this.grouped(t, collapsed)));\n  }\n')
s = sub(s, '    return v_flex().size_full().min_h(0).bg(t.chrome).text_size(14)', '    const out = v_flex().size_full().min_h(0).bg(t.chrome).text_size(14)')
s = sub(s, '        .children(this.grouped(t, collapsed)));\n  }\n', '        .children(this.grouped(t, collapsed)));\n    console.log("SPLOW", this.rec.low.toFixed(2));\n    return out;\n  }\n')
open('/tmp/hk-test/variants/round2/spaces.main.js', 'w').write(s)
print('ok', sorted(dirs))
