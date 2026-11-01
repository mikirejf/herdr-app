# Browser panes (prototype, macOS)

A browser pane shows a real Chromium page in a block: CEF (the Chromium
Embedded Framework) in windowed mode, one native view per page over the
window. It's behind the Cargo feature `browser`, off by default; other
builds show a placeholder where a browser block is.

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
herdr-app do browser.tab.new --browser b1 --url example.com
herdr-app do browser.tab.new --browser b1
herdr-app do browser.tab.select --browser b1 --index 0
herdr-app do browser.tab.close --browser b1
herdr-app do browser.close --browser b2
```

`scripts/browser-test-page.html` has what to try by hand: typing with dead
keys and an IME, a `<select>`, a long list to scroll, a right-click menu, a
`target=_blank` link and a `window.open` button (each lands as a tab).

`bash scripts/browser-live-checks`, run in a terminal inside the dev app,
walks the agent checks that need a person at the window: an agent's drag,
right-click and typing (the keyboard must stay where you left it). It writes your answers to a results file.

## How it behaves

- A browser block is `{ "mod": "browser-prototype", "props": { "browser": "b1", "tabs": [{ "url": "..." }], "active": 0 } }`,
  as a pane in a tab (saved with the tab, so it reopens with its tabs at
  their last pages and the same one shown after a restart; only urls and
  the shown index are saved) or as a block in a layout. It is saved as a mod block so
  a herdr-app without this prototype, sharing the same core, shows that one
  pane as a mod that didn't load instead of failing to read the whole
  layout. The name isn't `browser`, which Herdr's own browser mod will
  likely use: neither reads the other's blocks. In a floating block it
  never opens: a native view can't float over GPUI's drawing.
- Every browser pane of a tab shows the `pane` mod's header: a tab strip over
  the address bar, in every layout, whatever header the layout gives terminal
  panes (another mod, `reveal: "hover"`, none) or whether it frames panes at all.
  Its height is the layout's `pane` header height (default 30) plus 28 for the
  tabs. A browser block placed in a layout itself gets no header.
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
  restart.
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
  agent. When an agent's connection ends, Herdr undoes its overrides on
  the page and puts the person's viewport back (see "Agents drive a page"). A page
  without a `<meta name="viewport">` lays out 980 px wide as a mobile page,
  as it would on a phone.
- Herdr never gives out a browser id (`b1`, `b2`, ...) twice while the app
  runs. A layout block can still open a page under any id, a closed
  browser's included.
- A popup or a `target="_blank"` link opens as a new tab in the same pane,
  shown at once. It is a real popup of the page that opened it, hosted in
  the pane, so `window.opener` and `postMessage` work (sign-in popups need
  them); `window.close()` closes that tab, never the pane. `window.open()`
  with no url starts blank. A refused url is dropped.
- A browser has tabs, one shown. Each is its own Chromium page with an id
  unique in the app run (`b1:3`, not saved), created when the tab is first
  shown (a restart creates only the shown one); a created tab keeps running
  hidden until it is closed. A tab with no url has no page: its block shows
  "Browse with your agents". Closing a tab shows its right neighbour, else
  its left; the last one leaves an empty tab. `browser.navigate`, back,
  forward, reload, zoom, viewport and DevTools act on the shown tab. An
  agent attached to the pane sees all its tabs (below); closing a tab ends
  only that tab's agent sessions, never the pane's token or connection.
- No remote-debugging port is opened. `browser.devtools` opens CEF's own
  DevTools window.
- The profile (cookies, cache) is `<app home>/browser/profile`; delete it
  while the app is closed to start fresh. Don't sign into real accounts in
  it: it uses a mock keychain. The app home, `browser/` and the profile
  are created `0700`; the control socket is `0600` and drops a peer running
  as another user.
- One URL gate covers `browser.open`, `browser.navigate`, a restored
  block's url, popups and an agent's `Page.navigate`: `http`, `https` and
  `about:blank` only. A popup from a frame of another origin needs a click.
  Logs keep a url's scheme, host and path only.

## Agents drive a page

An agent in a Herdr terminal can drive any browser pane with
[`agent-browser`](https://github.com/vercel-labs/agent-browser). There is no
consent step, no separate agent profile and no per-pane switch: every browser
pane uses the person's own profile, and an agent that attaches gets that
page with its sign-ins.

1. `herdr-app do browser.open --url http://127.0.0.1:8765/browser-fixture.html`.
2. In a terminal Herdr started (any agent pane), `agent-browser snapshot -i`,
   `agent-browser click @e5`, `agent-browser fill @e2 "Ada"`.
