# Direction C — Vinyl Crate

## Personality

The crate-digging metaphor, made spatial. The plugin window is the **B-side** of the desktop app — same family, but where the desktop's home screen pushes the disc forward as an animation, here it stays put as a small static element and the **groove lines** carry the motif into the body of the layout. Hairlines of honey, set at the desktop's `rgba(200,163,90,0.18)` value, separate notes rows the way grooves separate songs on a record.

## Opinionated choices

- **Glass: skipped.** The disc-and-grooves vocabulary is the centerpiece, and glass would compete with it for surface attention. The notes container is a flat region of `--bg`, structured by hairlines.
- **Header micro-vinyl:** a 30×30 disc with `repeating-radial-gradient` grooves and a honey label. It's a logo, not an animation — explicitly to honor the brief's "no spin" rule.
- **Section divider:** `SIDE B · NOTES` with a fading honey rule trailing into the count. Reads as a record-label conceit without being a costume.
- **Status line:** `33⅓ rpm · 14 crates loaded`. The DB-missing variant becomes `no record · locate beatcrate.db`. It's playful but never blocks the data.
- **Row dividers:** thin honey hairlines between rows, opacity 0.5 of `accent-hairline`, painted with a pseudo-element on each row's top edge from `16px` to `right: 16px`. They stop short of the side rails so the rows feel inset, like grooves on a record edge.
- **Check style:** a 14px **round** dot (instead of a square checkbox) — the only round chrome in the system, matching the disc motif. Honey fill on complete.
- **Add-note row:** a honey "needle" dot to the left of the input, and a pill-shaped `Press` button (as in "press a record") in lieu of `+`. The hairline above the input echoes the row dividers.
- **Settings:** a full-bleed overlay with the disc reappearing next to the title. Reinforces "same record, different side."

## Tradeoffs

The vocabulary is the most decorative of the three, and the puns (`Press`, `SIDE B`, `cut a new note…`) are deliberate — the desktop already leans into the vinyl metaphor on the welcome screen, and this direction is the only one that picks it up. If they read as too much in production, swapping the button label to `Add` and the field label section header to a plain `NOTES` is a one-line change that retains the groove dividers as the load-bearing motif.

The round check is the most divergent control. It pulls double duty as "record dot" and "checkbox" — at a smaller size to compensate for the unfamiliar shape.
