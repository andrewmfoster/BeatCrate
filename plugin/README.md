# BeatCrate Plugin

VST3 + Audio Unit plugin that surfaces per-track notes from the [BeatCrate desktop app](../) inside any plugin host. Built primarily for Ableton Live (VST3); the AU build also loads in Logic and GarageBand.

Drop it on a Live track, pick the matching crate + track from the dropdowns, and the notes you wrote in BeatCrate appear right there — tick them off, add new ones, edit text inline. Everything writes back to the same SQLite database the desktop app uses; both stay in sync.

## Requirements

- **macOS on Apple Silicon** (arm64). Intel Macs are not supported. Universal Binary is intentionally out of scope.
- **A VST3 or Audio Unit host.** Tested in Ableton Live 12 (VST3). The AU passes `auval` and loads in Logic / GarageBand.
- **The BeatCrate desktop app**, or at least its database at `~/Library/Application Support/BeatCrate/beatcrate.db`. The plugin reads + writes the same file directly — BeatCrate doesn't have to be running for the plugin to work.

## Install (prebuilt)

If you have prebuilt bundles:

```
cp -R BeatCrate.vst3      ~/Library/Audio/Plug-Ins/VST3/
cp -R BeatCrate.component ~/Library/Audio/Plug-Ins/Components/   # Audio Unit
```

Then rescan your DAW. (In Live: Preferences → Plug-Ins → Rescan. Logic re-validates AUs on launch.)

The bundle is ad-hoc codesigned, which is fine for personal use on your own machine. If macOS Gatekeeper complains the first time you load it, right-click → Open in Finder once, then it'll trust the bundle going forward.

## Build from source

You need **JUCE 8.x** checked out somewhere on disk. The CMake config defaults to a sibling checkout at `../../JUCE` (i.e. `~/Documents/ClaudeCodeVault/JUCE` if BeatCrate lives in `~/Documents/ClaudeCodeVault/BeatCrate`). To use a different path:

```
cd plugin
cmake -B build -DJUCE_PATH=/path/to/JUCE
cmake --build build --config Release
```

Or, if your JUCE checkout is at the expected sibling path:

```
cd plugin
cmake -B build
cmake --build build --config Release
```

`COPY_PLUGIN_AFTER_BUILD TRUE` is set in `CMakeLists.txt`, so a successful build automatically installs both formats — `~/Library/Audio/Plug-Ins/VST3/BeatCrate.vst3` and `~/Library/Audio/Plug-Ins/Components/BeatCrate.component` (AU) — and ad-hoc codesigns them. No manual install step needed. Both formats build from one `juce_add_plugin(... FORMATS AU VST3)` and share the same binary logic.

### After rebuilding

**Live caches plugin binaries per instance.** A successful rebuild copies the new `.vst3`, but any Live project that already has BeatCrate loaded on a track will keep using the old binary until you **remove the plugin instance from that track and re-add it.** Rescanning the VST3 folder is *not* enough. You don't need to restart Live.

## Usage

1. Add **BeatCrate** to any track in Live (it lives under VST3 → BeatCrate).
2. The plugin window opens. If the database is at the expected path, you'll see your crates loaded immediately.
3. Pick a crate from the **CRATE** dropdown, then a track from the **TRACK** dropdown. Notes appear in the **SIDE B / NOTES** list.
4. Click the round dot to mark a note complete. Double-click a note's text to edit it inline — Enter saves, Esc cancels.
5. Add a new note in the "cut a new note…" field at the bottom; Enter or the **PRESS** button submits.

Hit **REFRESH** in the header to pull the latest from the database manually; the plugin also polls every 3 seconds in the background.

### Settings (gear icon)

The gear in the header opens a full-bleed settings panel:

- **Database path** — shows where the plugin is currently reading from. Use **CHANGE…** to pick a different `beatcrate.db` (e.g. if you moved it). **REVEAL IN FINDER** opens the containing folder.
- If the database isn't found at startup, the panel opens automatically with a **LOCATE BEATCRATE.DB…** primary action.

The path persists with the plugin's Live project, alongside the last-selected crate, track, and window size.

### Window

The window is resizable between 400×480 and 700×1100. Drag the corner; the size persists per Live project.

## Architecture

VST3 + Audio Unit plugin written in C++ on JUCE 8, with a vendored SQLite amalgamation (`src/sqlite3.{c,h}` from sqlite.org) for direct DB access. No external dependencies beyond JUCE itself.

```
plugin/
├── CMakeLists.txt
├── src/
│   ├── PluginProcessor.{h,cpp}   audio passthrough + plugin state ser/de
│   ├── PluginEditor.{h,cpp}      main window layout + interactions
│   ├── BeatCrateDB.{h,cpp}       SQLite wrapper — open, queries, writes
│   ├── NoteRow.{h,cpp}           custom row: round-dot check, inline-edit
│   ├── SettingsPanel.{h,cpp}     full-bleed overlay for DB path + future settings
│   ├── Theme.{h,cpp}             LookAndFeel + procedural micro-vinyl Disc
│   └── sqlite3.{c,h}             SQLite amalgamation, vendored
```

The plugin uses the "Vinyl Crate" visual direction — the same Aurora honey palette and vinyl crate-digging metaphor as the desktop app, carried into JUCE via a custom `LookAndFeel` and a procedural micro-vinyl `Disc` in `Theme.{h,cpp}`.

## Concurrent access

The BeatCrate desktop app opens its SQLite database in WAL mode (`PRAGMA journal_mode=WAL`), which lets the plugin read and write while the app is also running without lock contention. The plugin honors the same insert and update SQL the desktop uses, so changes made on either side appear on the other.

## Troubleshooting

**Plugin window says "NO RECORD · LOCATE BEATCRATE.DB"**
The database isn't where the plugin expected. Open settings (gear icon) and use **LOCATE BEATCRATE.DB…** to point it at the file.

**A new note I added in BeatCrate doesn't appear in the plugin**
Wait up to 3 seconds (polling cycle), or hit **REFRESH** in the header. If still missing, check the plugin is pointed at the right database — open settings and verify the path.

**Plugin still looks/behaves like the old version after rebuild**
Remove the plugin instance from the Live track and re-add it. Live caches plugin binaries per instance; rescanning the VST3 folder isn't enough.

**`cmake -B build` fails with "JUCE not found at …"**
Pass `-DJUCE_PATH=/path/to/JUCE` or set the `JUCE_PATH` environment variable. The plugin expects JUCE 8.x.

## License

Personal project. No license file; share at the author's discretion.
