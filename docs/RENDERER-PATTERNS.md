# BeatCrate renderer patterns

Moved verbatim out of `CLAUDE.md` on 2026-09-10 to bring that file under its
1500-word budget. These are load-bearing renderer decisions whose violation
shows up **on screen** — a wrong colour, a snapped scale, a dismissed popover —
so they teach by failing and don't earn CLAUDE.md rent. The renderer rules whose
violation is *silent* stayed behind in `CLAUDE.md` § Design System.

Read this before touching `public/style.css` or the Home/Library/Career views.

## Aurora — Honey palette (locked, dark mode only)

```
--bg:         #060a07      --fg:         #ede5d3
--bg-lift:    #0c1410      --fg-soft:    rgba(237,229,211,0.62)
--accent:     #c8a35a      --fg-faint:   rgba(237,229,211,0.32)
--accent-hi:  #e6c485      --fg-ghost:   rgba(237,229,211,0.10)
--accent-dim: rgba(200,163,90,0.16)   --rule: rgba(237,229,211,0.10)
--topbar:     #080604      --topbar-text: #ede5d3
```
Active styles live in `[data-theme="dark"]` blocks (`<html>` hardcodes `data-theme="dark"`). Dark mode only — no toggle. Dead `:root` light-mode block left for a future cleanup pass. Fonts: Archivo (sans) + JetBrains Mono (mono) via Google Fonts.

## Glass surfaces

**Aurora glass surfaces** (inspector, popovers, Home cards, search modal): `rgba(8,12,10,0.62)` + `backdrop-filter: blur(24px) saturate(125%)` + 14px radius + honey 1px inset hairline. Match this for new floating surfaces.

## Track tag pills (crate detail)

**Track tag pills (crate detail) — several load-bearing patterns:** outer pill stays `overflow: visible` so the absolute `×` (top/right −5px) escapes; only the inner `.track-tag-label` ellipsizes. `.track-name` is fixed 260px; `.track-tags` is `flex: 1 1 auto`. Overflow is **dynamic** (`applyTagOverflow()` measures `scrollWidth > clientWidth` and inserts a `+M` glass badge with a hover popover) — no hardcoded pill cap. The `+M` popover has an invisible 12px hover bridge so the cursor gap doesn't dismiss it. The `+` add button is a flex sibling of `.track-tags-chips`, not nested inside.

## Home welcome animation

**Home welcome animation (once per session):** ghosted "Midnight Wax" vinyl (`buildWelcomeVinyl()`, `discOnly`) + word-mask greeting. Two body classes drive it: `body.welcoming` (hides chrome, removed ~3800ms) and `body.mesh-fullscreen` (pins mesh to full viewport, removed ~4700ms). Spin runs on an inner `.welcome-vinyl-spin` wrapper; the outer element only transitions opacity+scale — **don't put `vinyl-spin` and `scale()` on the same element** (rotation keyframes overwrite the scale and it snaps). The mesh wrap is permanently `position: fixed; bottom: 84px` and only animates `bottom` — **never switch it between fixed/absolute** (position isn't transitionable). Re-trigger: `sessionStorage.removeItem('welcomed'); location.reload();`.

## Section titles (Home/Library/Career)

**Section titles (Home/Library/Career):** JetBrains Mono uppercase, 44px, weight 400, `letter-spacing: 0.18em`, honey `--accent-hi`. Source text is sentence-case; CSS uppercases. Header bands: `flex; align-items: flex-end; justify-content: space-between; margin-bottom: 22px`.

## View-mode toggle

**View-mode toggle** (`.view-mode-toggle` / `.view-mode-btn`): glass pill, active button solid honey on near-black, weight 700. Home renders it via `renderHomeHero()`; Library inlines `libModeToggleHtml()` — there is no `renderLibraryModeToggle()`.

## Mesh

**Mesh:** animated WebGL shader (`injectMesh(wrapId)`) on Home/Library/Career; the RAF only renders the active view's canvas and polls `clientHeight` each frame to follow the `bottom` transition.

## Settings (renderer)

- **Auto-save, no Save buttons.** `setupSettingsAutoSave()` wires blur+Enter on the name/folder/ableton inputs; saves only when the trimmed value changed. Browse buttons commit on pick (set `_prevValue` so the blur won't double-save). **Don't reintroduce Save buttons.**
- Native folder pick / reveal go through `window.beatcrateNative` (the dialog/opener shim) — don't reach for plugin APIs directly from feature code.

## Running the App

| Command | What it does |
|---|---|
| `cd src-tauri && cargo tauri dev` | Dev app — compiles Rust, opens the window, hot-serves `public/`. **First build is slow** (compiles rusqlite-bundled + symphonia); incrementals are fast. |
| `cd src-tauri && cargo tauri build` | Release `.dmg` (unsigned aarch64, ~7 MB) under `src-tauri/target/release/bundle/dmg/` |
| `cd src-tauri && cargo run -- --check-db` | Headless: open+migrate the DB at the resolved path, print row counts, exit. No window. |
| `cd src-tauri && cargo run -- --verify-ingest` | Headless: run ingest + .als index + loudness against the resolved DB. **Point `BEATCRATE_DATA_DIR` at a COPY — it writes.** |

> ⚠️ **`cargo tauri dev` does NOT apply the production CSP.** The bundled app applies a stricter CSP than dev — so dev-verification cannot catch CSP-class breakage (see `CLAUDE.md` § Distribution, the bundle-only CSP gotcha). **Always smoke-test the actual `.dmg` bundle before declaring a renderer change done.**

Reinstall after a rebuild: mount the dmg → `ditto /Volumes/BeatCrate/BeatCrate.app /Applications/`.

