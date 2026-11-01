# Browser panes: a prototype to discuss

This branch puts a real web page in a Herdr pane on macOS, as a native view
over the window, and lets a coding agent in a Herdr terminal drive that page
with [`agent-browser`](https://github.com/vercel-labs/agent-browser) once
the person allows it. It has two engines: the system's WKWebView
(`browser-webkit` feature), which is the proposal, and CEF, the Chromium
Embedded Framework (`browser` feature), which it started on. It is a
prototype to start a conversation, not a pull request. Both features are
off by default; a default build has neither engine in it.

The demo, in six lines:

1. `herdr-app do browser.open --profile agent --url http://127.0.0.1:8765/browser-fixture.html` opens a local test page beside a terminal.
2. The pane's header says "Test profile: don't sign into real accounts." and "Agent access off · b1".
3. The person clicks **Allow agent…**; a macOS dialog asks "Let agents drive this browser?"; the person clicks **Allow**.
4. In the terminal, `agent-browser snapshot -i` lists the page's fields and buttons.
5. `agent-browser fill @e2 "Ada"` and `agent-browser click @e7` fill and submit the form while the person watches.
6. **Stop agent** ends the agent's connection and reloads the page.

Who built it and how: a coding agent (Claude) wrote the code and these docs
under Cameron's direction. Cameron chose the approach and approved the plan.
Separate agent reviewers attacked the agent-access design over several
rounds, and each round's findings were fixed or written down as limits.
Cameron did the hands-on checks that need a person at the keyboard; the
ones still open are listed in [Manual checks](#manual-checks). Nothing here
has run outside this one Mac.

## Cost

With the WebKit engine the release binary is 0.74 MB larger (0.30 MB
gzipped) and the `.app` stays at 62 MB; the engine is the OS's. With CEF the
`.app` is 387 MB (172 MB compressed), almost all of it CEF's framework,
which is why CEF isn't the proposal ([Engine](#engine-the-system-web-view-not-chromium)).
The default build is unchanged. `src/browser/` is about 8,100 lines, tests
included: about 2,200 WebKit-only, about 1,100 CEF-only, the rest shared.
It is ad hoc signed only: Developer ID signing and notarization are not
done. Measurements: [Details](#details-measured-facts).

## Can's three questions

From `AGENTS.md`, "Before you grow the app's core: ask Can".

**1. Does the app itself act on it?** Yes. A browser pane is a native
`NSView` that the app has to create, place on its block every frame, hide
when the block isn't shown, and close in order on quit (CEF closes pages
asynchronously, then shuts down). The app also has to decide who gets the
keyboard: a page that has focus takes keys before GPUI does, so the app
watches AppKit's first responder and sends its own global keys to itself
first.

**2. Can only the app see it?** Yes. Where each block lands this frame,
which overlays (palette, menus, dialogs, toasts, floating blocks, the drag
grip) cover it this frame, and which view AppKit has as first responder are
known only inside the app's render pass. Mods get the list of open browsers
(id, url, title, pane) but none of these.

**3. Can mods not do it correctly, even with care?** A mod can't host an
`NSView` or start CEF: CEF needs a hook in the app's `NSApplication` class
and its own helper apps in the `.app` bundle. For agent access there is a
second reason: agents can write mods, layouts, keys and themes and can reach
the control socket, so a switch any of those can reach would let an agent
turn its own access on. Here only an app-modal macOS dialog turns access
on. That is consent for a cooperating agent, not a boundary against
software running as you ([Security](#security-in-short)).

## What the core would need

Three hooks, as the prototype has them. The second and third hold for any
engine; the first is CEF's alone and goes away with another engine:

1. **CEF's app-class hook.** CEF requires `NSApp` to report whether
   `sendEvent:` is running (`CefAppProtocol` in `cef_application_mac.h`).
   The prototype adds three methods to GPUI's `GPUIApplication`
   (`vendor/gpui-pre-macos/src/platform.rs`, 34 lines, `HERDR:` marked):
   two report and set a flag, and `sendEvent:` sets it around the call.
   The protocol itself is added at run time, after CEF's framework loads.
2. **A block that hosts a native view.** Today a mod block,
   `{ "mod": "browser-prototype", "props": { "browser": "b1", "url": "…", "profile"?: "agent" } }`,
   that the app draws itself, with bounds and visibility reported each
   frame, and key and focus routing. It is a mod block because the core
   keeps a machine's whole layout as one document and `Content` in
   `src/layout.rs` is an untagged enum: a block of a new kind matches no
   variant in a released herdr-app, which then can't read that machine's
   layout at all. Checked against master's `Content`: `{ "browser": "b1" }`
   fails with "data did not match any variant of untagged enum Content",
   while the mod block parses, shows that one pane as a mod that didn't
   load, and keeps its props. A general "native view" element a mod could
   draw would make this an ordinary mod.
3. **An overlay hook.** GPUI draws in one Metal layer and can't draw over a
   native view, so Herdr's overlays report their rectangles each frame and a
   page under one is hidden (its block shows the title and url meanwhile).

With CEF, besides the hooks, the CEF runtime ships in the app: starting CEF, its
message pump (a 33 ms tick), the ordered close on quit and the key routing
(about 3,200 lines of `src/browser/`, tests included), plus a bundle with
CEF's framework and 5 helper apps.

Agent access, if wanted, is a fourth piece that has to sit in the app for
the consent reason above: the native dialog, the per-page token proxy
(`src/browser/proxy.rs`, `src/browser/agent.rs`, about 2,100 lines) and the
provider that `agent-browser` calls. The rest stands without it.

## Engine: the system web view, not Chromium

The prototype started on CEF because it was the fastest way to a page an
agent can drive over CDP. Shipping it would make Herdr a Chromium vendor:
macOS lets an app load code signed only by its own Apple team, so Herdr
would sign, host and patch Chromium itself, and Chromium ships security
fixes about every two weeks. Making CEF an optional download doesn't change
that. A spike got it working (a slim app, about 0.24 MB larger gzipped,
loaded CEF from `~/Library/Application Support` with every helper
sandboxed), but the engine still had to be re-signed under Herdr's team,
hosted by Herdr, and launched through a second app bundle. We don't
recommend it.

**The branch now has a second engine: the system's WKWebView**, behind its
own feature, `browser-webkit`. Both engines share the engine-neutral code
(the block, key routing, the overlay cover, the consent dialog, the token
proxy and its allowlist). Only one file differs per engine: `runtime.rs` for
CEF, `webkit.rs` for WebKit. On WebKit:

- **Size.** The release binary is 0.74 MB larger than the default build
  (0.30 MB gzipped). The `.app` is 62 MB either way, against 387 MB for CEF.
  WebKit is part of macOS, so Herdr ships, signs and patches no engine, and
  none of CEF's app-class hook, message pump or helper apps.
- **Agents.** agent-browser 0.39.1 speaks CDP, and WebKit doesn't, so
  `src/browser/translate.rs` maps the CDP calls agent-browser makes onto
  WKWebView's own calls. Methods outside its table are refused. The demo
  ran end to end inside Herdr with trusted (`isTrusted`) input: snapshot,
  fill, select, check, click, get, eval, screenshot, console, errors and
  reload. It also ran on a CSP-strict site (github.com), because the agent's
  script is compiled by WebKit itself, never by string `eval`.
- **People.** Pages are debugged with Safari's Web Inspector (Develop
  menu); `browser.devtools` turns it on for a page. Zoom is per pane, not
  per site. The phone and tablet viewports set the size only, with no touch
  or device-pixel emulation.
- **Gaps.** No network events for agents (WebKit has no per-request API
  for them). The accessibility tree that `snapshot` reads is built in
  JavaScript and covers a subset of roles. No JS dialogs, downloads or file
  pickers. Full list: [browser-dev.md](browser-dev.md#the-webkit-engine-no-chromium).
- **One finding the reviews caught.** WebKit sends any key a page doesn't
  handle back to the app, so an agent's `press x` then `Enter` ran as a
  command in the person's focused terminal. That is fixed: the app now
  drops agent keys the page didn't take, and a regression check confirms
  the terminal receives nothing.

### Other operating systems

Nothing below has been built. It is research: the platform docs and the
source of wry and `gpui-webview` (longbridge's crate that already hosts
these web views in GPUI windows).

| | macOS | Windows | Linux |
|---|---|---|---|
| Engine | WKWebView (built) | WebView2. Microsoft ships and patches it, and it is part of Windows 11 | WebKitGTK |
| Agents | the CDP translator | CDP built in, per web view, with no port, so the proxy passes it through with no translator | the translator, ported |
| People's tools | Safari Web Inspector | Edge DevTools | WebKit's inspector, in a window. Its remote inspector opens a port and stays off |
| Embedding | a child `NSView`, as today | a child window. Open question: does it show above GPUI's DirectComposition layer? | X11 and XWayland only. Native Wayland can't embed another toolkit's view |
| Default build | +0.74 MB | small (estimated) | nothing: an optional module, so GTK and WebKitGTK never become startup requirements |

Servers pay nothing on any OS, because the browser lives only in
herdr-app's window code, never in herdr-core.

### What Can would maintain

The engine-neutral pieces, which every engine shares and which have to sit
in the app for consent: the block, the overlay cover, key routing, the
native dialog, the token proxy, the allowlist and revocation. Plus one small
adapter per OS. The adapters could come from outside crates: `gpui-webview`
or wry for hosting the view, and the CDP translator as its own crate. There
is no Chromium update cycle to follow.

Two other shapes were looked at and both cost the core more:

- **An engine plugin, or a separate browser app drawing into a block.** No
  OS supports embedding another process's native view, so the engine would
  have to send frames. The core would then need GPU frame import in three
  renderers, plus input, IME, accessibility, popup and drag-and-drop
  bridges: several thousand lines, more than the web view adapters.
- **CEF, bundled or downloaded:** Herdr owns Chromium (above).

People who want Chromium itself can run it outside Herdr: agent-browser can
launch its own Chrome, and terminal browsers that draw Chromium with the
kitty graphics protocol run in a Herdr terminal, which already renders
kitty graphics. Neither was run inside Herdr.

## The browser bar and the commands

The pane header's browser bar (back, forward, reload, the address, zoom, a
viewport button, DevTools) is a mod, `defaults/mods/pane/browser.js`: zero
core lines of its own.

Zoom and responsive size are two small core commands the bar, keys and
agents call, since only the app holds the page: `browser.zoom` (on CEF,
Chromium's host zoom kept per host name; on WebKit, per pane) and
`browser.viewport` (phone, tablet, a custom size, or fit; on CEF, Chromium's
device emulation; on WebKit, the size only). Together with their keys.json `browser`
section (`cmd-=`, `cmd--`, `cmd-0` zoom the page that has the keyboard) they
are about 350 lines of Rust outside tests (about 165 more in tests), 169 of
them in `src/browser/emulate.rs`.

Later, and mod-shaped where possible: browser tabs, docked
DevTools (today `browser.devtools` opens CEF's own window, or makes a WebKit
page inspectable from Safari), design mode (pick an element and send it to
the agent), a clear-data command, Linux and Windows.

## Security, in short

Agent access is a consent switch for a cooperating agent, not a security
boundary against software running as you. Full statement: [Agents drive a
page](browser-dev.md#agents-drive-a-page-agent-access) in
`docs/browser-dev.md`.

- **Agent profile only.** Agents can drive only a browser opened with
  `profile: "agent"`, a separate profile whose header always says "Test
  profile: don't sign into real accounts." On WebKit it is an in-memory
  data store that ends with the app. On CEF both profiles encrypt cookies
  with Chromium's public test key (`--use-mock-keychain`), so any process
  running as you can read them: don't sign into real accounts in either.
- **Native consent.** Access is off by default and never saved. Only
  **Allow** in an app-modal `NSAlert` turns it on; the control socket, mods
  and keys can only turn it off. The one exception is a debug test build
  with the `dev-agent-access` feature, where the control socket can turn it
  on and every agent header says "DEV SWITCH"; release builds don't have
  it. Cancel is the default button, and Allow stays disabled for half a
  second.
- **No remote-debugging port.** Agents get `ws://127.0.0.1:<port>/<token>`
  with a 256-bit token for one page. Herdr's proxy passes CDP (Chrome
  DevTools Protocol) messages to that page through a method allowlist: no
  browser-wide targets, no CDP cookie methods, no file inputs, no
  downloads, cookie and auth headers stripped from network events. Script
  in the page is allowed, so the agent can read the page's non-HttpOnly
  cookies, and it can take the pane to any http or https site.
- **Revocation.** Turning access off, closing the browser or quitting ends
  the token and closes the socket from Herdr's side. If an agent was
  connected when access goes off, the page reopens at its url, which drops
  what the agent set on the session (headers, overrides); the profile's
  cookies and local storage stay.
- **Residuals.** While access is on, any process running as you can get
  the token through the control socket and act as you on whatever site the
  pane shows. A process with Accessibility permission (or running under an
  app that has it) can click **Allow**, and any app can draw its own window
  over the dialog. agent-browser's own stream server (a loopback websocket
  with no token, checked on agent-browser 0.39.1) lets any local user, and
  any `localhost` or `file://` page in any browser, watch and drive an
  attached pane. Every local Herdr terminal points agent-browser at Herdr,
  so any agent-browser run there (a project's test suite included) can drive
  a pane with access on; `AGENT_BROWSER_PROVIDER=` opts a terminal out. The
  pane header and bottom strip are information, not the control: a mod or
  theme can cover or recolor them.

## Known gaps

- **Global keys win over a focused page.** Keys bound in keys.json's
  `global` section still go to Herdr, so `cmd-w` closes the tab and
  `cmd-1`…`cmd-9` switch tabs instead of reaching the page. A key bound to
  `terminal` in the `browser` section goes to the page instead; the defaults
  don't do that for any key.
- **Mod tooltips and in-mod popups draw under a page.** Only overlays Herdr
  registers hide a page.
- **The dialog's answer-time re-check is weak.** A page that moved to
  another site while the dialog was open gets access at its new site.
- **CEF's message pump is untested during menu tracking** and live window
  resizing.

Smaller ones are in [browser-dev.md](browser-dev.md#known-gaps).

## Try it

Needs macOS on Apple silicon, `cmake` and `ninja`, and for the agent part
`agent-browser` (tested with 0.39.1). Steps: [Try it](browser-dev.md#try-it)
and [Agents drive a page](browser-dev.md#agents-drive-a-page-agent-access)
in `docs/browser-dev.md`; show it on `just browser-dev --release`. The
WebKit engine needs no CEF download: `scripts/browser-dev-run --webkit`
(macOS 13.3 or later).

## Manual checks

Done by Cameron on this Mac: typing in a page (a dead key, a Japanese
IME), momentum scrolling, the right-click menu, a `<select>` popup and a
new-window link (`scripts/browser-test-page.html` has each);
`ctrl-alt-shift-escape` handing the keyboard back to Herdr; the command
palette and menus hiding a page and showing it again; browsing real sites
from the browser bar; and the agent demo: **Allow** in the dialog, which
named the same `b3` as the pane header, then an agent filling the test form.

Still open: Return, Esc and Cancel leaving access off; the half-second
Allow delay; **Stop agent**; `cmd-=`/`cmd--`/`cmd-0` and a global key
while a page has the keyboard; how a phone or tablet viewport looks in the
pane. That the app's other work waits while the dialog is open is read
from the code, not measured.

On the WebKit engine everything above is still open by hand: agent tests
ran on a window that was never on screen. Three checks are WebKit-specific.
An agent `press` of Tab and Space while another pane's consent dialog is
open must not press Allow. An agent drag over a link must not drop
anything where the real cursor is. An agent click on an input that calls
`focus()` must not take the keyboard from the person's terminal.

---

## Details: measured facts

| Fact | Value | How measured |
|---|---|---|
| App size, plain release | 62 MB `.app`, 25.8 MB `.tar.gz` | `scripts/package` (default features), then `du -sh Herdr.app` and the tarball's size |
| App size, WebKit release | binary +771,760 B (+0.74 MiB), gzip -9 +318,199 B; `.app` 62 MB | `HERDR_APP_RELEASE=1 cargo build --release --locked` with and without `--features browser-webkit`, one build each; `scripts/browser-dev-bundle --webkit --release`; `otool -L` shows the OS's `WebKit.framework` |
| App size, CEF release | 387 MB `.app`, 172 MB `.tar.gz` | `scripts/browser-dev-bundle --release`, then `du -sh "Herdr Dev.app"`; `tar -czf` of it |
| Where the CEF size goes | CEF framework 325 MB, app binary 59 MB, 5 helper apps 468 KB each | `du -sh` on each bundle part |
| Change size | about 6,800 lines added in 34 files, about 5,360 in `src/browser/` (tests included); `src/window.rs` about 570 | `git diff --shortstat origin/master...HEAD -- . ':!docs'`, `git diff --numstat` |
| CEF pin | `cef` crate `=156.2.0` (`156.2.0+156.0.14` in `Cargo.lock`), CEF 156.0.14 | `Cargo.toml`, `Cargo.lock` |
| Chromium version | 156.0.8078.48 | `CEF_VERSION` in the distribution's `include/cef_version.h`; the framework's `Info.plist` carries only `156.0.14.0` and branch 8078 |
| Signing | ad hoc, no hardened runtime, no team | `codesign -dv` on the app and a helper: `flags=0x2(adhoc)`, `Signature=adhoc`, `TeamIdentifier=not set`; `codesign --verify --deep --strict` exits 0 |
| Notarization | not attempted | — |
| Sandbox | renderer, GPU and utility processes run sandboxed with ad hoc signing | checked once, in the first spike (each helper process reported sandboxed); not re-checked on this build |
| Notices | not covered | `just notices` with cargo-about 0.9.3 exits 0 and leaves `THIRD-PARTY-NOTICES` unchanged; it has no `cef` or `cef-dll-sys` entry and nothing from Chromium's `CREDITS.html`; `licenses/inventory.json` has no CEF entry |
| CEF download | about 135 MB to download, about 470 MB in `CEF_PATH` once unpacked | size of the `.tar.bz2`; `du -sh` on `CEF_PATH` |

Signing. With ad hoc signing Chromium logs, once per launch,
`process_requirement.cc … Signature validation of current process failed …
(-67030)`. Nothing visible breaks; whether Chromium then skips checks
between its own processes is unverified. Library validation is off without
the hardened runtime, so `DYLD_INSERT_LIBRARIES` works on the browser
process (same user only). A Developer ID build needs per-helper entitlements
(JIT for the Renderer and GPU helpers), which are untested. The repo's
existing signing covers the app, not CEF's helpers.

Packaging. CEF on macOS only starts from an `.app` bundle with its
framework and helpers, so `cargo run` can't show a page; `just
browser-build` builds `target/browser-dev/Herdr Dev.app`.
