# BeatCrate — Claude Code Instructions

BeatCrate is a personal macOS desktop app for music producers to browse, preview, and annotate their tracks — beats, loops, vocal takes, acoustic demos. Vinyl crate-digging metaphor. Vanilla HTML/CSS/JS frontend + **Rust/Tauri 2 backend + SQLite** (rusqlite, bundled). Single user; ships unsigned, local-only. A companion **VST3 + AU plugin** (`plugin/`, JUCE/C++) surfaces per-track notes inside any DAW.

## Architecture

```
src-tauri/src/main.rs       ← bin entry; routes --check-db / --verify-ingest to headless fns, else run()
src-tauri/src/lib.rs        ← Tauri builder: managed AppState (DB behind a Mutex), startup ingest,
                              live folder watcher, the invoke_handler command list
src-tauri/src/db.rs         ← SQLite schema + idempotent migrations (rusqlite); DB-PATH override (see §DB)
src-tauri/src/commands.rs   ← ~60 #[tauri::command]s — the renderer's whole API
src-tauri/src/ingestion.rs  ← scans the Music Folder into the DB
src-tauri/src/als.rs        ← Ableton .als project indexer (gunzip + read-only XML DOM walk)
src-tauri/src/loudness.rs   ← integrated-loudness (ebur128) — pure Rust, NO ffmpeg sidecar
src-tauri/tauri.conf.json   ← window, CSP, asset-protocol scope, bundle config
src-tauri/capabilities/     ← least-privilege permission grants (dialog open + opener reveal only)
public/index.html           ← single-page app shell
public/app.js               ← ALL frontend logic (vanilla JS, no framework) + the IPC facade
public/style.css            ← all styles
public/assets/, public/uploads/  ← logo SVG; user avatar uploads (uploads/ gitignored)
```

Music source folder (each subfolder = one crate) is stored in the `config` table (`albums_folder`), not hardcoded.

## Running the App

| Command | What it does |
|---|---|
| `cd src-tauri && cargo tauri dev` | Dev app — compiles Rust, opens the window, hot-serves `public/`. **First build is slow** (compiles rusqlite-bundled + symphonia); incrementals are fast. |
| `cd src-tauri && cargo tauri build` | Release `.dmg` (unsigned aarch64, ~7 MB) under `src-tauri/target/release/bundle/dmg/` |
| `cd src-tauri && cargo run -- --check-db` | Headless: open+migrate the DB at the resolved path, print row counts, exit. No window. |
| `cd src-tauri && cargo run -- --verify-ingest` | Headless: run ingest + .als index + loudness against the resolved DB. **Point `BEATCRATE_DATA_DIR` at a COPY — it writes.** |

> ⚠️ **`cargo tauri dev` does NOT apply the production CSP.** The bundled app applies a stricter CSP than dev — so dev-verification cannot catch CSP-class breakage (see the inline-handler gotcha below). **Always smoke-test the actual `.dmg` bundle before declaring a renderer change done.**

Reinstall after a rebuild: mount the dmg → `ditto /Volumes/BeatCrate/BeatCrate.app /Applications/`.

## DB — path, ownership, and the VST3 constraint

**Single DB at `~/Library/Application Support/BeatCrate/beatcrate.db`.** This is real user data — never delete, truncate, or write test rows into it.

**⚠️ DB path is deliberately NOT the Tauri bundle-id dir.** `db.rs::data_dir()` resolves to `~/Library/Application Support/BeatCrate/` explicitly (or `$BEATCRATE_DATA_DIR` if set). This is load-bearing: the **BeatCrate plugin** (JUCE/C++, in-repo at `plugin/` — builds VST3 + Audio Unit) reads per-track notes from that exact hardcoded path. If you "clean this up" to use Tauri's identifier-derived dir, the plugin reads a stale/empty DB inside the DAW.

