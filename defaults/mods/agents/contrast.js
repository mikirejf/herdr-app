// Text colors that stay readable on any theme. `readable(theme())` pushes a color
// toward white (on dark backgrounds) or black (on light ones) until it reaches a
// WCAG contrast minimum against the background it is drawn on.

function rgba(c) {
  let h = String(c).replace("#", "");
  if (h.length === 3) h = [...h].map((x) => x + x).join("");
  const n = parseInt(h.slice(0, 6), 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255, h.length >= 8 ? parseInt(h.slice(6, 8), 16) / 255 : 1];
}

const hex = (v) => "#" + v.slice(0, 3).map((x) => Math.round(Math.min(255, Math.max(0, x))).toString(16).padStart(2, "0")).join("");

/** `top` (which may be translucent, `#rrggbbaa`) painted over the opaque `under`. */
export function over(top, under) {
  const a = rgba(top), u = rgba(under);
  return hex([0, 1, 2].map((i) => a[i] * a[3] + u[i] * (1 - a[3])));
}

const mix = (a, b, k) => over(a + Math.round(k * 255).toString(16).padStart(2, "0"), b);

function luminance(c) {
  const [r, g, b] = rgba(c).slice(0, 3).map((x) => {
    x /= 255;
    return x <= 0.03928 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

export function contrast(a, b) {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

export function readable(t) {
  const base = over(t.chrome, t.canvas);
  // A background as it shows: translucent theme colors over the sidebar.
  const solid = (bg = t.chrome) => over(bg, base);
  const ink = (fg, bg, min) => {
    const b = solid(bg);
    const start = over(fg, b);
    const toward = luminance(b) < 0.4 ? "#ffffff" : "#000000";
    let c = start;
    for (let k = 0.05; k <= 1 && contrast(c, b) < min; k += 0.05) c = mix(toward, start, k);
    return c;
  };
  return {
    solid,
    /** Names and other main text: 7:1. */
    primary: (bg) => ink(t.foreground, bg, 7),
    /** Details, durations, headers: 4.5:1. */
    secondary: (bg) => ink(t.muted, bg, 4.5),
    /** A theme color (a status, the accent) as text: 4.5:1. */
    tinted: (color, bg) => ink(color, bg, 4.5),
    /** Text on a solid fill: the theme's dark or white, whichever reads better. */
    on: (fill) => {
      const f = solid(fill), dark = over(t.canvas, f);
      return contrast(dark, f) >= contrast("#ffffff", f) ? dark : "#ffffff";
    },
  };
}
