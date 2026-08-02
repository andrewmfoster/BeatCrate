// Tauri app entry. Owns the SQLite connection in managed state (behind a Mutex)
// and registers the command surface (the renderer's whole API).
//
// db_health is a smoke-test command proving the DB opens + the schema applied
// against the real DB file.

mod als;
mod commands;
mod db;
mod ingestion;
mod loudness;

use rusqlite::Connection;
use serde::Serialize;
use std::sync::Mutex;
use tauri::{Emitter, Manager};

/// Push a user-facing notice to the renderer's toast (the `beatcrate-notice`
/// event wired in public/app.js). `kind` is "err" or "info". Best-effort: a
/// failed emit (e.g. the window isn't up yet) is logged, never fatal. This is
/// how background work that has no renderer round-trip — the watcher, the
/// loudness worker — tells the user something went wrong.
pub fn notify(app: &tauri::AppHandle, message: &str, kind: &str) {
    if let Err(e) = app.emit(
        "beatcrate-notice",
        serde_json::json!({ "message": message, "kind": kind }),
    ) {
        eprintln!("[notify] emit failed: {e}");
    }
}

/// Follow-up for an ingest that found changed audio (a track re-exported over its
/// own path). The row survived — its measurements didn't:
///   1. re-measure loudness, since `ingestion` nulled `replay_gain` and the only
///      other trigger is the Settings normalization toggle;
///   2. tell the renderer to drop the decoded AudioBuffers it's holding for those
///      tracks, or a swap made while the app is open keeps playing the old audio
///      from cache for the rest of the session.
/// No-op when nothing changed, which is the overwhelmingly common case.
pub(crate) fn after_ingest(app: &tauri::AppHandle, invalidated: &[i64]) {
    if invalidated.is_empty() {
        return;
    }
    if let Err(e) = app.emit("beatcrate-tracks-changed", serde_json::json!(invalidated)) {
        eprintln!("[ingest] tracks-changed emit failed: {e}");
    }
    let started = commands::start_analysis_if_idle(app);
    println!(
        "[ingest] {} track(s) changed on disk; loudness re-analysis queued for {started}",
        invalidated.len()
    );
}

pub struct AppState {
    pub db: Mutex<Connection>,
    /// Epoch-millis of the last successful ingest. Read by weekly-stats for the
    /// "synced" pill; set by the startup ingest and the watcher. None until the
    /// first scan.
    pub last_sync: Mutex<Option<i64>>,
    /// Background loudness-analysis job state. None until analyze_all is first
    /// invoked.
    pub analysis: Mutex<Option<AnalysisJob>>,
    /// The live folder watcher (M2). Held here — not `mem::forget`'d — so that
    /// changing the Music Folder at runtime can drop the old watcher and start a
    /// fresh one on the new path. None until a folder is configured + watched.
    pub watcher: Mutex<Option<AlbumsWatcher>>,
}

/// Type alias for the debounced filesystem watcher we keep alive in AppState.
pub type AlbumsWatcher =
    notify_debouncer_mini::Debouncer<notify_debouncer_mini::notify::RecommendedWatcher>;

/// Progress of the background loudness-analysis job. Serialized straight to the
/// renderer by analyze_status / analyze_all.
#[derive(Clone, Serialize)]
pub struct AnalysisJob {
    pub running: bool,
    pub total: i64,
    pub completed: i64,
    pub failed: i64,
    pub done: bool,
}

#[derive(Serialize)]
pub struct DbHealth {
    db_path: String,
    crates: i64,
    tracks: i64,
}

/// Smoke test: confirm the DB opened at the expected path and report row counts.
#[tauri::command]
fn db_health(state: tauri::State<AppState>) -> Result<DbHealth, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let crates: i64 = conn
        .query_row("SELECT COUNT(*) FROM crates", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let tracks: i64 = conn
        .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    Ok(DbHealth {
        db_path: db::db_path().to_string_lossy().into_owned(),
        crates,
        tracks,
    })
}