**`BEATCRATE_DATA_DIR` is shell-overrideable** for throwaway-DB testing:
```
BEATCRATE_DATA_DIR=/tmp/bc-test cargo run -- --verify-ingest   # writes to the copy, never the live DB
```

## Backend / IPC contract

The renderer talks to Rust via **`invoke()`**, not HTTP — there is no server, no port, no loopback. The bridge lives entirely in `public/app.js`:

- **Central facade (don't bypass it):** the one `api(path, opts)` wrapper dispatches every `/api/...`-style call through the `API_ROUTES` route table to `invoke(command, args)`. To add an endpoint: add a `#[tauri::command]` in `commands.rs`, register it in `lib.rs`'s `invoke_handler!`, and add a route-table entry — don't sprinkle raw `invoke()` calls.
- **File URLs go through `convertFileSrc()`**, not fetch paths: audio (`track_audio_path` → `convertFileSrc`), covers (resolved off `cover_path` in `state.crates`), avatars (`avatar_path` cmd → `convertFileSrc`). The asset protocol is scoped to `$HOME/**` in `tauri.conf.json`.
- **`window.beatcrateNative`** is a shim (defined in app.js) over the dialog + opener plugins: `selectFolder()` → `dialog.open`, `revealPath()` → `opener.revealItemInDir`. Renderer still guards `if (window.beatcrateNative)` (always present under Tauri).
- **Capabilities are least-privilege:** only `core:default` + `dialog:allow-open` + `opener:allow-reveal-item-in-dir`. Adding a plugin API the renderer calls means granting it in `capabilities/default.json` or it's silently denied.

**⚠️ Never `canvas.toDataURL()` / `getImageData()` on a `convertFileSrc` (asset:) image** — it taints the canvas and throws `SecurityError`. (Cost a debug cycle: crate-detail backdrop drew the cover to a canvas; fixed by using the URL directly.)

## Ingestion, .als index, loudness, watcher (all pure-Rust, no Node/ffmpeg)

- **Ingestion** (`ingestion.rs`): `lofty` for track duration. Runs at startup if a Music Folder is configured, and on every watcher event.
- **Content fingerprint — a re-export keeps its row, never its measurements.** A track upsert compares `(file_mtime, file_size)`: changed → overwrite `duration` and set `replay_gain = NULL` (the ingest returns those ids; `lib.rs::after_ingest` re-runs the loudness worker and emits `beatcrate-tracks-changed` so the renderer drops its decoded buffer). NULL stored fingerprint means *unknown* — adopt it, never treat it as changed, or the first ingest after a migration nulls the whole library's gain. Loudness is only ever measured `WHERE replay_gain IS NULL`, so preserving a row without invalidating here leaves the old measurement in place silently.
- **Missing files get a 60s grace, not an instant delete** (`PRUNE_GRACE_SECS`). A DAW re-export unlinks then rewrites; the debounced watcher can scan inside that gap, and deleting there destroys the row plus its cascaded notes/tags/plays — the re-inserted row comes back with NULL `sort_order`, so the track also drops to the bottom of its crate. Absent rows are stamped (`tracks.missing_since` / `crates.emptied_since`), cleared on reappearance, deleted only past the window.
- **.als index** (`als.rs`): `flate2` (gunzip) + `roxmltree` (read-only DOM walk).
- **Loudness** (`loudness.rs`): `symphonia` 0.5 (decode, `features=["all"]` — pinned 0.5; 0.6 is an undocumented rewrite) → `ebur128` (integrated loudness). **No ffmpeg sidecar.**
- **Live watcher** (`lib.rs::start_albums_watcher`): `notify-debouncer-mini`, 1.5s debounce. **Full-rescan-on-event** — it deliberately ignores event paths (FSEvents coalesces/mis-types). Folder renames are handled losslessly inside the ingest by `reconcile_renames` (content/filename-set match → UPDATE in place, preserving crate_id + track ids). The debouncer is `mem::forget`'d on purpose (must live for the app lifetime; no teardown hook). Whole-folder deletion leaves orphan rows (nothing prunes them); only within-folder file removal prunes.

## Design System (renderer — `public/`)

These renderer-side patterns are load-bearing — treat as such.

**Aurora — Honey palette (locked, dark mode only):**
```
--bg:         #060a07      --fg:         #ede5d3
--bg-lift:    #0c1410      --fg-soft:    rgba(237,229,211,0.62)
--accent:     #c8a35a      --fg-faint:   rgba(237,229,211,0.32)
--accent-hi:  #e6c485      --fg-ghost:   rgba(237,229,211,0.10)
--accent-dim: rgba(200,163,90,0.16)   --rule: rgba(237,229,211,0.10)
--topbar:     #080604      --topbar-text: #ede5d3
```
Active styles live in `[data-theme="dark"]` blocks (`<html>` hardcodes `data-theme="dark"`). Dark mode only — no toggle. Dead `:root` light-mode block left for a future cleanup pass. Fonts: Archivo (sans) + JetBrains Mono (mono) via Google Fonts.

**MediaSession + silent-audio focus holder (DO NOT remove — works on WKWebView):** macOS Now Playing / media keys are wired via `navigator.mediaSession`. Because playback uses Web Audio (`AudioBufferSourceNode`), the page loses "media producer" status when Web Audio stops, so the next F8 would route to Apple Music. `ensureMediaFocusAudio()` / `startMediaFocus()` keep a hidden silent looping `<audio>` element alive to hold focus. The `play`/`pause` actions are bound to one toggle (`mkToggle`). Verified on WKWebView (F7/F8/F9 + Now Playing + focus-survives-pause all work). **⚠️ The silent focus element loads from a `blob:` URL (`URL.createObjectURL`), so the CSP's `media-src` MUST include `blob:` (`tauri.conf.json`). Without it, WKWebView refuses to load the element, `startMediaFocus()` swallows the failure (`.play().catch(()=>{})`), and the page never becomes a media producer → F-keys route to Apple Music + nothing shows in the menu bar. This only reproduces in the bundle, not `tauri dev` (which skips the production CSP). Don't drop `blob:` from `media-src`.** The silent audio MUST follow real playback state (`pauseMediaFocus()` on pause, `startMediaFocus()` on resume) or the menu-bar widget shows the wrong play/pause glyph. Don't detach/recreate the element — pause-only is what retains focus.

**Playback — buffer cache:** `cueTrackWithoutPlay()` sets UI + `state.playingTrackId` but fetches no audio; `togglePlay()`'s resume branch checks `audioBufferCache` first and falls back to `loadAndPlay()` if cold. Any new "cue without play" path must respect this.

**Refresh-stats discipline:** any action that changes a tracked count (plays, notes, tags, todos done) must call `refreshStats()` after the mutation or the Home hero column won't tick.

**Aurora glass surfaces** (inspector, popovers, Home cards, search modal): `rgba(8,12,10,0.62)` + `backdrop-filter: blur(24px) saturate(125%)` + 14px radius + honey 1px inset hairline. Match this for new floating surfaces.

**Track tag pills (crate detail) — several load-bearing patterns:** outer pill stays `overflow: visible` so the absolute `×` (top/right −5px) escapes; only the inner `.track-tag-label` ellipsizes. `.track-name` is fixed 260px; `.track-tags` is `flex: 1 1 auto`. Overflow is **dynamic** (`applyTagOverflow()` measures `scrollWidth > clientWidth` and inserts a `+M` glass badge with a hover popover) — no hardcoded pill cap. The `+M` popover has an invisible 12px hover bridge so the cursor gap doesn't dismiss it. The `+` add button is a flex sibling of `.track-tags-chips`, not nested inside.

**Home welcome animation (once per session):** ghosted "Midnight Wax" vinyl (`buildWelcomeVinyl()`, `discOnly`) + word-mask greeting. Two body classes drive it: `body.welcoming` (hides chrome, removed ~3800ms) and `body.mesh-fullscreen` (pins mesh to full viewport, removed ~4700ms). Spin runs on an inner `.welcome-vinyl-spin` wrapper; the outer element only transitions opacity+scale — **don't put `vinyl-spin` and `scale()` on the same element** (rotation keyframes overwrite the scale and it snaps). The mesh wrap is permanently `position: fixed; bottom: 84px` and only animates `bottom` — **never switch it between fixed/absolute** (position isn't transitionable). Re-trigger: `sessionStorage.removeItem('welcomed'); location.reload();`.