3. `agent-browser tab` lists the pane's tabs, `tab new [url]` opens one (after
   the shown tab, and shows it), `tab t2` switches (0.39.1 takes `t2`, not `2`),
   `tab close` closes the one it is on. Snapshot, click, fill, eval and
   screenshot work in the tab it is on, shown or not. A `target=_blank` link
   or `window.open` opens a tab that agent-browser switches to by itself.

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
gets `ws://127.0.0.1:<port>/<token>`: a 256-bit token for that one pane, with
`directPage: false`: the socket is the pane's browser endpoint, with the pane's
tabs as its page targets. The proxy answers the Target domain itself
(`Target.setDiscoverTargets`, `getTargets`, `getTargetInfo`, `createTarget`,
`attachToTarget` with `flatten: true`, `detachFromTarget`, `closeTarget`,
`activateTarget`), only from that pane's tabs: none of it reaches Chromium and a
tab of another pane is never named. A target id is the tab's runtime id
(`b1:3`); a session id is Herdr's own (`HERDR…`), stripped on the way to the
tab's DevTools session and put back on what comes from it, with
several sessions per tab allowed. Attaching a tab whose page was never created
creates it, hidden (an empty tab opens at `about:blank`). `createTarget` opens a
tab after the shown one and shows it, `activateTarget` and `Page.bringToFront`
select a tab (the app window is not raised); attaching or driving a tab does
not change which one the person sees (agent-browser 0.39.1's `tab t2` sends no
`bringToFront`). `targetCreated`, `targetInfoChanged` and `targetDestroyed` follow
the pane's tabs whoever changed them (the person, `browser.tab.*`, a popup,
the agent). `getTargets` lists the shown tab first, because agent-browser starts
on the first page that answers (so `t1` is the shown tab at connect). A page's
own `Target.setAutoAttach` (frames) goes to the tab's session with `flatten`
forced on, and the frame sessions it reports route back to that tab; a
`Target.setAutoAttach` on the browser itself (agent-browser sends it only with
network filtering or proxy auth) is refused with an error. Every other message
goes to its tab's in-process DevTools session through an allowlist
(`src/browser/agent.rs`), the URL gate included (a `createTarget` url too): no
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
anything but an upgrade with a bare 404. A new attach
for the same pane, closing the browser and quitting end the
token and the connection (closing a tab does not): Herdr shuts the socket itself, so an agent that
stopped reading can't hold it open. A connection's own end (the agent
closed it, a write it didn't take in 5 s, or more than 1024 messages or
32 MiB waiting for it) ends its token too; agent-browser asks the provider
for a new one. Whenever a connection ends, the pages it sent
messages get the undo of everything an allowed method can set (emulation,
extra headers, cache and network conditions, enabled domains, auto-attach,
screencast, lifecycle events; `reset_calls` in `src/browser/agent.rs`, kept
in step with the allowlist by a test), then Herdr's own viewport goes back on.
The page itself is not reloaded: form input, history, scroll, and a popup's
opener stay as the person has them. CEF keeps one DevTools client per browser
and dropping its observer does not detach it (`CefDevToolsController`
detaches only when the browser is destroyed), so the undo is sent call by call.
What a page's own script did at the agent's bidding (typed text, DOM changes,
a created isolated world) stays. When Chromium itself drops a tab's DevTools
agent, only that tab's agent sessions end (`Target.detachedFromTarget`); the
token, the connection and the other tabs stay.
Every attach is a line in the app's log, never the token or port. The log
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
alive.

agent-browser hides a provider's error text (`Plugin 'herdr' returned
success=false`); `agent-browser plugin run herdr herdr.check` says why
(for example `no browser in this space: open one with: herdr-app do
browser.open --url <url>`). Terminals started before this build, or by `hc run`, don't
have the environment; `AGENT_BROWSER_PROVIDER=` opts a terminal out.
agent-browser's daemon reads the provider's environment once, when its
session starts, so to pick another browser run
`export HERDR_BROWSER=<id> AGENT_BROWSER_SESSION=$AGENT_BROWSER_SESSION-<id>`
before the next agent-browser command.

What this does and doesn't protect (state this wherever the prototype is
shown):

> There is no consent switch. Any process running as your user can ask
> Herdr's control socket for a page's CDP token and then act as you on any
> http or https site that pane is pointed at: run script in the page (which
> reads its non-HttpOnly cookies), click, type, and read what the page
> shows. Any process running as you can also read the profile's cookies
> from disk, because the prototype encrypts them with Chromium's public
> test key (`--use-mock-keychain`). Herdr's own agent connection needs a
> 256-bit token and accepts only loopback connections without an `Origin`
> header. But while an agent-browser session is attached, agent-browser's
> own stream server (a loopback websocket with no token, in agent-browser's
> code, not Herdr's) lets any user on this machine, and any page served
> from `localhost` or opened from a file in any browser, watch the pane and
> click and type in it. Every local Herdr terminal points agent-browser at
> Herdr, so any agent-browser run there (a project's test suite included)
> can drive a pane; `AGENT_BROWSER_PROVIDER=` opts a terminal out. Closing
> the browser or quitting ends the token. Keep sign-ins you can't lose out
> of a pane an agent can reach.

Signing: the dev bundle is ad hoc signed without the hardened runtime, so
library validation is off (`DYLD_INSERT_LIBRARIES` works on the browser
process at its next launch, same user only), and Chromium logs a failed
signature check of its own process (`process_requirement.cc … -67030`);
whether Chromium then skips checks between its processes is unverified.

## Known gaps

Known ones:

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
  team would request. Without it, WebAuthn refuses at once; measured on
  macOS 27.2 with dev builds, `http://127.0.0.1` pages (`scripts/browser-passkey-probe.html` writes them into its title;
  `?get` adds a timed `get`):
  - CEF starts with `--disable-blink-features=WebAuth`
    (`LIST_SWITCHES` in `src/browser/runtime.rs`): pages have no
    `PublicKeyCredential`, so sites offer passwords, and
    `navigator.credentials.get` rejects in under 25 ms. Without the switch
    CEF said conditional mediation was available, and a passkey request
    would run with no dialog (CEF in a native parent view has no dialog
    host) until the site's timeout. The switch is process-wide.

  `browser.open_external` (the bar's open-in-browser button, shown on
  hover) opens the page in the default browser, where passkeys work. It
  takes the url from Herdr's record of the page, http or https only. Agents
  can call it too; that gives them nothing new, since a Herdr terminal can
  already run `open <url>`.

  Until a native WebAuthn bridge exists, nothing can raise a passkey sheet
  (above).