/// Headless schema verification (see main.rs --check-db). Opens + migrates the
/// DB at the resolved path and prints row counts. No window, no Tauri runtime.
pub fn check_db() {
    let conn = db::open_and_init().expect("failed to open/init BeatCrate DB");
    let crates: i64 = conn
        .query_row("SELECT COUNT(*) FROM crates", [], |r| r.get(0))
        .unwrap();
    let tracks: i64 = conn
        .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
        .unwrap();
    let notes: i64 = conn
        .query_row("SELECT COUNT(*) FROM track_notes", [], |r| r.get(0))
        .unwrap();
    let null_sort: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM track_notes WHERE sort_order IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    println!("DB OK at {}", db::db_path().to_string_lossy());
    println!(
        "  crates={crates} tracks={tracks} track_notes={notes} (null sort_order: {null_sort})"
    );
}

/// Headless end-to-end check for task #4 (see main.rs --verify-ingest). Runs
/// against the resolved DB (point BEATCRATE_DATA_DIR at a throwaway *copy* — this
/// writes to crates/tracks/als_* and must never touch the live DB). Exercises
/// ingestion, .als indexing, and a single-track loudness measurement.
pub fn verify_ingest() {
    let mut conn = db::open_and_init().expect("failed to open/init BeatCrate DB");

    let cfg = |conn: &Connection, key: &str| -> Option<String> {
        conn.query_row("SELECT value FROM config WHERE key = ?", [key], |r| {
            r.get(0)
        })
        .ok()
        .filter(|s: &String| !s.is_empty())
    };

    println!("DB at {}", db::db_path().to_string_lossy());

    if let Some(folder) = cfg(&conn, "albums_folder") {
        println!("\n[ingest] {folder}");
        ingestion::ingest_albums_folder(&mut conn, &folder).expect("ingest failed");
        let crates: i64 = conn
            .query_row("SELECT COUNT(*) FROM crates", [], |r| r.get(0))
            .unwrap();
        let tracks: i64 = conn
            .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
            .unwrap();
        let null_dur: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tracks WHERE duration IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        println!("  crates={crates} tracks={tracks} null_duration={null_dur}");
        let mut stmt = conn
            .prepare("SELECT filename, title FROM tracks ORDER BY id LIMIT 5")
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap();
        for r in rows {
            let (f, t) = r.unwrap();
            println!("    {f:<45} → {t}");
        }
    } else {
        println!("[ingest] albums_folder not configured — skipped");
    }

    if let Some(root) = cfg(&conn, "ableton_root") {
        println!("\n[als-index] {root}");
        let (scanned, failed, pruned) =
            als::index_als_root(&mut conn, &root).expect("als index failed");
        let projects: i64 = conn
            .query_row("SELECT COUNT(*) FROM als_project_index", [], |r| r.get(0))
            .unwrap();
        let plugins: i64 = conn
            .query_row("SELECT COUNT(*) FROM als_plugins", [], |r| r.get(0))
            .unwrap();
        let samples: i64 = conn
            .query_row("SELECT COUNT(*) FROM als_samples", [], |r| r.get(0))
            .unwrap();
        let with_bpm: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM als_project_index WHERE bpm IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        println!("  scanned={scanned} failed={} pruned={pruned} | projects={projects} with_bpm={with_bpm} plugins={plugins} samples={samples}", failed.len());
        if !failed.is_empty() {
            println!("  failed files: {}", failed.join(", "));
        }
    } else {
        println!("[als-index] ableton_root not configured — skipped");
    }

    // Loudness smoke test over the first several tracks (compare to the
    // stored replay_gain to confirm the ebur128 result agrees).
    println!("\n[loudness] (ebur128 gain vs stored replay_gain)");
    let samples: Vec<(String, String, Option<f64>)> = {
        let mut stmt = conn
            .prepare(
                "SELECT c.folder, t.filename, t.replay_gain
                 FROM tracks t JOIN crates c ON c.id = t.crate_id ORDER BY t.id LIMIT 8",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    };
    for (folder, filename, live_gain) in samples {
        let path = std::path::Path::new(&folder).join(&filename);
        match loudness::analyze_loudness(&path) {
            Ok(gain) => {
                let live = live_gain
                    .map(|g| format!("{g:+.2}"))
                    .unwrap_or_else(|| "—".into());
                println!("  {filename:<40} rust={gain:+.2}  stored={live}");
            }
            Err(e) => println!("  {filename:<40} FAILED: {e}"),
        }
    }
    println!("\nverify-ingest done.");
}

/// Live folder watcher. On any debounced filesystem change under the Music
/// Folder, re-ingest the whole folder — we deliberately don't trust the event
/// paths (macOS FSEvents coalesces/imprecisely types events); `reconcile_renames`
/// inside the ingest handles folder renames losslessly. The 1.5s debounce is the
/// stability window before re-ingesting.
pub(crate) fn start_albums_watcher(
    app: tauri::AppHandle,
    folder: String,
) -> Result<AlbumsWatcher, Box<dyn std::error::Error>> {
    use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebounceEventResult};
    use std::time::Duration;

    let watch_path = std::path::PathBuf::from(&folder);
    let mut debouncer = new_debouncer(
        Duration::from_millis(1500),
        move |res: DebounceEventResult| {
            if res.is_err() {
                return;
            }
            let state = app.state::<AppState>();
            // H4: if the data dir was removed mid-run, recreate it before the
            // write so SQLite can re-create its WAL/journal files. Full deletion
            // of an in-use DB still needs a restart — the failed-write path below
            // surfaces that to the user via the toast.
            let _ = std::fs::create_dir_all(db::data_dir());
            let result = {
                let mut conn = match state.db.lock() {
                    Ok(c) => c,
                    Err(_) => return,
                };
                ingestion::ingest_albums_folder(&mut conn, &folder)
            };
            match result {
                Ok(invalidated) => {
                    if let Ok(mut ls) = state.last_sync.lock() {
                        *ls = Some(chrono::Local::now().timestamp_millis());
                    }
                    println!("[watcher] re-ingest complete");
                    after_ingest(&app, &invalidated);
                }
                // H5/M5: the busy_timeout handles the common VST3-lock case, but a
                // lock that outlasts it — or an unreadable/missing music folder —
                // still fails here. Tell the user instead of drifting silently.
                Err(e) => {
                    eprintln!("[watcher] re-ingest failed: {e}");
                    notify(
                        &app,
                        "Library sync failed — your music folder may be unavailable or locked. Try Re-scan Library in Settings.",
                        "err",
                    );
                }
            }
        },
    )?;
    debouncer
        .watcher()
        .watch(&watch_path, RecursiveMode::Recursive)?;
    Ok(debouncer)
}

/// Show a native error dialog and quit. Used for unrecoverable startup failures
/// (the window doesn't exist yet, so we can't surface them in the renderer).
fn fatal_startup_error(msg: &str) -> ! {
    eprintln!("[startup] fatal: {msg}");
    // Sanitize for AppleScript string literal embedding.
    let safe = msg.replace('\\', "").replace('"', "'");
    let script = format!(
        "display dialog \"BeatCrate could not start.\n\n{safe}\" \
         with title \"BeatCrate\" buttons {{\"Quit\"}} default button 1 with icon stop"
    );
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .status();
    std::process::exit(1);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut conn = match db::open_and_init() {
        Ok(c) => c,
        Err(e) => fatal_startup_error(&e),
    };

    // Startup ingest: if a Music Folder is already configured, re-scan it now so
    // the library reflects on-disk changes made while the app was closed. Records
    // the last-sync time.
    let mut last_sync = None;
    let configured_folder: Option<String> = conn
        .query_row(
            "SELECT value FROM config WHERE key = 'albums_folder'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok();
    let watch_folder = configured_folder.filter(|f| !f.is_empty());
    // Files re-exported while the app was closed. Acted on in setup() — the
    // follow-up needs an AppHandle, which doesn't exist until the builder runs.
    let mut startup_invalidated: Vec<i64> = Vec::new();
    if let Some(folder) = &watch_folder {
        match ingestion::ingest_albums_folder(&mut conn, folder) {
            Err(e) => eprintln!("[startup-ingest] failed: {e}"),
            Ok(invalidated) => {
                startup_invalidated = invalidated;
                last_sync = Some(chrono::Local::now().timestamp_millis());
            }
        }
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            db: Mutex::new(conn),
            last_sync: Mutex::new(last_sync),
            analysis: Mutex::new(None),
            watcher: Mutex::new(None),
        })
        .setup(move |app| {
            // Asset-protocol scope: the static tauri.conf.json scope only seeds the
            // default data dir (avatars). Add the real data dir (covers a
            // BEATCRATE_DATA_DIR override) and the configured albums folder
            // (covers/audio) at runtime so convertFileSrc() can serve them — this
            // runs before the webview loads any asset. (Audit L7: scope narrowed
            // from $HOME/** to exactly what the app reads.)
            let asset_scope = app.asset_protocol_scope();
            if let Err(e) = asset_scope.allow_directory(db::data_dir(), true) {
                eprintln!("[asset-scope] failed to allow data dir: {e}");
            }
            if let Some(folder) = &watch_folder {
                if let Err(e) = asset_scope.allow_directory(folder, true) {
                    eprintln!("[asset-scope] failed to allow albums folder: {e}");
                }
            }

            // Start the live folder watcher if a Music Folder is configured
            // (set-folder-at-runtime won't start it until next launch).
            // Full-rescan-on-event; rename safety lives in the ingest.
            if let Some(folder) = watch_folder {
                match start_albums_watcher(app.handle().clone(), folder.clone()) {
                    // M2: store the debouncer in AppState (was mem::forget) so a
                    // runtime Music-Folder change can drop it and re-point. Holding
                    // it here keeps it alive for the app lifetime just the same.
                    Ok(debouncer) => {
                        if let Ok(mut w) = app.state::<AppState>().watcher.lock() {
                            *w = Some(debouncer);
                        }
                        println!("[watcher] watching {folder}");
                    }
                    Err(e) => eprintln!("[watcher] failed to start: {e}"),
                }
            }

            // Re-measure anything the startup ingest found changed on disk. (The
            // buffer-cache event it also emits is a no-op this early — nothing is
            // cached yet — but the analysis kick is the point.)
            after_ingest(app.handle(), &startup_invalidated);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            db_health,
            commands::get_config,
            commands::set_ableton_root,
            commands::set_albums_folder,
            commands::rescan_albums,
            commands::reindex_als,
            commands::analyze_all,
            commands::analyze_status,
            commands::set_config_value,
            commands::list_crates,
            commands::get_crate,
            commands::get_crate_tracks,
            commands::set_crate_track_order,
            commands::set_crate_status,
            commands::set_crate_producer,
            commands::get_crate_scratchpad,
            commands::set_crate_scratchpad,
            commands::set_track_favorite,
            commands::add_track_tag,
            commands::remove_track_tag,
            commands::get_track_notes,
            commands::add_track_note,
            commands::update_track_note,
            commands::set_track_notes_order,
            commands::delete_track_note,
            commands::recent_favorites,
            commands::random_track,
            commands::list_favorites,
            commands::list_all_tracks,
            commands::track_audio_path,
            commands::get_about,
            commands::get_track_tags,
            commands::get_scratchpad,
            commands::set_scratchpad,
            commands::get_todos,
            commands::add_todo,
            commands::update_todo_text,
            commands::set_todos_order,
            commands::set_todo_completed,
            commands::delete_todo,
            commands::get_stats,
            commands::get_profile,
            commands::set_profile_name,
            commands::set_profile_avatar,
            commands::avatar_path,
            commands::log_play,
            commands::get_weekly_stats,
            commands::get_alltime_stats,
            commands::search,
            commands::insights_summary,
            commands::insights_timeline,
            commands::insights_plugins,
            commands::insights_day_of_week,
        ])
        .run(tauri::generate_context!())
        .expect("error while running BeatCrate");
}
