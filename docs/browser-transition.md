# Moving from this prototype to Herdr's own browser

Herdr plans an official browser mod. This page says what the prototype
leaves on your Mac, how to move off it in one step, and what to check in
the official mod before relying on it.

## What the prototype leaves where

| What | Where | Removed by |
|---|---|---|
| The everyday app copy | `~/Applications/Herdr Browser.app` (WebKit copy: bundle id `dev.herdr.app.browser-daily`) | `uninstall` |
| Browser panes in your layout | blocks `{ "mod": "browser-prototype", "props": { … } }` in each machine's layout document | you: close them; other Herdr builds show them as a mod that didn't load |
| agent-browser's provider entry | the `herdr` plugin in `~/.agent-browser/config.json` | `uninstall` (other plugins and settings stay) |
| Your own browsing data (cookies, sign-ins) | WebKit: `~/Library/{WebKit,HTTPStorages,Caches}/dev.herdr.app.browser-daily*`; CEF: `~/.config/herdr-app/browser` | `uninstall --data` |
| The agent profile | in memory (WebKit) or `~/.config/herdr-app/browser/agent-profile` (CEF) | quitting (WebKit); `uninstall --data` (CEF) |

The prototype saves its panes as `browser-prototype`, not `browser`, so its
blocks and an official browser mod's never read each other.

## Switching over

1. While the prototype runs, `bash scripts/browser-daily pages` lists each
   open page's title and address. A page without agent access is listed
   without its query string.
2. `bash scripts/browser-daily uninstall` quits it, removes the app copy and
   the agent-browser entry, and opens `/Applications/Herdr.app`. Add
   `--data` to delete the browsing data too; without it, the script says
   where the data is.
3. In Herdr, close the `browser-prototype` panes and reopen the pages from
   step 1 in the official mod.

Your terminals keep running throughout: they live in the core.

## What to check in the official mod

Each line is something this prototype hit; the issue has the detail.

- **Layouts on older apps.** A browser pane must not make an older herdr-app
  fail to read the whole layout (cameronsjo/herdr-app#4).
- **Passkeys.** Signing in with a passkey, from macOS's Passwords and from
  1Password, needs Apple's web-browser passkey entitlement and, on CEF, a
  native bridge (cameronsjo/herdr-app#2, cameronsjo/herdr-app#3).
- **Agents.** How an agent gets access to a page, whether only the person
  can turn it on, and whether an agent's keys can leak into the focused
  terminal (`docs/browser-dev.md`, "Agents drive a page").
- **Terminals older than the app.** Whether agents in terminals started
  before a switch can still find the browser (cameronsjo/herdr-app#6).
- **Hands-on behaviour.** Overlays above the page, keyboard handoff,
  right-click and drag (cameronsjo/herdr-app#1).
