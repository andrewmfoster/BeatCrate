# BeatCrate — Claude Code Instructions

BeatCrate is a personal macOS desktop app for music producers to browse, preview, and annotate their tracks — beats, loops, vocal takes, acoustic demos. Vinyl crate-digging metaphor. Vanilla HTML/CSS/JS frontend + **Rust/Tauri 2 backend + SQLite** (rusqlite, bundled). Single user; ships unsigned, local-only. A companion **VST3 + AU plugin** (`plugin/`, JUCE/C++) surfaces per-track notes inside any DAW.

## Architecture

```
src-tauri/src/main.rs       ← bin entry; --check-db / --verify-ingest go headless, else run()
src-tauri/src/lib.rs        ← Tauri builder: AppState (DB behind a Mutex), startup ingest,
                              folder watcher, the invoke_handler list
src-tauri/src/db.rs         ← SQLite schema + idempotent migrations (rusqlite); DB-PATH override (see §DB)
src-tauri/src/commands.rs   ← ~60 #[tauri::command]s — the renderer's whole API
src-tauri/src/ingestion.rs  ← scans the Music Folder into the DB
src-tauri/src/als.rs        ← Ableton .als project indexer (gunzip + read-only XML DOM walk)
src-tauri/src/loudness.rs   ← integrated-loudness (ebur128) — pure Rust, NO ffmpeg sidecar
src-tauri/tauri.conf.json   ← window, CSP, asset-protocol scope, bundle config
src-tauri/capabilities/     ← least-privilege permission grants (dialog open + opener reveal only)
public/                     ← index.html shell, app.js (ALL frontend logic + IPC facade),
                              style.css, assets/ + uploads/ (uploads/ gitignored)
```

Music source folder (each subfolder = one crate) is stored in the `config` table (`albums_folder`), not hardcoded.

## Running the App

Commands, flags and reinstall: `docs/RENDERER-PATTERNS.md` § Running the App.
The one rule that belongs here, because its failure is silent:

> ⚠️ **`cargo tauri dev` does NOT apply the production CSP.** The bundle applies
> a stricter one, so dev-verification cannot catch CSP-class breakage — and that
> class boots the app *looking fine* and inert (see § Distribution). **Always
> smoke-test the actual `.dmg` before declaring a renderer change done.**
>
> **`--verify-ingest` writes.** Point `BEATCRATE_DATA_DIR` at a COPY.

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

**Visual patterns — palette, glass, tag pills, welcome animation, section
titles, view-mode toggle, mesh — moved to `docs/RENDERER-PATTERNS.md` (09-10),**
because breaking one is *visible*: a wrong colour, a snapped scale, a dismissed
popover. Read it before touching `public/style.css` or the Home/Library/Career
views. What stays here fails **silently**:

**MediaSession + silent-audio focus holder (DO NOT remove):** playback is Web
Audio, so the page stops being a "media producer" the moment it stops and the
next F8 routes to Apple Music. A hidden silent looping `<audio>`
(`ensureMediaFocusAudio()` / `startMediaFocus()`) holds focus. Three silent
traps: **(1)** that element loads from a `blob:` URL, so the CSP's `media-src`
MUST keep `blob:` — without it WKWebView refuses it, `startMediaFocus()`
swallows the failure (`.play().catch(()=>{})`), and the F-keys silently defect;
reproduces in the bundle only, never in `tauri dev`. **(2)** the silent audio
must follow real playback state (`pauseMediaFocus()` / `startMediaFocus()`) or
the menu-bar widget shows the wrong glyph. **(3)** pause it, never
detach/recreate it — pause-only is what retains focus.

**Playback — buffer cache:** `cueTrackWithoutPlay()` sets UI + `state.playingTrackId` but fetches no audio; `togglePlay()`'s resume branch checks `audioBufferCache` first and falls back to `loadAndPlay()` if cold. Any new "cue without play" path must respect this.

**Refresh-stats discipline:** any action that changes a tracked count (plays, notes, tags, todos done) must call `refreshStats()` after the mutation or the Home hero column won't tick.

## Settings + Onboarding (renderer)

- **Onboarding: folder → profile → loadApp.** First launch (`!config.albums_folder`) shows onboarding; after folder pick → `loadApp()`; if no profile name → profile onboarding → **must call `loadApp()` again** (not `showHome()`) so init runs end-to-end (else no mesh until reload).

- **Auto-save, no Save buttons** — `setupSettingsAutoSave()`; the rest of the
  settings/native-shim detail is in `docs/RENDERER-PATTERNS.md`.

## Career Arc — Ableton-specific

The Career Arc view indexes Ableton `.als` projects only. The rest of the app (library, preview, notes, the companion plugin) is DAW-agnostic. Don't imply Career Arc covers other DAWs.

## V1 Scope — Do Not Add
- BPM detection · Waveform display · Cloud sync · Drag-and-drop crate reordering (track reordering within a crate exists).

## Distribution
Ships **unsigned, aarch64-only, local-only.** Username is scrubbed from the binary via `--remap-path-prefix` in the gitignored `src-tauri/.cargo/config.toml` plus `strip = true`. App bundle ID is `com.beatcrate.app`; the plugin is `com.beatcrate.plugin`. Optional follow-up: Developer-ID signing, *iff* distributing beyond a personal machine.

**⚠️ Bundle-only CSP gotcha (cost a debug cycle):** the renderer uses inline `on*=` handlers everywhere (~54 of them). On *bundling* (not dev) Tauri injects a script nonce, and per CSP spec a present nonce makes `'unsafe-inline'` ignored → every inline handler is refused → the app boots but is totally inert. Fix already in place: `app.security.dangerousDisableAssetCspModification: ["script-src","style-src"]` in `tauri.conf.json`. This is why you must smoke-test the real bundle, not just `tauri dev`.

**⚠️ Plugin VST3 codesign seal — fixed in CMakeLists, don't remove:** JUCE signs the `.vst3`, then regenerates `Contents/Resources/moduleinfo.json`, breaking the seal — hosts (Ableton 12) silently reject the plugin on scan, so a broken build looks like one that never appeared. `plugin/CMakeLists.txt` re-seals both bundles in a `POST_BUILD` step. Delete it and every rebuild reships a rejected VST3.

**⚠️ Two download surfaces — update both or neither.** The GitHub release asset must be named `BeatCrate.dmg`: the README links `releases/latest/download/BeatCrate.dmg`, and Tauri's native `BeatCrate_<ver>_aarch64.dmg` silently 404s it. The same dmg also has to be re-uploaded to the Gumroad listing. A stale copy on either surface downloads and runs fine — it just isn't the version you shipped.

---
*History, audit/port docs and machine config: gitignored `CLAUDE.local.md`.*
