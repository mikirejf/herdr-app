// The CDP helper for Herdr's WKWebView engine (webkit.rs). It runs in an
// isolated WKContentWorld ("herdr-cdp"): it shares the DOM with the page but
// none of the page's globals, and the page can't see globalThis.__cdp.
// One id space: backendNodeId == nodeId == the number in a node objectId ("n:<id>").
// The accessibility tree goes out as raw rows; translate.rs shapes them for CDP.
(() => {
  if (globalThis.__cdp) return;
  const byId = new Map(), ids = new WeakMap(), objs = new Map();
  let next = 1, nextObj = 1;
  const idOf = (n) => { let i = ids.get(n); if (!i) { i = next++; ids.set(n, i); byId.set(i, new WeakRef(n)); } return i; };
  const node = (i) => { const n = byId.get(i)?.deref(); if (!n || !n.isConnected) throw new Error("No node with given id found"); return n; };
  const fromOid = (oid) => {
    if (typeof oid !== "string") throw new Error("Invalid remote object id");
    if (oid.startsWith("n:")) return node(+oid.slice(2));
    const k = +oid.slice(2); if (!objs.has(k)) throw new Error("Could not find object with given id"); return objs.get(k);
  };
  const pick = (p) => p.objectId ? fromOid(p.objectId) : node(p.backendNodeId ?? p.nodeId);
  const descr = (n) => n.nodeType === 1 ? n.localName + (n.id ? "#" + n.id : "") : n.nodeName;
  const remote = (v, byValue) => {
    const t = typeof v;
    if (v === undefined) return { type: "undefined" };
    if (v === null) return { type: "object", subtype: "null", value: null };
    if (t === "string" || t === "boolean") return { type: t, value: v };
    if (t === "number") return Number.isFinite(v) && !Object.is(v, -0) ? { type: t, value: v, description: String(v) } : { type: t, unserializableValue: String(v), description: String(v) };
    if (t === "bigint") return { type: t, unserializableValue: v + "n" };
    if (byValue) { let value; try { value = JSON.parse(JSON.stringify(v)); } catch (_) {} return { type: t === "function" ? "function" : "object", value }; }
    if (v instanceof Node) return { type: "object", subtype: "node", className: v.constructor.name, description: descr(v), objectId: "n:" + idOf(v) };
    const k = nextObj++; objs.set(k, v);
    const sub = Array.isArray(v) ? "array" : v instanceof Error ? "error" : undefined;
    return { type: t === "function" ? "function" : "object", subtype: sub, className: v.constructor?.name, description: v instanceof Error ? v.name + ": " + v.message : (v.constructor?.name || "Object"), objectId: "o:" + k };
  };
  // CDP's unserializable values, parsed (not eval'd).
  const unserializable = (s) => {
    if (s === "NaN") return NaN;
    if (s === "Infinity") return Infinity;
    if (s === "-Infinity") return -Infinity;
    if (s === "-0") return -0;
    if (typeof s === "string" && /^-?\d+n$/.test(s)) return BigInt(s.slice(0, -1));
    throw new Error("Couldn't parse value object in call argument");
  };
  const exc = (e) => ({ result: remote(e, false), exceptionDetails: { exceptionId: 1, text: "Uncaught", lineNumber: 0, columnNumber: 0,
    exception: { type: "object", subtype: "error", className: e?.name || "Error", description: e instanceof Error ? e.name + ": " + e.message : String(e) } } });

  // --- accessibility tree (a small HTML-AAM/accname subset; Chromium role names) ---
  const text = (s) => (s || "").replace(/\s+/g, " ").trim();
  const hidden = (el) => el.localName === "option" ? !!el.closest("[hidden],[aria-hidden=true]") : el.hidden || el.getAttribute("aria-hidden") === "true" || ["SCRIPT", "STYLE", "TEMPLATE", "NOSCRIPT", "HEAD"].includes(el.tagName) ||
    (el.checkVisibility ? !el.checkVisibility({ visibilityProperty: true }) : getComputedStyle(el).display === "none");
  const INPUT_ROLE = { checkbox: "checkbox", radio: "radio", range: "slider", number: "spinbutton", search: "searchbox", button: "button", submit: "button", reset: "button", image: "button" };
  const role = (el) => {
    const r = el.getAttribute("role"); if (r) return r.split(" ")[0];
    switch (el.localName) {
      case "a": case "area": return el.hasAttribute("href") ? "link" : "generic";
      case "button": return "button";
      case "input": return el.type === "hidden" ? null : INPUT_ROLE[el.type] || (el.list ? "combobox" : "textbox");
      case "textarea": return "textbox";
      case "select": return el.multiple || el.size > 1 ? "listbox" : "combobox";
      case "option": return "option";
      case "h1": case "h2": case "h3": case "h4": case "h5": case "h6": return "heading";
      case "ul": case "ol": case "menu": return "list";
      case "li": return "listitem";
      case "p": return "paragraph";
      case "label": return "LabelText";
      case "form": return "form";
      case "img": return el.getAttribute("alt") === "" ? "presentation" : "image";
      case "nav": return "navigation";
      case "main": return "main";
      case "header": return "banner";
      case "footer": return "contentinfo";
      case "aside": return "complementary";
      case "article": return "article";
      case "section": return el.hasAttribute("aria-label") || el.hasAttribute("aria-labelledby") ? "region" : "generic";
      case "table": return "table";
      case "tr": return "row";
      case "td": return "cell";
      case "th": return "columnheader";
      case "dialog": return "dialog";
      case "iframe": return null; // frames are not expanded
      default: return "generic";
    }
  };
  const FROM_CONTENT = new Set(["button", "link", "heading", "option", "cell", "columnheader", "rowheader", "tab", "menuitem", "menuitemcheckbox", "menuitemradio", "treeitem", "switch", "radio", "checkbox", "tooltip"]);
  const contentText = (el, skip) => {
    let s = "";
    for (const c of el.childNodes) {
      if (c.nodeType === 3) s += c.data;
      else if (c.nodeType === 1 && c !== skip && !hidden(c) && !["INPUT", "SELECT", "TEXTAREA"].includes(c.tagName)) s += " " + (c.getAttribute("aria-label") || contentText(c, skip)) + " ";
      else if (c.nodeType === 1 && c.tagName === "INPUT" && c !== skip && ["submit", "button"].includes(c.type)) s += c.value;
    }
    return s;
  };
  const name = (el, r) => {
    const lb = el.getAttribute("aria-labelledby");
    if (lb) { const t = lb.split(/\s+/).map((id) => document.getElementById(id)).filter(Boolean).map((e) => contentText(e)).join(" "); if (text(t)) return text(t); }
    const al = el.getAttribute("aria-label"); if (al && text(al)) return text(al);
    if (el.labels && el.labels.length) { const t = [...el.labels].map((l) => contentText(l, el)).join(" "); if (text(t)) return text(t); }
    if (el.localName === "input" && ["submit", "button", "reset"].includes(el.type)) return el.value || (el.type === "submit" ? "Submit" : el.type === "reset" ? "Reset" : "");
    if (el.localName === "img") return text(el.getAttribute("alt"));
    if (FROM_CONTENT.has(r)) return text(contentText(el));
    return text(el.getAttribute("title") || el.getAttribute("placeholder") || "");
  };
  const props = (el, r) => {
    const p = {};
    if (r === "heading") p.level = +(el.getAttribute("aria-level") || el.localName.slice(1)) || 2;
    if (r === "checkbox" || r === "radio" || r === "switch") p.checked = el.indeterminate ? "mixed" : String(el.checked ?? el.getAttribute("aria-checked") === "true");
    if (r === "combobox") p.expanded = el.getAttribute("aria-expanded") === "true";
    if (r === "option" && (el.selected || el.getAttribute("aria-selected") === "true")) p.selected = true;
    if (el.disabled || el.getAttribute("aria-disabled") === "true") p.disabled = true;
    if (el.required) p.required = true;
    return p;
  };
  const value = (el, r) => {
    if (r === "textbox" || r === "searchbox" || r === "spinbutton" || r === "slider") return el.value ?? null;
    if (r === "combobox" && el.localName === "select") return el.selectedOptions[0]?.text ?? null;
    return null;
  };
  // Rows: [id, role, name, value|null, {props}, [child ids]], parents first.
  const axTree = () => {
    const nodes = [];
    const emit = (dom, r, nm, el) => { const row = [idOf(dom), r, nm, el ? value(el, r) : null, el ? props(el, r) : {}, []]; nodes.push(row); return row; };
    // The AX ids this DOM node gives its parent (an element with no role passes its children up).
    const walk = (dom) => {
      if (dom.nodeType === 3) { const t = dom.data.replace(/\s+/g, " "); return t.trim() ? [emit(dom, "StaticText", t, null)[0]] : []; }
      if (dom.nodeType !== 1 || hidden(dom)) return [];
      const r = role(dom);
      const kidsOf = () => dom.localName === "select" ? [...dom.options].flatMap(walk) : [...dom.childNodes, ...(dom.shadowRoot ? dom.shadowRoot.childNodes : [])].flatMap(walk);
      if (!r || r === "presentation" || r === "none") return kidsOf();
      const row = emit(dom, r, name(dom, r), dom);
      const kids = kidsOf();
      // A text field's own text is its value, not children.
      if (!(r === "textbox" || (r === "combobox" && dom.localName !== "select"))) row[5] = kids;
      return [row[0]];
    };
    const root = emit(document, "RootWebArea", document.title, null);
    root[5] = document.body ? walk(document.body) : [];
    return { nodes };
  };

  const describe = (n, depth) => {
    const d = { nodeId: idOf(n), backendNodeId: idOf(n), nodeType: n.nodeType, nodeName: n.nodeName, localName: n.localName || "", nodeValue: n.nodeType === 3 ? n.data : "" };
    if (n.nodeType === 1) { d.attributes = [...n.attributes].flatMap((a) => [a.name, a.value]); d.childNodeCount = n.childNodes.length; }
    if (depth !== 0 && n.childNodes.length) d.children = [...n.childNodes].map((c) => describe(c, depth - 1));
    return d;
  };
  const scrollable = (el) => {
    for (let e = el; e && e !== document.documentElement; e = e.parentElement) {
      const s = getComputedStyle(e);
      if (/(auto|scroll)/.test(s.overflowY + s.overflowX) && (e.scrollHeight > e.clientHeight || e.scrollWidth > e.clientWidth)) return e;
    }
    return null;
  };

  globalThis.__cdp = {
    getFullAXTree: () => axTree(),
    resolveNode: (p) => ({ object: remote(pick(p), false) }),
    getDocument: (p) => ({ root: describe(document, p.depth ?? 1) }),
    querySelectorAll: (p) => ({ nodeIds: [...node(p.nodeId).querySelectorAll(p.selector)].map(idOf) }),
    querySelector: (p) => ({ nodeId: (() => { const e = node(p.nodeId).querySelector(p.selector); return e ? idOf(e) : 0; })() }),
    describeNode: (p) => ({ node: describe(pick(p), p.depth ?? 0) }),
    focus: (p) => { pick(p).focus(); return {}; },
    scrollIntoViewIfNeeded: (p) => {
      const el = pick(p), r = (el.nodeType === 1 ? el : el.parentElement).getBoundingClientRect();
      if (r.top < 0 || r.left < 0 || r.bottom > innerHeight || r.right > innerWidth) (el.nodeType === 1 ? el : el.parentElement).scrollIntoView({ block: "center", inline: "center" });
      return {};
    },
    getBoxModel: (p) => {
      const el = pick(p), r = (el.nodeType === 1 ? el : el.parentElement).getBoundingClientRect();
      if (!r.width && !r.height) throw new Error("Could not compute box model.");
      const q = [r.left, r.top, r.right, r.top, r.right, r.bottom, r.left, r.bottom];
      return { model: { content: q, padding: q, border: q, margin: q, width: Math.round(r.width), height: Math.round(r.height) } };
    },
    layoutMetrics: () => {
      const se = document.scrollingElement || document.documentElement;
      const vp = { pageX: scrollX, pageY: scrollY, clientWidth: innerWidth, clientHeight: innerHeight };
      const vv = { offsetX: 0, offsetY: 0, pageX: scrollX, pageY: scrollY, clientWidth: innerWidth, clientHeight: innerHeight, scale: 1, zoom: 1 };
      const cs = { x: 0, y: 0, width: se.scrollWidth, height: se.scrollHeight };
      return { layoutViewport: vp, visualViewport: vv, contentSize: cs, cssLayoutViewport: vp, cssVisualViewport: vv, cssContentSize: cs };
    },
    // A wheel at (x, y): the nearest scrollable box under it scrolls, else the page.
    wheel: (p) => {
      const at = document.elementFromPoint(+p.x || 0, +p.y || 0);
      (scrollable(at) || window).scrollBy(+p.deltaX || 0, +p.deltaY || 0);
      return {};
    },
    // No string eval here: a page's CSP would refuse it. webkit.rs puts the
    // agent's expression or function in the call's own script, which WebKit
    // compiles itself, and hands over what it gave: { ok: value } or { err: thrown }.
    evaluate: async (p, got) => {
      try { if ("err" in got) throw got.err; let v = got.ok; if (p.awaitPromise) v = await v; return { result: remote(v, p.returnByValue) }; } catch (e) { return exc(e); }
    },
    callFunctionOn: async (p, got) => {
      try {
        if ("err" in got) throw got.err;
        const fn = got.ok;
        if (typeof fn !== "function") throw new Error("Given expression does not evaluate to a function");
        const self = p.objectId ? fromOid(p.objectId) : undefined;
        const args = (p.arguments || []).map((a) => a.objectId ? fromOid(a.objectId) : "unserializableValue" in a ? unserializable(a.unserializableValue) : a.value);
        let v = fn.apply(self, args); if (p.awaitPromise) v = await v;
        return { result: remote(v, p.returnByValue) };
      } catch (e) { return exc(e); }
    },
    releaseObject: (p) => { if (typeof p.objectId === "string" && p.objectId.startsWith("o:")) objs.delete(+p.objectId.slice(2)); return {}; },
    releaseObjectGroup: () => { objs.clear(); return {}; },
  };
})();
