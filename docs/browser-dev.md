# Browser panes (prototype, macOS)

A browser pane shows a real Chromium page in a block: CEF (the Chromium
Embedded Framework) in windowed mode, one native view per page over the
window. It's behind the Cargo feature `browser`, off by default; other
builds show a placeholder where a browser block is. The same pane can run
on the system's WKWebView instead, with no Chromium in the app:
[the WebKit engine](#the-webkit-engine-no-chromium).

What the prototype is for, what it asks of the core, its measured size and
signing, and its main gaps: [browser-prototype.md](browser-prototype.md).

## Build and run

```sh
just browser-build   # target/browser-dev/Herdr Dev.app: the app, CEF's framework, 5 helper apps
just browser-dev     # the same, then runs it on its own home and core (/tmp/hbd.dev)
just browser-relaunch  # rebuild, then quit the running one and run the new build in the background
just browser-stop    # quit it and stop its core
```

A build never replaces a bundle a running app uses: CEF starts helper
processes from it on demand, so deleting it can kill that app.
`browser-build` refuses while an app runs from `target/browser-dev`;
`browser-dev` and `browser-relaunch` build beside it (`Herdr Dev.next.app`)
and swap the new bundle in once the old app has quit. A `FRESH=1` run builds
its own bundle inside its scratch home. `BROWSER_DEV_APP=<absolute path>.app`
sends `scripts/browser-dev-bundle` anywhere else.

Both need `cmake` and `ninja` (`brew install cmake ninja`). CEF on macOS only
starts from such a bundle, so `cargo run` can't show a page.

`CEF_PATH` is where CEF's binary distribution is downloaded once and kept
(about 135 MB to download, about 470 MB once unpacked). It must be outside the repo; the recipes default to
`~/.cache/herdr-app/cef`. A bare `cargo build --features browser` without it
is refused: `build.rs` says why, and `.cargo/config.toml` points CEF's
download at an address that doesn't resolve, so nothing lands in `target/`.
To build by hand:

```sh
CEF_PATH=~/.cache/herdr-app/cef CEF_DOWNLOAD_URL=https://cef-builds.spotifycdn.com \
  cargo build --features browser --bin herdr-helper --bin herdr-app
```

### The WebKit engine (no Chromium)

The `browser-webkit` feature builds the same pane, bar, commands, consent,
token proxy and agent provider on the system's WKWebView instead of CEF
(`src/browser/webkit.rs`). WebKit is part of macOS, so there is nothing to
download, no `CEF_PATH`, no helper apps, and the bundle is the app alone.
Each engine feature turns on `browser-pane`, the engine-neutral rest; build
with one of them, not both.

```sh
just browser-build --webkit   # target/browser-dev-webkit/Herdr Dev.app
just browser-dev --webkit     # the same, run on its own home (FRESH=1 for a scratch one)
cargo build --features browser-webkit --bin herdr-app   # `cargo run` works too
```

Agents drive it through the same proxy: the token, the dialog, the
allowlist and revocation are unchanged. Behind the proxy, a translator
(`src/browser/translate.rs`, `src/browser/webkit.js`) answers the CDP calls
agent-browser makes from WebKit: script in an isolated content world for
the DOM and the accessibility tree, `NSEvent`s sent to the view for trusted
clicks and keys, `takeSnapshot` for screenshots. A method it doesn't map
answers "wasn't found". An agent's script is never evaluated from a
string: it goes into the script WebKit runs, so a page whose
Content-Security-Policy forbids `eval` (github.com,
`scripts/browser-csp-fixture.html`) still answers `eval`, `get title` and
`back`. A statement list runs as a script and returns its last value by
value only. WebKit hands a key the page didn't handle back to the app;
for an agent's key, the window's event monitor drops it, so it never
reaches a terminal, a keys.json action or a menu. Gaps against CEF:

- **No network events for agents.** WebKit has no per-request API, so
  `network requests`, HAR and `route` see nothing.
- **Viewport is size only.** `browser.viewport` lays the page out at the
  preset's or the given size in CSS pixels; pixel ratio, mobile and touch
  emulation are CEF-only (WebKit has no public API for them).
- **Zoom is per pane** (WebKit's `pageZoom`), not per site.
- **DevTools are Safari's.** `browser.devtools` makes the page inspectable
  and a toast says where: Safari's Develop menu, with "Show features for web
  developers" on. Nothing opens in Herdr, and no port opens.
- **The accessibility tree is built in script**: common HTML roles and a
  subset of the ARIA name rules. Frames aren't expanded and closed shadow
  roots aren't seen; real sites may get thinner snapshots than on CEF.
- **No JavaScript dialogs, downloads or file pickers.** `alert`, `confirm`
  and `prompt` show nothing; a download is refused and logged, in both
  profiles.
- **Profiles.** The agent profile is in memory: its cookies and storage
  last until the app quits. The person's profile is WebKit's default store
  for the app's bundle id, so every Herdr home of that bundle shares it
  (CEF keeps one per home).
- **Popups from a cross-origin frame** open only from a clicked link:
  WebKit reports no gesture flag.
- **Console relay.** Console calls and uncaught errors reach agents through
  a small script in the page's own world, which a page can see and post to.

### As your everyday Herdr

`bash scripts/browser-daily on` copies the built bundle to
`~/Applications/Herdr Browser.app`, quits Herdr (your terminals keep
running in the core, as across an update), opens the copy on your real home
and core, and runs `browser.agent_browser_setup`. `off` quits it and opens
`/Applications/Herdr.app`; `status` says which runs. `on --cef` takes the
CEF bundle. This is the one path here that runs a prototype on your real
home: never run it and stock Herdr at once, and while it's off, stock
Herdr shows browser panes as a mod that didn't load. `DRY_RUN=1` prints
the steps. Moving to Herdr's own browser later: [browser-transition.md](browser-transition.md).

## Try it

`just browser-dev` runs the app on its own home, `/tmp/hbd.dev`, kept
between runs: spaces, pages and the browser profile survive a rebuild
(`FRESH=1 just browser-dev` uses a new scratch home instead). A new home
skips the welcome and opens a space with a terminal and the test page. While
the app runs, `scripts/` is served on `http://127.0.0.1:8765` (pages open
over http, https or `about:blank` only, no `file:`). `/tmp/hbd.dev/herdr-app`
talks to that window only; run the commands below through it, not your
installed `herdr-app`:

```sh
herdr-app do browser.open --url http://127.0.0.1:8765/browser-test-page.html
herdr-app do browser.open --url example.com --place tab
herdr-app ls
herdr-app do browser.navigate --browser b1 --url https://example.com/
herdr-app do browser.devtools --browser b1
herdr-app do browser.zoom --browser b1 --step in
herdr-app do browser.viewport --browser b1 --preset phone
herdr-app do browser.open_external --browser b1
herdr-app do browser.close --browser b2
```

`scripts/browser-test-page.html` has what to try by hand: typing with dead
keys and an IME, a `<select>`, a long list to scroll, a right-click menu and
a new-window link.

`bash scripts/browser-live-checks`, run in a terminal inside the dev app,
walks the agent checks that need a person at the window: the consent
dialog's keys, an agent's drag, right-click and typing (the keyboard must
stay where you left it), Stop agent, and that an iframe can't post lines
into the agent's console. It writes your answers to a results file.

## How it behaves

- A browser block is `{ "mod": "browser-prototype", "props": { "browser": "b1", "url": "..." } }`,
  as a pane in a tab (saved with the tab, so it reopens at its last page
  after a restart) or as a block in a layout. It is saved as a mod block so
  a herdr-app without this prototype, sharing the same core, shows that one
  pane as a mod that didn't load instead of failing to read the whole
  layout. The name isn't `browser`, which Herdr's own browser mod will
  likely use: neither reads the other's blocks. In a floating block it
  never opens: a native view can't float over GPUI's drawing.
- Each frame the page's view takes its block's bounds; a block that isn't
  painted (another tab, a zoomed pane, a hidden column) hides its page.
- GPUI can't draw over a page, so a page hides while any overlay overlaps
  its block: a dialog (its dimmed backdrop covers every page), a popup (the
  palette, a menu), a toast, a mod window, the update pill, a floating
  block, the pane grip and the drop preview while you drag a pane. Its block
  shows the page's title and url on the theme's `surface` meanwhile; the
  page keeps its scroll position and comes back when the overlay goes. A
  hidden page can't have the keyboard: Herdr takes it, and the page gets it
  back only if it should still have it then. The log names each change
  (`b1 hidden under overlay popup frame-palette`). Popups and tooltips a mod
  draws inside its own view (not through Herdr's overlays) still draw under
  a page.
- Clicking a page gives it the keyboard and makes its pane the focused one.
  A layout block's page keeps the keyboard until a click elsewhere in Herdr
  or a focus change. A dialog or popup (the palette, a menu, a rename
  prompt) takes the keyboard while it's open. So does a control you click
  (the Changes panel's commit box, a floating terminal), until it loses
  focus, the focused pane changes, or you click a page. A page that asks for
  focus on its own (CEF does when it creates one, and on loads) is refused
  unless it's the page that should have the keyboard. Closing the page that
  has the keyboard gives it back to Herdr.
  While a page has the keyboard, `ctrl-alt-shift-escape` and the keys bound in
  keys.json's `browser` section, then the shown layout's, then `global`, still
  go to Herdr, from the last keys.json that loaded. `browser` applies only
  there: the macOS defaults bind `cmd-=` and `cmd-+` to `browser.zoom:in`,
  `cmd--` to `browser.zoom:out` and `cmd-0` to `browser.zoom:reset`, so they
  zoom the page instead of Herdr (outside a page they still zoom Herdr;
  `cmd-1`…`cmd-9` still switch tabs). A key bound to `terminal` there goes to
  the page instead of Herdr. A keys.json written before this build has no
  `browser` section: add it. `send:` keys stay with the page; `app.hide` and
  friends hide the app as usual.
- **Zoom.** `browser.zoom {step: "in" | "out" | "reset"}` walks Chromium's
  own steps (25, 33, 50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250,
  300, 400, 500 %); `{level: 125}` (or the shorthand `browser.zoom:125`)
  sets a percent from 25 to 500. Chromium keeps one zoom per host name in
  each profile, as Chrome does: every port on that host shares it
  (`127.0.0.1:8765` and `127.0.0.1:8799` zoom together, `localhost` is
  another host), every open page there follows, a page that moves to another
  host takes that host's zoom, and the profile saves it, so it outlasts a
  restart. The person's and the agent profile keep separate zooms.
  `browsers()` and `herdr-app ls` show it as `zoom` (percent).
- **Responsive size.** `browser.viewport {preset: "phone"}` lays the page out
  at 390x844 (pixel ratio 3), `"tablet"` at 820x1180 (ratio 2), both as
  mobile pages with touch (`navigator.maxTouchPoints` 5, `pointer: coarse`);
  `{width, height, mobile?}` sets any size (1 to 10000); `"fit"` goes back to
  the pane's own size. It is Chromium's device emulation
  (`Emulation.setDeviceMetricsOverride`, `setTouchEmulationEnabled`, sent over
  the page's own DevTools session), not a user agent change. What the person
  sees: Chromium lays the page out at the emulated size at the pane's top
  left; a size bigger than the pane is shrunk (`scale`) until all of it fits,
  and it is fitted again whenever the pane changes size. This is read from
  Chromium's emulation and the log (`b1 viewport 390x844 mobile scale 0.83`);
  how it looks in the pane has not been checked by a person. An agent uses the
  same DevTools session, so its own `agent-browser set viewport` or `set device`
  replaces the person's size and the other way round: the last one set wins,
  and `viewport` in `browsers()` shows only what `browser.viewport` set. Two
  exceptions. The person's touch emulation (from phone or tablet) stays on
  under an agent's size until the person picks fit. And when the pane
  changes size while the person's viewport is shrunk to fit, Herdr sends the
  person's viewport again, over any agent override, without telling the
  agent. When an agent's connection ends the page reopens without any
  override. A page
  without a `<meta name="viewport">` lays out 980 px wide as a mobile page,
  as it would on a phone.
- Herdr never gives out a browser id (`b1`, `b2`, ...) twice while the app
  runs. A layout block can still open a page under any id, a closed
  browser's included.
- A popup or a `target="_blank"` link opens in the same pane.
- No remote-debugging port is opened. `browser.devtools` opens CEF's own
  DevTools window.
- The profile (cookies, cache) is `<app home>/browser/profile`; delete it
  while the app is closed to start fresh. Don't sign into real accounts in
  it: it uses a mock keychain. The app home, `browser/` and both profiles
  are created `0700`; the control socket is `0600` and drops a peer running
  as another user.
- One URL gate covers `browser.open`, `browser.navigate`, a restored
  block's url, popups and an agent's `Page.navigate`: `http`, `https` and
  `about:blank` only. A popup from a frame of another origin needs a click.
  Logs keep a url's scheme, host and path only.

## Agents drive a page (agent access)

An agent in a Herdr terminal can drive a browser pane with
[`agent-browser`](https://github.com/vercel-labs/agent-browser), once the
person allows it for that pane:

1. `herdr-app do browser.open --profile agent --url http://127.0.0.1:8765/browser-fixture.html`.
   `profile: "agent"` is a separate Chromium profile
   (`<app home>/browser/agent-profile`); only such a browser can be driven.
   Its header says "Test profile: don't sign into real accounts." Downloads
   are refused in it.
2. Click **Allow agent…** in that header. It turns nothing on. It opens a
   macOS dialog, "Let agents drive this browser?", naming the browser, its
   profile and its site (scheme and host only). The header shows the same
   browser id ("Agent access off · b1"), so you can match the dialog to the
   pane. An agent-profile browser's id must have the shape of the ids
   Herdr gives out: `b` and 1 to 9 digits, no leading zero
   (`registry::herdr_id`). A layout block on the agent profile with any
   other id never opens, so an agent can't put words of its own into the
   dialog through a layout. The check is the shape only: an agent that
   writes a layout block can still open an agent-profile browser under
   any such id, including one Herdr never gave out or a closed browser's
   id. The site is the page's host, which the page's owner chose.
   Access turns on only when
   you click **Allow** in that dialog; the log says
   `agent access on browser=b1 by=native confirm`. **Cancel** is the
   default button, so Return and Esc cancel; **Allow** has no key, and it
   stays disabled for half a second after the dialog shows, so a double
   click whose first click opened the dialog can't land on it. The dialog
   doesn't open while Herdr isn't the active app or while another such
   dialog is open, and an answer counts only if Herdr is still the active
   app and Herdr's record of the browser still has it open at the site the
   dialog named. That record is checked as the dialog closes, before Herdr
   handles any page event from while it was open, so a page that
   went to another site while the dialog was open isn't caught: access
   turns on for that browser at its new site. The
   dialog is app-modal and AppKit draws it in its own window, above every
   view in Herdr's window (GPUI's drawing and the pages): no layout, mod,
   theme, page or Herdr command can draw over it, move it or answer it.
   The header and the strip below are status markers Herdr draws, not the
   control: a mod or a page can still cover or recolor them, which can
   mislead you about the state but can't turn access on.
   Nothing else turns agent access on:
   `herdr-app do browser.agent_access --on true` and a mod's `run()` are
   refused, because an agent can reach both. Turning it off works from
   anywhere (the header's **Stop agent**, or
   `herdr-app do browser.agent_access --browser b1 --on false`). Access is
   never saved; closing the browser or quitting turns it off. While any
   browser has access on, a strip along the bottom of the window names it,
   with its own **Stop agent**. Herdr draws the header and the strip from the
   open browser, not the layout, so hiding or editing a block, or switching
   tab or space, doesn't hide them.
3. In a terminal Herdr started (any agent pane), `agent-browser snapshot -i`,
   `agent-browser click @e5`, `agent-browser fill @e2 "Ada"`. The header
   names the terminal that last attached, as it claimed itself.

Terminals that started before this Herdr (from another Herdr build, before
you switched to this one) have none of these variables, and a running
program's environment can't be changed from outside. Run
`herdr-app do browser.agent_browser_setup` once: it adds Herdr's provider to
`~/.agent-browser/config.json`, keeping the file's other plugins and
settings, and sets no default provider, so agent-browser outside Herdr is
unchanged. An agent in such a terminal then names the provider and a
session of its terminal's own:
`agent-browser -p herdr --session herdr-$HC_TERMINAL_ID snapshot -i`. Any
other session name is refused there, because agent-browser runs the
provider in the environment of whichever terminal started that session's
daemon. Such a terminal doesn't get `AGENT_BROWSER_IDLE_TIMEOUT_MS`, so its
daemon idles for agent-browser's own default (60 minutes) before exiting.
Terminal ids are per core, so the provider also reads the caller's core id
(`hc` from the caller's environment), and the window attaches only a
terminal of its own core.

How it works: Herdr gives its local terminals `AGENT_BROWSER_PROVIDER=herdr`,
`AGENT_BROWSER_PLUGINS` (Herdr's own provider, `herdr-app
__agent-browser-provider`, first) and an `AGENT_BROWSER_SESSION` of their
own. The provider asks the window over the control socket for the caller's
browser (`HERDR_BROWSER=<id>` picks one in the caller's space; else the
browser in its tab, else the one in its space the person focused last) and
gets `ws://127.0.0.1:<port>/<token>`: a 256-bit token for that one page.
Herdr's proxy passes CDP messages from that socket to the page's in-process
DevTools session through an allowlist (`src/browser/agent.rs`): no
`Target.attachToBrowserTarget`, no CDP cookie methods, no file inputs, no
downloads; `Cookie`, `Set-Cookie`, `Authorization` and
`Proxy-Authorization` headers are stripped from network events. Script in
the page is allowed, so it still reads the page's non-HttpOnly cookies
(`document.cookie`), and `Page.navigate` takes the pane to any http or
https site. Page-scoped emulation is allowed (`Emulation.setDeviceMetricsOverride`,
`setUserAgentOverride`, touch, media, locale, timezone): agent-browser 0.39.1's
`set viewport 390 844` and `set device "iPhone 16 Pro"` work. Both also ask
for `Browser.getWindowForTarget` (the native window's bounds), which stays
refused; agent-browser carries on without it. (0.39.1 has no "iPhone 15 Pro"
device; it says so itself.) The proxy refuses any upgrade with an `Origin` header or a
`Host` other than `127.0.0.1:<port>` or `localhost:<port>`, and answers
anything but an upgrade with a bare 404. Turning access off, a
new attach for the same page, closing the browser and quitting end the
token and the connection: Herdr shuts the socket itself, so an agent that
stopped reading can't hold it open. A connection's own end (the agent
closed it, a write it didn't take in 5 s, or more than 1024 messages or
32 MiB waiting for it) ends its token too; agent-browser asks the provider
for a new one. Whenever a connection ends, the page reopens at its url so
nothing the agent set (headers, overrides) stays. The reopen keeps the
profile's storage (cookies, local storage).
Every on, off and attach is a line in the app's log (closing or quitting
logs the off even if no agent attached), never the token or port. The log
is the app's stderr: `just browser-dev` keeps it in `/tmp/hbd.dev/app.log`, a
file any process running as you can edit, and an app opened from Finder
keeps none.

agent-browser's own stream server: while an agent-browser session is
attached to a pane, its daemon also runs a websocket server on
`127.0.0.1` with no token. This was checked against agent-browser 0.39.1
(source and a live run); later versions may differ. 0.39.1 starts it for
every session, with no setting to keep it from starting, and writes its port to
`~/.agent-browser/<session>.stream` (`agent-browser stream disable`, run
in that session, shuts it down until the daemon exits; the next daemon
starts it again). It accepts connections with no
`Origin` header, from `file://` pages, and from any `localhost`,
`127.0.0.1` or `::1` origin, and it relays mouse, keyboard and touch input
and screencast frames through the daemon's connection to Herdr. So while a
session is attached, any process on this machine, under any user, and
any page served from `localhost` or opened from a file, in any browser,
can watch the pane and click and type in it. Herdr can't change that code.
Herdr's terminals set `AGENT_BROWSER_IDLE_TIMEOUT_MS=300000`, so a daemon
whose agent stopped sending commands exits after 5 minutes (agent-browser's
default is 60), which ends its connection and its token; input sent
through the stream server also counts as activity and keeps the daemon
alive. Turning access off ends the connection at once.

agent-browser hides a provider's error text (`Plugin 'herdr' returned
success=false`); `agent-browser plugin run herdr herdr.check` says why
(for example `agent access is off for b1: ask the person to turn on agent
access for b1`). Terminals started before this build, or by `hc run`, don't
have the environment; `AGENT_BROWSER_PROVIDER=` opts a terminal out.
agent-browser's daemon reads the provider's environment once, when its
session starts, so to pick another browser run
`export HERDR_BROWSER=<id> AGENT_BROWSER_SESSION=$AGENT_BROWSER_SESSION-<id>`
before the next agent-browser command.

What this does and doesn't protect (state this wherever the prototype is
shown):

> Agent access is a consent switch for a cooperating agent, not a security
> boundary against software running as you. Only a person answering
> **Allow** in a native macOS dialog turns it on. A process running as you
> could still answer that dialog if it has macOS Accessibility permission
> (or runs under an app that has it), and any app can open its own window
> over the dialog without any permission, to relabel its buttons. While
> access is on, any process running as your user can get the page's CDP
> token through Herdr's control socket and act as you on any http or https
> site it points that pane at: run script in the page (which reads its
> non-HttpOnly cookies), click, type, and read what the page shows. Any
> process running as you can also read this profile's cookies from disk,
> because the prototype encrypts them with Chromium's public test key
> (`--use-mock-keychain`). It can also hide or recolor the in-window access
> marker (the pane header and the bottom strip) with a mod, a theme or a
> layout; those are information, not the control. Herdr's own agent
> connection needs a 256-bit token and accepts only loopback connections
> without an `Origin` header. But while an agent-browser session is
> attached, agent-browser's own stream server (a loopback websocket with no
> token, in agent-browser's code, not Herdr's) lets any user on this
> machine, and any page served from `localhost` or opened from a file in
> any browser, watch the pane and click and type in it. Every local Herdr
> terminal points agent-browser at Herdr, so any agent-browser run there (a
> project's test suite included) can drive a pane with access on;
> `AGENT_BROWSER_PROVIDER=` opts a terminal out. Turning access off,
> closing the browser, or quitting ends both. Use this browser only with
> test accounts, never your own.

Signing: the dev bundle is ad hoc signed without the hardened runtime, so
library validation is off (`DYLD_INSERT_LIBRARIES` works on the browser
process at its next launch, same user only), and Chromium logs a failed
signature check of its own process (`process_requirement.cc … -67030`);
whether Chromium then skips checks between its processes is unverified.

### Dev-only switch (automated tests only)

A build with the switch drops the consent property: an agent that can
relaunch the app (`herdr-app do app.quit`, then the app with the variable
set) can turn its own access on. So only an explicit test build has it:
`scripts/browser-dev-bundle --dev-agent-access` builds a debug bundle with
the `dev-agent-access` Cargo feature. `just browser-dev` and
`just browser-build` leave it out, and release builds ignore it (the code is
behind `debug_assertions` and the feature; the test
`builds_without_the_dev_feature_have_no_dev_switch` pins it).

Started with `HERDR_DEV_AGENT_ACCESS=1` in its own environment, such a
build lets `herdr-app do browser.agent_access --on true` turn access on, so
an agent can test the whole path without a person's click. It is read once
at launch and removed from the environment (terminals never inherit it),
logged as `DEV SWITCH ON` at startup and on every use, and every agent
header and the window's agent strip say "DEV SWITCH: the control socket can
turn agent access on". Show the prototype on a bundle without it
(`just browser-dev --release`).

## Known gaps

The main ones are in [browser-prototype.md](browser-prototype.md#known-gaps).
Smaller ones:

- **Takeover's first call fails.** When a second agent attaches to the same
  page, the first agent's connection ends and the page reopens; the new
  agent's first request gets `herdr: the page isn't there right now (opening
  or reopening): try again`. A retry half a second later works.
- **CEF logs "Timeout of new browser info response"** about 2 s after a
  page fails to load (seen in two test runs' logs). Its effect was not
  investigated.
- **A frame of lag while resizing.** The view follows its block every frame;
  GPUI has no layout-change event or hosted native view.
- **`herdr-app shot` can't see pages.** It renders GPUI only, so a shot shows
  the placeholder where a page is; a demo needs a screen recording. While the
  window isn't frontmost, `shot` can return a stale frame (the same image
  across changes); raise the window first (`{"raise": true}` on the socket).
- **The bar's width is known only after the pointer crosses it.** Mods get
  no layout size, so the bar measures itself on mouse move. Until then it
  draws its narrow form (no `-`/`+`, a bare zoom number, the viewport icon
  without its name). The address keeps at least 64 px; in a pane under about
  220 pt what's right of it (zoom, viewport, DevTools) clips instead.
- **No passkeys or security keys yet.** macOS lets an app run WebAuthn for
  any site only with an Apple-managed entitlement
  (`com.apple.developer.web-browser.public-key-credential`) that Herdr's
  team would request. Without it, both engines refuse at once; measured on
  macOS 27.2 with dev builds, `http://127.0.0.1` pages, person and agent
  profiles (`scripts/browser-passkey-probe.html` writes them into its title;
  `?get` adds a timed `get`):
  - **WebKit** reports `isUserVerifyingPlatformAuthenticatorAvailable()` and
    `isConditionalMediationAvailable()` as `false`, and
    `navigator.credentials.get` rejects in 0 ms (`NotAllowedError` on the
    person's profile, `SecurityError` on the agent profile), with no sheet.
    Its `getClientCapabilities()` still says `passkeyPlatformAuthenticator`
    and `hybridTransport` are true, so a site that trusts that call alone
    offers a passkey button that then fails. WebKit has no public API to turn
    WebAuthn off.
  - **CEF** starts with `--disable-blink-features=WebAuth`
    (`LIST_SWITCHES` in `src/browser/runtime.rs`): pages have no
    `PublicKeyCredential`, so sites offer passwords, and
    `navigator.credentials.get` rejects in under 25 ms. Without the switch
    CEF said conditional mediation was available, and a passkey request
    would run with no dialog (CEF in a native parent view has no dialog
    host) until the site's timeout. The switch is process-wide, so it covers
    agent-profile pages too.

  `browser.open_external` (the bar's open-in-browser button, shown on
  hover) opens the page in the default browser, where passkeys work. It
  takes the url from Herdr's record of the page, http or https only. Agents
  can call it too; that gives them nothing new, since a Herdr terminal can
  already run `open <url>`.

  Refusing passkeys on agent-profile pages, once they work, belongs in a
  native WebAuthn bridge, where Herdr's own code decides before any sheet
  shows. A script in the page can't do it: an agent's `eval` runs in that
  page and could undo it. Until the bridge exists, nothing can raise a
  passkey sheet on either engine (above).