**Section titles (Home/Library/Career):** JetBrains Mono uppercase, 44px, weight 400, `letter-spacing: 0.18em`, honey `--accent-hi`. Source text is sentence-case; CSS uppercases. Header bands: `flex; align-items: flex-end; justify-content: space-between; margin-bottom: 22px`.

**View-mode toggle** (`.view-mode-toggle` / `.view-mode-btn`): glass pill, active button solid honey on near-black, weight 700. Home renders it via `renderHomeHero()`; Library inlines `libModeToggleHtml()` — there is no `renderLibraryModeToggle()`.

**Mesh:** animated WebGL shader (`injectMesh(wrapId)`) on Home/Library/Career; the RAF only renders the active view's canvas and polls `clientHeight` each frame to follow the `bottom` transition.

## Settings + Onboarding (renderer)

- **Auto-save, no Save buttons.** `setupSettingsAutoSave()` wires blur+Enter on the name/folder/ableton inputs; saves only when the trimmed value changed. Browse buttons commit on pick (set `_prevValue` so the blur won't double-save). **Don't reintroduce Save buttons.**
- **Onboarding: folder → profile → loadApp.** First launch (`!config.albums_folder`) shows onboarding; after folder pick → `loadApp()`; if no profile name → profile onboarding → **must call `loadApp()` again** (not `showHome()`) so init runs end-to-end (else no mesh until reload).
- Native folder pick / reveal go through `window.beatcrateNative` (the dialog/opener shim) — don't reach for plugin APIs directly from feature code.

## Career Arc — Ableton-specific

The Career Arc view indexes Ableton `.als` projects only. The rest of the app (library, preview, notes, the companion plugin) is DAW-agnostic. Don't imply Career Arc covers other DAWs.

## V1 Scope — Do Not Add
- BPM detection · Waveform display · Cloud sync · Drag-and-drop crate reordering (track reordering within a crate exists).

## Distribution
Ships **unsigned, aarch64-only, local-only.** Username is scrubbed from the binary via `--remap-path-prefix` in the gitignored `src-tauri/.cargo/config.toml` plus `strip = true`. App bundle ID is `com.beatcrate.app`; the plugin is `com.beatcrate.plugin`. Optional follow-up: Developer-ID signing, *iff* distributing beyond a personal machine.

**⚠️ Bundle-only CSP gotcha (cost a debug cycle):** the renderer uses inline `on*=` handlers everywhere (~54 of them). On *bundling* (not dev) Tauri injects a script nonce, and per CSP spec a present nonce makes `'unsafe-inline'` ignored → every inline handler is refused → the app boots but is totally inert. Fix already in place: `app.security.dangerousDisableAssetCspModification: ["script-src","style-src"]` in `tauri.conf.json`. This is why you must smoke-test the real bundle, not just `tauri dev`.

**⚠️ Plugin VST3 codesign seal:** the JUCE Release build signs the `.vst3` then regenerates `Contents/Resources/moduleinfo.json` after, breaking the codesign seal — hosts (e.g. Ableton 12) then silently reject the VST3 on scan. Re-sign after building: `codesign --force --deep --sign - <bundle>`.

---
*Project history, internal audit/port docs, and machine-specific config live in the gitignored `CLAUDE.local.md` (not published).*
