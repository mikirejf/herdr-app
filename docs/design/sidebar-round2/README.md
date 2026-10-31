# Sidebar design round 2 (October 2026)

Eight prototypes for the agents list with big jump numbers (cmd+1..9),
built as edited copies of the `agents` and `spaces` mods and shot in an
isolated window. `all.png` shows all eight; `hN.png` is one each.

| | Design | Theme |
|---|---|---|
| H1 | Numeral cards: big number in a card, status as the left edge | ink |
| H2 | Tinted numerals: big number, row tinted by status | graphite |
| H3 | Terminal Herdr rows, status dot before the name, number at the end | vesper |
| H4 | Tinted cards, number at the end | atom-one-dark |
| H5 | Terminal Herdr two-line rows, tinted by status | vesper |
| H6 | Two-line rows, big number at the end, status edge | graphite |
| H7 | Single-line rows tinted by status, number on the left | vesper |
| H8 | H1 in vesper | vesper |

Every design keeps text at 12px or more, secondary text at 4.5:1 contrast or
better and primary text at 7:1, computed from the theme by `readable()` at the
top of each `agents.main.js`. `hN/agents.main.js` and `spaces.main.js` are
the mod files as shot. `gen.py` built them from the shipped mods and
`dirs.js`; `all.swift` composed `all.png`. Both read from the scratch
folder they ran in (`/tmp/hk-test`), so they are a record, not a tool that
runs from here.
