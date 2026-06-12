// Tauri command surface — the app-level command surface. Each command is an
// endpoint the renderer invokes.
//
// Payloads are built EXPLICITLY (never a serialized row spread): a SQLite
// INTEGER 0/1 spread into a struct typed Option<bool> fails serde deserialization
// before the command body runs, and an un-try/catch'd invoke() swallows it as a
// silent no-op. Wire shape: 0/1 columns stay as numbers (i64), nullable columns
// become null.
//
// The full command surface (config, crates, tracks, notes/tags/todos, profile,
// loudness analysis, and the ingestion-backed set_albums_folder/rescan_albums)
// is present and registered in lib.rs's invoke_handler.

use crate::{als, ingestion, loudness, AnalysisJob, AppState};
use chrono::{Datelike, Duration, Local, TimeZone};
use rusqlite::{OptionalExtension, Row};
use serde_json::{json, Map, Value};
use tauri::Manager;

type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Stamp `last_sync` to now (M4). Call AFTER releasing the db lock — the watcher
/// locks db then last_sync separately, so we keep that ordering to avoid any
/// lock-order inversion. A poisoned lock is non-fatal: the readout just won't tick.
fn touch_last_sync(state: &AppState) {
    if let Ok(mut ls) = state.last_sync.lock() {
        *ls = Some(Local::now().timestamp_millis());
    }
}

/// Build a comma-separated `?,?,…` placeholder list of length `n` for a SQL
/// `IN (…)` clause. Centralized so the placeholder count can't drift from the
/// bound-parameter count at the call site.
fn sql_placeholders(n: usize) -> String {
    std::iter::repeat_n("?", n).collect::<Vec<_>>().join(",")
}

// ── row → JSON mappers (explicit, column-by-name) ────────────────────────────

fn crate_to_json(r: &Row, with_count: bool) -> rusqlite::Result<Value> {
    let mut o = Map::new();
    o.insert("id".into(), json!(r.get::<_, i64>("id")?));
    o.insert("name".into(), json!(r.get::<_, String>("name")?));
    o.insert("folder".into(), json!(r.get::<_, String>("folder")?));
    o.insert(
        "cover_path".into(),
        json!(r.get::<_, Option<String>>("cover_path")?),
    );
    o.insert("created_at".into(), json!(r.get::<_, i64>("created_at")?));
    o.insert(
        "producer".into(),
        json!(r.get::<_, Option<String>>("producer")?),
    );
    o.insert(
        "scratch_pad".into(),
        json!(r.get::<_, Option<String>>("scratch_pad")?),
    );
    o.insert("status".into(), json!(r.get::<_, String>("status")?));
    if with_count {
        o.insert("track_count".into(), json!(r.get::<_, i64>("track_count")?));
    }
    Ok(Value::Object(o))
}

fn track_to_json(r: &Row) -> rusqlite::Result<Value> {
    let mut o = Map::new();
    o.insert("id".into(), json!(r.get::<_, i64>("id")?));
    o.insert("crate_id".into(), json!(r.get::<_, i64>("crate_id")?));
    o.insert("filename".into(), json!(r.get::<_, String>("filename")?));
    o.insert("title".into(), json!(r.get::<_, String>("title")?));
    o.insert(
        "track_num".into(),
        json!(r.get::<_, Option<i64>>("track_num")?),
    );
    o.insert(
        "duration".into(),
        json!(r.get::<_, Option<f64>>("duration")?),
    );
    o.insert("favorited".into(), json!(r.get::<_, i64>("favorited")?));
    o.insert("created_at".into(), json!(r.get::<_, i64>("created_at")?));
    o.insert("notes".into(), json!(r.get::<_, Option<String>>("notes")?));
    o.insert(
        "favorited_at".into(),
        json!(r.get::<_, Option<i64>>("favorited_at")?),
    );
    o.insert(
        "replay_gain".into(),
        json!(r.get::<_, Option<f64>>("replay_gain")?),
    );
    o.insert(
        "sort_order".into(),
        json!(r.get::<_, Option<i64>>("sort_order")?),
    );
    Ok(Value::Object(o))
}

fn note_to_json(r: &Row) -> rusqlite::Result<Value> {
    let mut o = Map::new();
    o.insert("id".into(), json!(r.get::<_, i64>("id")?));
    o.insert("track_id".into(), json!(r.get::<_, i64>("track_id")?));
    o.insert("note".into(), json!(r.get::<_, String>("note")?));
    o.insert("created_at".into(), json!(r.get::<_, i64>("created_at")?));
    o.insert(
        "sort_order".into(),
        json!(r.get::<_, Option<i64>>("sort_order")?),
    );
    o.insert("completed".into(), json!(r.get::<_, i64>("completed")?));
    Ok(Value::Object(o))
}

// ── config ───────────────────────────────────────────────────────────────────

/// GET /api/config — all config key/values as an object.
#[tauri::command]
pub fn get_config(state: tauri::State<AppState>) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    let mut stmt = conn.prepare("SELECT key, value FROM config").map_err(err)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(err)?;
    let mut o = Map::new();
    for row in rows {
        let (k, v) = row.map_err(err)?;
        o.insert(k, json!(v));
    }
    Ok(Value::Object(o))
}

/// POST /api/config/ableton-root — persist the Ableton projects root.
#[tauri::command]
pub fn set_ableton_root(state: tauri::State<AppState>, path: String) -> CmdResult<Value> {
    if path.is_empty() {
        return Err("path is required".into());
    }
    let conn = state.db.lock().map_err(err)?;
    conn.execute(
        "INSERT OR REPLACE INTO config (key, value) VALUES ('ableton_root', ?)",
        [&path],
    )
    .map_err(err)?;
    Ok(json!({ "ok": true, "ableton_root": path }))
}

/// Generic config key/value setter — replaces the per-toggle POST endpoints
/// (normalization, crates-view, todo/scratchpad/timeline-collapsed, library-mode).
/// Validation mirrors the originals; unknown keys are rejected so the config
/// table can't accumulate garbage.
#[tauri::command]
pub fn set_config_value(
    state: tauri::State<AppState>,
    key: String,
    value: String,
) -> CmdResult<Value> {
    let valid = match key.as_str() {
        "normalization_enabled"
        | "todo_collapsed"
        | "scratchpad_collapsed"
        | "timeline_collapsed" => value == "0" || value == "1",
        // The renderer persists the active crate-grid filter here.
        // PAIRED EDIT: this value set mirrors CRATES_VIEW_MODES in public/app.js;
        // change both together.
        "crates_view_mode" => matches!(
            value.as_str(),
            "all" | "released" | "unreleased" | "shelved"
        ),
        "library_mode" => value == "crates" || value == "songs",
        _ => false,
    };
    if !valid {
        return Err(format!("invalid config key/value: {key}={value}"));
    }
    let conn = state.db.lock().map_err(err)?;
    conn.execute(
        "INSERT OR REPLACE INTO config (key, value) VALUES (?, ?)",
        rusqlite::params![key, value],
    )
    .map_err(err)?;
    Ok(json!({ "ok": true }))
}

// ── crates ─────────────────────────────────────────────────────────────────

/// GET /api/crates — all crates with track_count, ordered by name.
#[tauri::command]
pub fn list_crates(state: tauri::State<AppState>) -> CmdResult<Vec<Value>> {
    let conn = state.db.lock().map_err(err)?;
    let mut stmt = conn
        .prepare(
            "SELECT c.*, COUNT(t.id) AS track_count
             FROM crates c
             LEFT JOIN tracks t ON t.crate_id = c.id
             GROUP BY c.id
             ORDER BY c.name",
        )
        .map_err(err)?;
    let rows = stmt
        .query_map([], |r| crate_to_json(r, true))
        .map_err(err)?;
    let out = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?;
    Ok(out)
}

/// GET /api/crates/:id — single crate.
#[tauri::command]
pub fn get_crate(state: tauri::State<AppState>, id: i64) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    conn.query_row("SELECT * FROM crates WHERE id = ?", [id], |r| {
        crate_to_json(r, false)
    })
    .map_err(|_| "Not found".to_string())
}

/// GET /api/crates/:id/tracks — tracks with their tags + notes arrays attached.
#[tauri::command]
pub fn get_crate_tracks(state: tauri::State<AppState>, id: i64) -> CmdResult<Vec<Value>> {
    let conn = state.db.lock().map_err(err)?;

    let mut tracks: Vec<Value> = {
        let mut stmt = conn
            .prepare(
                "SELECT * FROM tracks WHERE crate_id = ?
                 ORDER BY sort_order IS NULL, sort_order, track_num, filename",
            )
            .map_err(err)?;
        let rows = stmt.query_map([id], track_to_json).map_err(err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?
    };
    if tracks.is_empty() {
        return Ok(vec![]);
    }

    // tags per track
    let mut tags_by: std::collections::HashMap<i64, Vec<Value>> = std::collections::HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT track_id, tag FROM track_tags
                 WHERE track_id IN (SELECT id FROM tracks WHERE crate_id = ?)",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(err)?;
        for row in rows {
            let (tid, tag) = row.map_err(err)?;
            tags_by.entry(tid).or_default().push(json!(tag));
        }
    }

    // notes per track (sort_order ASC, created_at ASC — matches the original)
    let mut notes_by: std::collections::HashMap<i64, Vec<Value>> = std::collections::HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT id, track_id, note, created_at, sort_order, completed
                 FROM track_notes
                 WHERE track_id IN (SELECT id FROM tracks WHERE crate_id = ?)
                 ORDER BY sort_order ASC, created_at ASC",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map([id], |r| {
                Ok((r.get::<_, i64>("track_id")?, note_to_json(r)?))
            })
            .map_err(err)?;
        for row in rows {
            let (tid, note) = row.map_err(err)?;
            notes_by.entry(tid).or_default().push(note);
        }
    }

    for t in tracks.iter_mut() {
        let tid = t.get("id").and_then(|v| v.as_i64()).unwrap();
        let obj = t.as_object_mut().unwrap();
        obj.insert(
            "tags".into(),
            json!(tags_by.remove(&tid).unwrap_or_default()),
        );
        obj.insert(
            "notes".into(),
            json!(notes_by.remove(&tid).unwrap_or_default()),
        );
    }
    Ok(tracks)
}

/// PUT /api/crates/:id/tracks/order — persist a new track order for a crate.
#[tauri::command]
pub fn set_crate_track_order(
    state: tauri::State<AppState>,
    id: i64,
    order: Vec<i64>,
) -> CmdResult<Value> {
    let mut conn = state.db.lock().map_err(err)?;
    let tx = conn.transaction().map_err(err)?;
    for (i, track_id) in order.iter().enumerate() {
        let affected = tx
            .execute(
                "UPDATE tracks SET sort_order = ? WHERE id = ? AND crate_id = ?",
                rusqlite::params![i as i64, track_id, id],
            )
            .map_err(err)?;
        // A foreign/stale track_id matches no row — reject the whole reorder
        // (the transaction rolls back) rather than silently committing a partial
        // order. The renderer only ever sends this crate's own track ids.
        if affected == 0 {
            return Err(format!("track {track_id} does not belong to crate {id}"));
        }
    }
    tx.commit().map_err(err)?;
    Ok(json!({ "ok": true }))
}

/// PATCH /api/crates/:id/status — set release status, return the updated crate.
#[tauri::command]
pub fn set_crate_status(
    state: tauri::State<AppState>,
    id: i64,
    status: String,
) -> CmdResult<Value> {
    if !["unreleased", "released", "shelved"].contains(&status.as_str()) {
        return Err("status must be unreleased, released, or shelved".into());
    }
    let conn = state.db.lock().map_err(err)?;
    conn.execute(
        "UPDATE crates SET status = ? WHERE id = ?",
        rusqlite::params![status, id],
    )
    .map_err(err)?;
    conn.query_row("SELECT * FROM crates WHERE id = ?", [id], |r| {
        crate_to_json(r, false)
    })
    .map_err(err)
}

/// PATCH /api/crates/:id/producer — set/clear the producer (empty → NULL).
#[tauri::command]
pub fn set_crate_producer(
    state: tauri::State<AppState>,
    id: i64,
    producer: Option<String>,
) -> CmdResult<Value> {
    let value = producer.filter(|p| !p.is_empty());
    let conn = state.db.lock().map_err(err)?;
    conn.execute(
        "UPDATE crates SET producer = ? WHERE id = ?",
        rusqlite::params![value, id],
    )
    .map_err(err)?;
    Ok(json!({ "ok": true }))
}

/// GET /api/crates/:id/scratchpad
#[tauri::command]
pub fn get_crate_scratchpad(state: tauri::State<AppState>, id: i64) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    let content: Option<String> = conn
        .query_row("SELECT scratch_pad FROM crates WHERE id = ?", [id], |r| {
            r.get(0)
        })
        .map_err(|_| "Not found".to_string())?;
    Ok(json!({ "content": content.unwrap_or_default() }))
}

/// PUT /api/crates/:id/scratchpad
#[tauri::command]
pub fn set_crate_scratchpad(
    state: tauri::State<AppState>,
    id: i64,
    content: String,
) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    conn.execute(
        "UPDATE crates SET scratch_pad = ? WHERE id = ?",
        rusqlite::params![content, id],
    )
    .map_err(err)?;
    Ok(json!({ "ok": true }))
}

// ── tracks ───────────────────────────────────────────────────────────────────

/// PATCH /api/tracks/:id/favorite — toggle favorite, stamping favorited_at.
#[tauri::command]
pub fn set_track_favorite(
    state: tauri::State<AppState>,
    id: i64,
    favorited: bool,
) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    if favorited {
        conn.execute(
            "UPDATE tracks SET favorited = 1, favorited_at = unixepoch() WHERE id = ?",
            [id],
        )
        .map_err(err)?;
    } else {
        conn.execute(
            "UPDATE tracks SET favorited = 0, favorited_at = NULL WHERE id = ?",
            [id],
        )
        .map_err(err)?;
    }
    Ok(json!({ "ok": true }))
}

/// POST /api/tracks/:id/tags
#[tauri::command]
pub fn add_track_tag(state: tauri::State<AppState>, id: i64, tag: String) -> CmdResult<Value> {
    let tag = tag.trim().to_string();
    if tag.is_empty() {
        return Err("tag is required".into());
    }
    let conn = state.db.lock().map_err(err)?;
    conn.execute(
        "INSERT OR IGNORE INTO track_tags (track_id, tag, created_at) VALUES (?, ?, unixepoch())",
        rusqlite::params![id, tag],
    )
    .map_err(err)?;
    Ok(json!({ "ok": true }))
}

/// DELETE /api/tracks/:id/tags/:tag
#[tauri::command]
pub fn remove_track_tag(state: tauri::State<AppState>, id: i64, tag: String) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    conn.execute(
        "DELETE FROM track_tags WHERE track_id = ? AND tag = ?",
        rusqlite::params![id, tag],
    )
    .map_err(err)?;
    Ok(json!({ "ok": true }))
}

/// GET /api/tracks/:id/notes — ORDER BY sort_order ASC, id ASC (matches plugin).
#[tauri::command]
pub fn get_track_notes(state: tauri::State<AppState>, id: i64) -> CmdResult<Vec<Value>> {
    let conn = state.db.lock().map_err(err)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, track_id, note, created_at, sort_order, completed
             FROM track_notes WHERE track_id = ?
             ORDER BY sort_order ASC, id ASC",
        )
        .map_err(err)?;
    let rows = stmt.query_map([id], note_to_json).map_err(err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)
}

/// POST /api/tracks/:id/notes — append with MAX(sort_order)+1.
/// The MAX+1 contract depends on no NULL sort_order rows (CLAUDE.md / plugin).
#[tauri::command]
pub fn add_track_note(state: tauri::State<AppState>, id: i64, note: String) -> CmdResult<Value> {
    let note = note.trim().to_string();
    if note.is_empty() {
        return Err("note is required".into());
    }
    let conn = state.db.lock().map_err(err)?;
    let max: Option<i64> = conn
        .query_row(
            "SELECT MAX(sort_order) FROM track_notes WHERE track_id = ?",
            [id],
            |r| r.get(0),
        )
        .map_err(err)?;
    let next_order = max.unwrap_or(-1) + 1;
    conn.execute(
        "INSERT INTO track_notes (track_id, note, sort_order) VALUES (?, ?, ?)",
        rusqlite::params![id, note, next_order],
    )
    .map_err(err)?;
    Ok(json!({ "ok": true, "id": conn.last_insert_rowid(), "sort_order": next_order }))
}

/// PATCH /api/tracks/:id/notes/:noteId — either flips `completed` or edits text.
#[tauri::command]
pub fn update_track_note(
    state: tauri::State<AppState>,
    id: i64,
    note_id: i64,
    note: Option<String>,
    completed: Option<bool>,
) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    if let Some(done) = completed {
        let affected = conn
            .execute(
                "UPDATE track_notes SET completed = ? WHERE id = ? AND track_id = ?",
                rusqlite::params![done as i64, note_id, id],
            )
            .map_err(err)?;
        // M6: a stale/foreign note_id matches no row — say so instead of {ok:true}.
        if affected == 0 {
            return Err(format!("note {note_id} not found on track {id}"));
        }
        return Ok(json!({ "ok": true }));
    }
    let text = note.unwrap_or_default().trim().to_string();
    if text.is_empty() {
        return Err("note is required".into());
    }
    let affected = conn
        .execute(
            "UPDATE track_notes SET note = ? WHERE id = ? AND track_id = ?",
            rusqlite::params![text, note_id, id],
        )
        .map_err(err)?;
    // M6: a stale/foreign note_id matches no row — surface it, don't fake success.
    if affected == 0 {
        return Err(format!("note {note_id} not found on track {id}"));
    }
    Ok(json!({ "ok": true, "note": text }))
}

/// PUT /api/tracks/:id/notes/order — [{ id, sort_order }, ...]
#[tauri::command]
pub fn set_track_notes_order(
    state: tauri::State<AppState>,
    id: i64,
    items: Vec<NoteOrder>,
) -> CmdResult<Value> {
    let mut conn = state.db.lock().map_err(err)?;
    let tx = conn.transaction().map_err(err)?;
    for it in &items {
        let affected = tx
            .execute(
                "UPDATE track_notes SET sort_order = ? WHERE id = ? AND track_id = ?",
                rusqlite::params![it.sort_order, it.id, id],
            )
            .map_err(err)?;
        // M6: a foreign/stale note id matches no row — reject the whole reorder
        // (tx rolls back) rather than committing a partial order.
        if affected == 0 {
            return Err(format!("note {} does not belong to track {id}", it.id));
        }
    }
    tx.commit().map_err(err)?;
    Ok(json!({ "ok": true }))
}

#[derive(serde::Deserialize)]
pub struct NoteOrder {
    pub id: i64,
    pub sort_order: i64,
}

/// DELETE /api/tracks/:id/notes/:noteId
#[tauri::command]
pub fn delete_track_note(state: tauri::State<AppState>, id: i64, note_id: i64) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    conn.execute(
        "DELETE FROM track_notes WHERE id = ? AND track_id = ?",
        rusqlite::params![note_id, id],
    )
    .map_err(err)?;
    Ok(json!({ "ok": true }))
}

/// GET /api/tracks/favorites/recent — N most recently starred (default 3, cap 20).
#[tauri::command]
pub fn recent_favorites(
    state: tauri::State<AppState>,
    limit: Option<i64>,
) -> CmdResult<Vec<Value>> {
    let limit = limit.unwrap_or(3).clamp(1, 20);
    let conn = state.db.lock().map_err(err)?;
    let mut stmt = conn
        .prepare(
            "SELECT t.id, t.title, t.crate_id, t.favorited_at, c.name AS crate_name
             FROM tracks t JOIN crates c ON c.id = t.crate_id
             WHERE t.favorited = 1
             ORDER BY t.favorited_at DESC, t.id DESC
             LIMIT ?",
        )
        .map_err(err)?;
    let rows = stmt
        .query_map([limit], |r| {
            Ok(json!({
                "id": r.get::<_, i64>("id")?,
                "title": r.get::<_, String>("title")?,
                "crate_id": r.get::<_, i64>("crate_id")?,
                "favorited_at": r.get::<_, Option<i64>>("favorited_at")?,
                "crate_name": r.get::<_, String>("crate_name")?,
            }))
        })
        .map_err(err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)
}

/// GET /api/tracks/random — one random track with its crate name.
#[tauri::command]
pub fn random_track(state: tauri::State<AppState>) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    conn.query_row(
        "SELECT t.*, c.name AS crate_name
         FROM tracks t JOIN crates c ON c.id = t.crate_id
         ORDER BY RANDOM() LIMIT 1",
        [],
        |r| {
            let mut v = track_to_json(r)?;
            v.as_object_mut().unwrap().insert(
                "crate_name".into(),
                json!(r.get::<_, String>("crate_name")?),
            );
            Ok(v)
        },
    )
    .map_err(|_| "no tracks".to_string())
}

/// GET /api/tracks/favorites — all favorited tracks with crate name.
#[tauri::command]
pub fn list_favorites(state: tauri::State<AppState>) -> CmdResult<Vec<Value>> {
    let conn = state.db.lock().map_err(err)?;
    let mut stmt = conn
        .prepare(
            "SELECT t.*, c.name AS crate_name
             FROM tracks t JOIN crates c ON c.id = t.crate_id
             WHERE t.favorited = 1
             ORDER BY c.name, t.track_num",
        )
        .map_err(err)?;
    let rows = stmt
        .query_map([], |r| {
            let mut v = track_to_json(r)?;
            v.as_object_mut().unwrap().insert(
                "crate_name".into(),
                json!(r.get::<_, String>("crate_name")?),
            );
            Ok(v)
        })
        .map_err(err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)
}

/// GET /api/tracks/all — flat track list (subset cols) + crate name + tags.
#[tauri::command]
pub fn list_all_tracks(state: tauri::State<AppState>) -> CmdResult<Vec<Value>> {
    let conn = state.db.lock().map_err(err)?;
    let mut tracks: Vec<Value> = {
        let mut stmt = conn
            .prepare(
                "SELECT t.id, t.title, t.crate_id, c.name AS crate_name,
                        t.duration, t.sort_order, t.favorited
                 FROM tracks t JOIN crates c ON c.id = t.crate_id
                 ORDER BY c.name ASC, t.sort_order ASC",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>("id")?,
                    "title": r.get::<_, String>("title")?,
                    "crate_id": r.get::<_, i64>("crate_id")?,
                    "crate_name": r.get::<_, String>("crate_name")?,
                    "duration": r.get::<_, Option<f64>>("duration")?,
                    "sort_order": r.get::<_, Option<i64>>("sort_order")?,
                    "favorited": r.get::<_, i64>("favorited")?,
                }))
            })
            .map_err(err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?
    };

    let mut tags_by: std::collections::HashMap<i64, Vec<Value>> = std::collections::HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT track_id, tag FROM track_tags ORDER BY track_id, created_at")
            .map_err(err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(err)?;
        for row in rows {
            let (tid, tag) = row.map_err(err)?;
            tags_by.entry(tid).or_default().push(json!(tag));
        }
    }
    for t in tracks.iter_mut() {
        let tid = t.get("id").and_then(|v| v.as_i64()).unwrap();
        t.as_object_mut().unwrap().insert(
            "tags".into(),
            json!(tags_by.remove(&tid).unwrap_or_default()),
        );
    }
    Ok(tracks)
}

/// Absolute on-disk path for a track's audio file (folder + filename). The
/// renderer feeds this to convertFileSrc() to load the audio directly off disk.
#[tauri::command]
pub fn track_audio_path(state: tauri::State<AppState>, id: i64) -> CmdResult<String> {
    let conn = state.db.lock().map_err(err)?;
    let (folder, filename): (String, String) = conn
        .query_row(
            "SELECT c.folder, t.filename FROM tracks t
             JOIN crates c ON c.id = t.crate_id WHERE t.id = ?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| "track not found".to_string())?;
    let path = std::path::Path::new(&folder).join(&filename);
    // M7: the file may have been moved/deleted since the last scan. Returning a
    // dead path made the renderer bail silently (no audio, stale transport UI);
    // an explicit error lets it show a toast and reset the player (see M11).
    if !path.exists() {
        return Err(format!("Audio file not found: {filename}"));
    }
    Ok(path.to_string_lossy().into_owned())
}

// ── app-level: about / tags / scratchpad ─────────────────────────────────────

/// GET /api/about — app version + resolved data dir (for Settings "About").
#[tauri::command]
pub fn get_about() -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "dataDir": crate::db::data_dir().to_string_lossy(),
    })
}

/// GET /api/tags/track — all distinct track tags, alphabetical.
#[tauri::command]
pub fn get_track_tags(state: tauri::State<AppState>) -> CmdResult<Vec<String>> {
    let conn = state.db.lock().map_err(err)?;
    let mut stmt = conn
        .prepare("SELECT DISTINCT tag FROM track_tags ORDER BY tag ASC")
        .map_err(err)?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)
}

/// GET /api/scratchpad
#[tauri::command]
pub fn get_scratchpad(state: tauri::State<AppState>) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    let content: Option<String> = conn
        .query_row("SELECT content FROM scratchpad WHERE id = 1", [], |r| {
            r.get(0)
        })
        .ok();
    Ok(json!({ "content": content.unwrap_or_default() }))
}

/// PUT /api/scratchpad
#[tauri::command]
pub fn set_scratchpad(state: tauri::State<AppState>, content: String) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    conn.execute(
        "INSERT OR REPLACE INTO scratchpad (id, content) VALUES (1, ?)",
        [&content],
    )
    .map_err(err)?;
    Ok(json!({ "ok": true }))
}

// ── todos ────────────────────────────────────────────────────────────────────

/// GET /api/todos — incomplete only.
#[tauri::command]
pub fn get_todos(state: tauri::State<AppState>) -> CmdResult<Vec<Value>> {
    let conn = state.db.lock().map_err(err)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, text, completed, created_at, sort_order, completed_at
             FROM todos WHERE completed = 0 ORDER BY sort_order ASC, created_at ASC",
        )
        .map_err(err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, i64>("id")?,
                "text": r.get::<_, String>("text")?,
                "completed": r.get::<_, i64>("completed")?,
                "created_at": r.get::<_, i64>("created_at")?,
                "sort_order": r.get::<_, Option<i64>>("sort_order")?,
                "completed_at": r.get::<_, Option<i64>>("completed_at")?,
            }))
        })
        .map_err(err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)
}

/// POST /api/todos — append with MAX(sort_order)+1 over incomplete rows.
#[tauri::command]
pub fn add_todo(state: tauri::State<AppState>, text: String) -> CmdResult<Value> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("text required".into());
    }
    let conn = state.db.lock().map_err(err)?;
    let max: Option<i64> = conn
        .query_row(
            "SELECT MAX(sort_order) FROM todos WHERE completed = 0",
            [],
            |r| r.get(0),
        )
        .map_err(err)?;
    let next_order = max.unwrap_or(-1) + 1;
    conn.execute(
        "INSERT INTO todos (text, sort_order) VALUES (?, ?)",
        rusqlite::params![text, next_order],
    )
    .map_err(err)?;
    Ok(json!({
        "id": conn.last_insert_rowid(),
        "text": text,
        "completed": 0,
        "sort_order": next_order,
    }))
}

/// PATCH /api/todos/:id/text
#[tauri::command]
pub fn update_todo_text(state: tauri::State<AppState>, id: i64, text: String) -> CmdResult<Value> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("text required".into());
    }
    let conn = state.db.lock().map_err(err)?;
    let affected = conn
        .execute(
            "UPDATE todos SET text = ? WHERE id = ?",
            rusqlite::params![text, id],
        )
        .map_err(err)?;
    if affected == 0 {
        return Err(format!("todo {id} not found")); // M6: don't report success on a stale id.
    }
    Ok(json!({ "ok": true, "text": text }))
}

/// PUT /api/todos/order — [{ id, sort_order }, ...]
#[tauri::command]
pub fn set_todos_order(state: tauri::State<AppState>, items: Vec<NoteOrder>) -> CmdResult<Value> {
    let mut conn = state.db.lock().map_err(err)?;
    let tx = conn.transaction().map_err(err)?;
    for it in &items {
        let affected = tx
            .execute(
                "UPDATE todos SET sort_order = ? WHERE id = ?",
                rusqlite::params![it.sort_order, it.id],
            )
            .map_err(err)?;
        // M6: a stale todo id matches no row — reject the whole reorder (rollback)
        // rather than committing a partial order.
        if affected == 0 {
            return Err(format!("todo {} not found", it.id));
        }
    }
    tx.commit().map_err(err)?;
    Ok(json!({ "ok": true }))
}

/// PATCH /api/todos/:id — toggle completed, stamping completed_at.
#[tauri::command]
pub fn set_todo_completed(
    state: tauri::State<AppState>,
    id: i64,
    completed: bool,
) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    if completed {
        conn.execute(
            "UPDATE todos SET completed = 1, completed_at = unixepoch() WHERE id = ?",
            [id],
        )
        .map_err(err)?;
    } else {
        conn.execute(
            "UPDATE todos SET completed = 0, completed_at = NULL WHERE id = ?",
            [id],
        )
        .map_err(err)?;
    }
    Ok(json!({ "ok": true }))
}

/// DELETE /api/todos/:id
#[tauri::command]
pub fn delete_todo(state: tauri::State<AppState>, id: i64) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    conn.execute("DELETE FROM todos WHERE id = ?", [id])
        .map_err(err)?;
    Ok(json!({ "ok": true }))
}

// ── stats / profile / play log ────────────────────────────────────────────────

/// GET /api/stats — headline counts for Home.
#[tauri::command]
pub fn get_stats(state: tauri::State<AppState>) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    let one = |sql: &str| -> rusqlite::Result<i64> { conn.query_row(sql, [], |r| r.get(0)) };
    Ok(json!({
        "crates": one("SELECT COUNT(*) FROM crates").map_err(err)?,
        "beats": one("SELECT COUNT(*) FROM tracks").map_err(err)?,
        "tags": one("SELECT COUNT(DISTINCT tag) FROM track_tags").map_err(err)?,
        "notes": one("SELECT COUNT(*) FROM track_notes").map_err(err)?,
        "todos": one("SELECT COUNT(*) FROM todos WHERE completed = 0").map_err(err)?,
        "released": one("SELECT COUNT(*) FROM crates WHERE status = 'released'").map_err(err)?,
    }))
}

/// GET /api/profile
#[tauri::command]
pub fn get_profile(state: tauri::State<AppState>) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    conn.query_row(
        "SELECT name, avatar_path FROM user_profile WHERE id = 1",
        [],
        |r| {
            Ok(json!({
                "name": r.get::<_, String>("name")?,
                "avatar_path": r.get::<_, Option<String>>("avatar_path")?,
            }))
        },
    )
    .or_else(|_| Ok(json!({ "name": "", "avatar_path": null })))
}

/// PUT /api/profile — set display name.
#[tauri::command]
pub fn set_profile_name(state: tauri::State<AppState>, name: String) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    conn.execute("UPDATE user_profile SET name = ? WHERE id = 1", [&name])
        .map_err(err)?;
    Ok(json!({ "ok": true }))
}

/// M5: sniff an image's magic bytes and confirm they match the declared format.
/// Lightweight on purpose (no image-crate dep) — just enough to reject a
/// mislabeled or non-image blob before it's written to uploads/.
fn bytes_match_format(b: &[u8], ext_raw: &str) -> bool {
    match ext_raw {
        "png" => b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        "jpg" | "jpeg" => b.starts_with(&[0xFF, 0xD8, 0xFF]),
        "gif" => b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a"),
        "webp" => b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP",
        _ => false,
    }
}

/// POST /api/profile/avatar — decode a data-URL image, write it under the data
/// dir's uploads/ (NOT a repo path — that lands read-only in the bundle), and
/// store the /uploads/<file> reference. Mirrors server/index.js.
#[tauri::command]
pub fn set_profile_avatar(state: tauri::State<AppState>, data_url: String) -> CmdResult<Value> {
    use base64::Engine;
    const PREFIX: &str = "data:image/";
    const MARKER: &str = ";base64,";
    if !data_url.starts_with(PREFIX) {
        return Err("invalid image data".into());
    }
    let marker_pos = data_url.find(MARKER).ok_or("invalid image data")?;
    let ext_raw = &data_url[PREFIX.len()..marker_pos];
    if !["jpeg", "jpg", "png", "gif", "webp"].contains(&ext_raw) {
        return Err("invalid image data".into());
    }
    let ext = if ext_raw == "jpeg" { "jpg" } else { ext_raw };
    let b64 = &data_url[marker_pos + MARKER.len()..];

    // M5: cap the payload. An avatar is tiny; bound it so a renderer bug or a
    // pasted giant data-URL can't decode an arbitrary blob into memory/disk.
    // Check the encoded length first (≈4 base64 chars per 3 bytes) to reject
    // before allocating the decode buffer, then re-check the exact decoded size.
    const MAX_AVATAR_BYTES: usize = 8 * 1024 * 1024; // 8 MB decoded
    if b64.len() / 4 * 3 > MAX_AVATAR_BYTES + 1024 {
        return Err("image is too large (max 8 MB)".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| {
            eprintln!("[avatar] base64 decode failed: {e}");
            "invalid image data".to_string()
        })?;
    if bytes.len() > MAX_AVATAR_BYTES {
        return Err("image is too large (max 8 MB)".into());
    }
    // M5: validate the bytes actually are the declared format (magic-byte sniff),
    // not just a trusted MIME label on arbitrary content.
    if !bytes_match_format(&bytes, ext_raw) {
        return Err("invalid image data".into());
    }

    let uploads = crate::db::data_dir().join("uploads");
    std::fs::create_dir_all(&uploads).map_err(err)?;
    let filename = format!("avatar_{}.{}", Local::now().timestamp_millis(), ext);
    std::fs::write(uploads.join(&filename), &bytes).map_err(|e| format!("write failed: {e}"))?;

    let avatar_path = format!("/uploads/{filename}");
    let conn = state.db.lock().map_err(err)?;
    conn.execute(
        "UPDATE user_profile SET avatar_path = ? WHERE id = 1",
        [&avatar_path],
    )
    .map_err(err)?;
    Ok(json!({ "avatar_path": avatar_path }))
}

/// Reduce an avatar reference ("/uploads/<file>" or a bare name) to a safe
/// basename that can't escape the uploads/ dir. Rejects empty names and the
/// `.`/`..` traversal components — the basename-strip alone would let a stored
/// "/uploads/.." resolve to the parent (data_dir itself).
fn path_basename(reference: &str) -> Result<&str, String> {
    let name = reference.rsplit('/').next().unwrap_or(reference);
    if name.is_empty() || name == "." || name == ".." || name.contains('\\') || name.contains('/') {
        return Err("invalid avatar filename".into());
    }
    Ok(name)
}

/// Absolute path for an avatar reference ("/uploads/<file>") so the renderer can
/// convertFileSrc() it — replaces GET /uploads/:filename.
#[tauri::command]
pub fn avatar_path(filename: String) -> CmdResult<String> {
    // Accept either a bare filename or a "/uploads/<file>" reference.
    let name = path_basename(&filename)?;
    Ok(crate::db::data_dir()
        .join("uploads")
        .join(name)
        .to_string_lossy()
        .into_owned())
}

/// POST /api/tracks/:id/play — log a play event.
#[tauri::command]
pub fn log_play(state: tauri::State<AppState>, id: i64) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    conn.execute("INSERT INTO play_log (track_id) VALUES (?)", [id])
        .map_err(err)?;
    Ok(json!({ "ok": true }))
}

// ── weekly / all-time stats ───────────────────────────────────────────────────

/// Resolve a local wall-clock `NaiveDateTime` to an epoch timestamp without
/// panicking on DST transitions. Ambiguous (fall-back) hour → earliest instant;
/// nonexistent (spring-forward gap) → the UTC interpretation (a harmless ~1h
/// skew — these bounds only window play-count queries).
fn local_naive_to_ts(naive: chrono::NaiveDateTime) -> i64 {
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => dt.timestamp(),
        chrono::LocalResult::Ambiguous(earliest, _) => earliest.timestamp(),
        chrono::LocalResult::None => naive.and_utc().timestamp(),
    }
}

/// Local-time Mon 00:00:00 → Sun 23:59:59 for the current and previous week.
/// Returns (week_start, week_end, prev_week_start, prev_week_end) in epoch secs.
fn week_bounds() -> (i64, i64, i64, i64) {
    let now = Local::now();
    // JS getDay(): 0=Sun..6=Sat → days since Monday = (getDay()+6)%7.
    let days_since_monday = (now.weekday().num_days_from_sunday() + 6) % 7;
    let today = now.date_naive();
    let monday = today - Duration::days(days_since_monday as i64);
    let sunday = monday + Duration::days(6);
    let prev_sunday = monday - Duration::days(1);
    let prev_monday = prev_sunday - Duration::days(6);

    // and_hms_opt with constant valid times never returns None.
    let start_of = |d: chrono::NaiveDate| local_naive_to_ts(d.and_hms_opt(0, 0, 0).unwrap());
    let end_of = |d: chrono::NaiveDate| local_naive_to_ts(d.and_hms_opt(23, 59, 59).unwrap());
    (
        start_of(monday),
        end_of(sunday),
        start_of(prev_monday),
        end_of(prev_sunday),
    )
}

/// GET /api/stats/weekly — current-week activity for the Home hero.
#[tauri::command]
pub fn get_weekly_stats(state: tauri::State<AppState>) -> CmdResult<Value> {
    let (week_start, week_end, prev_start, prev_end) = week_bounds();
    let conn = state.db.lock().map_err(err)?;
    let count = |sql: &str, a: i64, b: i64| -> rusqlite::Result<i64> {
        conn.query_row(sql, rusqlite::params![a, b], |r| r.get(0))
    };

    let total_plays = count(
        "SELECT COUNT(*) FROM play_log WHERE played_at >= ? AND played_at <= ?",
        week_start,
        week_end,
    )
    .map_err(err)?;
    let prev_week_plays = count(
        "SELECT COUNT(*) FROM play_log WHERE played_at >= ? AND played_at <= ?",
        prev_start,
        prev_end,
    )
    .map_err(err)?;
    let tracks_played = count(
        "SELECT COUNT(DISTINCT track_id) FROM play_log WHERE played_at >= ? AND played_at <= ?",
        week_start,
        week_end,
    )
    .map_err(err)?;
    let notes_added = count(
        "SELECT COUNT(*) FROM track_notes WHERE created_at >= ? AND created_at <= ?",
        week_start,
        week_end,
    )
    .map_err(err)?;
    let tags_added = count(
        "SELECT COUNT(*) FROM track_tags WHERE created_at >= ? AND created_at <= ?",
        week_start,
        week_end,
    )
    .map_err(err)?;
    let todos_done = count(
        "SELECT COUNT(*) FROM todos WHERE completed = 1 AND completed_at >= ? AND completed_at <= ?",
        week_start, week_end,
    ).map_err(err)?;

    let top_tracks = top_tracks_between(&conn, Some((week_start, week_end))).map_err(err)?;
    let last_sync = *state.last_sync.lock().map_err(err)?;

    Ok(json!({
        "totalPlays": total_plays,
        "prevWeekPlays": prev_week_plays,
        "tracksPlayed": tracks_played,
        "notesAdded": notes_added,
        "tagsAdded": tags_added,
        "todosDone": todos_done,
        "topTracks": top_tracks,
        "lastSyncTime": last_sync,
    }))
}

/// Top-5 most-played tracks, optionally within a [start,end] window.
fn top_tracks_between(
    conn: &rusqlite::Connection,
    window: Option<(i64, i64)>,
) -> rusqlite::Result<Vec<Value>> {
    let map_row = |r: &Row| {
        Ok(json!({
            "track_id": r.get::<_, i64>("track_id")?,
            "title": r.get::<_, String>("title")?,
            "crate_id": r.get::<_, i64>("crate_id")?,
            "crate_name": r.get::<_, String>("crate_name")?,
            "plays": r.get::<_, i64>("plays")?,
        }))
    };
    let base = "SELECT pl.track_id, t.title, t.crate_id, c.name AS crate_name, COUNT(*) AS plays
                FROM play_log pl
                JOIN tracks t ON t.id = pl.track_id
                JOIN crates c ON c.id = t.crate_id";
    match window {
        Some((a, b)) => {
            let sql = format!(
                "{base} WHERE pl.played_at >= ? AND pl.played_at <= ?
                 GROUP BY pl.track_id ORDER BY plays DESC LIMIT 5"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params![a, b], map_row)?;
            rows.collect()
        }
        None => {
            let sql = format!("{base} GROUP BY pl.track_id ORDER BY plays DESC LIMIT 5");
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([], map_row)?;
            rows.collect()
        }
    }
}

/// GET /api/stats/alltime — lifetime totals for the All Time analytics tab.
#[tauri::command]
pub fn get_alltime_stats(state: tauri::State<AppState>) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    let one = |sql: &str| -> rusqlite::Result<i64> { conn.query_row(sql, [], |r| r.get(0)) };

    let total_plays = one("SELECT COUNT(*) FROM play_log").map_err(err)?;
    let tracks_played = one("SELECT COUNT(DISTINCT track_id) FROM play_log").map_err(err)?;
    let notes_count = one("SELECT COUNT(*) FROM track_notes").map_err(err)?;
    let tags_count = one("SELECT COUNT(*) FROM track_tags").map_err(err)?;
    let todos_done = one("SELECT COUNT(*) FROM todos WHERE completed = 1").map_err(err)?;

    let first_play_ts: Option<i64> = conn
        .query_row("SELECT MIN(played_at) FROM play_log", [], |r| r.get(0))
        .map_err(err)?;
    let first_play_date = first_play_ts.and_then(|ts| {
        const MO: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        Local
            .timestamp_opt(ts, 0)
            .single()
            .map(|d| format!("{} {}, {}", MO[d.month0() as usize], d.day(), d.year()))
    });

    let top_tracks = top_tracks_between(&conn, None).map_err(err)?;

    Ok(json!({
        "totalPlays": total_plays,
        "tracksPlayed": tracks_played,
        "notesCount": notes_count,
        "tagsCount": tags_count,
        "todosDone": todos_done,
        "firstPlayDate": first_play_date,
        "topTracks": top_tracks,
    }))
}

// ── search ─────────────────────────────────────────────────────────────────

/// GET /api/search?q= — crates by name, tracks by title/tag, notes by text
/// (track_notes + inline tracks.notes), deduped by (track_id, note_text).
#[tauri::command]
pub fn search(state: tauri::State<AppState>, q: String) -> CmdResult<Value> {
    let q = q.trim();
    if q.is_empty() {
        return Ok(json!({ "crates": [], "tracks": [], "notes": [] }));
    }
    let like = format!("%{q}%");
    let conn = state.db.lock().map_err(err)?;

    // crates
    let crates: Vec<Value> = {
        let mut stmt = conn
            .prepare("SELECT id, name FROM crates WHERE name LIKE ? ORDER BY name")
            .map_err(err)?;
        let rows = stmt
            .query_map([&like], |r| {
                Ok(json!({ "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)? }))
            })
            .map_err(err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?
    };

    // tracks (title or tag match)
    let track_rows: Vec<(i64, String, i64, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT t.id, t.title, t.crate_id, c.name AS crate_name
                 FROM tracks t
                 JOIN crates c ON c.id = t.crate_id
                 LEFT JOIN track_tags tt ON tt.track_id = t.id
                 WHERE t.title LIKE ? OR tt.tag LIKE ?
                 ORDER BY c.name, t.title",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map([&like, &like], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?
    };

    // tags for the matched tracks
    let mut tag_map: std::collections::HashMap<i64, Vec<Value>> = std::collections::HashMap::new();
    if !track_rows.is_empty() {
        let ids: Vec<i64> = track_rows.iter().map(|t| t.0).collect();
        let ph = sql_placeholders(ids.len());
        let sql = format!("SELECT track_id, tag FROM track_tags WHERE track_id IN ({ph})");
        let mut stmt = conn.prepare(&sql).map_err(err)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(err)?;
        for row in rows {
            let (tid, tag) = row.map_err(err)?;
            tag_map.entry(tid).or_default().push(json!(tag));
        }
    }
    let tracks: Vec<Value> = track_rows
        .into_iter()
        .map(|(id, title, crate_id, crate_name)| {
            json!({
                "track_id": id,
                "title": title,
                "crate_id": crate_id,
                "crate_name": crate_name,
                "tags": tag_map.remove(&id).unwrap_or_default(),
            })
        })
        .collect();

    // notes: track_notes rows + inline tracks.notes, deduped by (track_id, text)
    let mut notes: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT tn.note AS note_text, t.id AS track_id, t.title AS track_title,
                        c.id AS crate_id, c.name AS crate_name
                 FROM track_notes tn
                 JOIN tracks t ON t.id = tn.track_id
                 JOIN crates c ON c.id = t.crate_id
                 WHERE tn.note LIKE ? ORDER BY c.name, t.title",
            )
            .map_err(err)?;
        let rows = stmt.query_map([&like], note_search_row).map_err(err)?;
        for row in rows {
            push_note(&mut notes, &mut seen, row.map_err(err)?);
        }
    }
    {
        let mut stmt = conn
            .prepare(
                "SELECT t.notes AS note_text, t.id AS track_id, t.title AS track_title,
                        c.id AS crate_id, c.name AS crate_name
                 FROM tracks t
                 JOIN crates c ON c.id = t.crate_id
                 WHERE t.notes IS NOT NULL AND t.notes LIKE ? ORDER BY c.name, t.title",
            )
            .map_err(err)?;
        let rows = stmt.query_map([&like], note_search_row).map_err(err)?;
        for row in rows {
            push_note(&mut notes, &mut seen, row.map_err(err)?);
        }
    }

    Ok(json!({ "crates": crates, "tracks": tracks, "notes": notes }))
}

fn note_search_row(r: &Row) -> rusqlite::Result<(String, i64, String, i64, String)> {
    Ok((
        r.get("note_text")?,
        r.get("track_id")?,
        r.get("track_title")?,
        r.get("crate_id")?,
        r.get("crate_name")?,
    ))
}

fn push_note(
    notes: &mut Vec<Value>,
    seen: &mut std::collections::HashSet<String>,
    (note_text, track_id, track_title, crate_id, crate_name): (String, i64, String, i64, String),
) {
    let key = format!("{track_id}::{note_text}");
    if !seen.insert(key) {
        return;
    }
    notes.push(json!({
        "note_text": note_text,
        "track_id": track_id,
        "track_title": track_title,
        "crate_id": crate_id,
        "crate_name": crate_name,
    }));
}

// ── insights (Career Arc) — read-only over als_project_index/als_plugins ─────
// The reindex command (POST /api/insights/reindex) needs the .als parser and
// lands with task #4; these reads work against the existing index now.

/// GET /api/insights/summary
#[tauri::command]
pub fn insights_summary(state: tauri::State<AppState>) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    let totals = conn
        .query_row(
            "SELECT COUNT(*) AS total, ROUND(AVG(bpm),1) AS avg_bpm,
                    ROUND(MIN(bpm),1) AS min_bpm, ROUND(MAX(bpm),1) AS max_bpm
             FROM als_project_index WHERE bpm IS NOT NULL",
            [],
            |r| {
                Ok(json!({
                    "total": r.get::<_, i64>("total")?,
                    "avg_bpm": r.get::<_, Option<f64>>("avg_bpm")?,
                    "min_bpm": r.get::<_, Option<f64>>("min_bpm")?,
                    "max_bpm": r.get::<_, Option<f64>>("max_bpm")?,
                }))
            },
        )
        .map_err(err)?;

    let top_plugin = conn
        .query_row(
            "SELECT plugin_name, COUNT(*) AS cnt FROM als_plugins
             GROUP BY plugin_name ORDER BY cnt DESC LIMIT 1",
            [],
            |r| Ok(json!({ "plugin_name": r.get::<_, String>(0)?, "cnt": r.get::<_, i64>(1)? })),
        )
        .ok()
        .unwrap_or(Value::Null);

    let time_sigs: Vec<Value> = {
        let mut stmt = conn
            .prepare(
                "SELECT time_sig_num || '/' || time_sig_den AS sig, COUNT(*) AS cnt
                 FROM als_project_index WHERE time_sig_num IS NOT NULL
                 GROUP BY sig ORDER BY cnt DESC",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(json!({ "sig": r.get::<_, String>(0)?, "cnt": r.get::<_, i64>(1)? }))
            })
            .map_err(err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?
    };

    let top_key = conn
        .query_row(
            // CAST key_scale → TEXT: the column holds the scale NAME as text
            // ("Major"/"Minor") on newer Live, but SQLite's INTEGER affinity
            // coerces older Live's numeric scale indices back to integers, so the
            // column is genuinely mixed-type. CAST makes the Option<String> read
            // below safe for either stored form. (audit M1)
            "SELECT key_root, CAST(key_scale AS TEXT), COUNT(*) AS cnt FROM als_project_index
             WHERE key_confirmed = 1 GROUP BY key_root, key_scale ORDER BY cnt DESC LIMIT 1",
            [],
            |r| {
                Ok(json!({
                    "key_root": r.get::<_, Option<i64>>(0)?,
                    "key_scale": r.get::<_, Option<String>>(1)?,
                    "cnt": r.get::<_, i64>(2)?,
                }))
            },
        )
        .ok()
        .unwrap_or(Value::Null);

    Ok(
        json!({ "totals": totals, "topPlugin": top_plugin, "timeSigs": time_sigs, "topKey": top_key }),
    )
}

/// GET /api/insights/timeline — beats per month (file_mod_date).
#[tauri::command]
pub fn insights_timeline(state: tauri::State<AppState>) -> CmdResult<Vec<Value>> {
    let conn = state.db.lock().map_err(err)?;
    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-%m', datetime(file_mod_date, 'unixepoch')) AS month, COUNT(*) AS cnt
             FROM als_project_index WHERE file_mod_date IS NOT NULL
             GROUP BY month ORDER BY month",
        )
        .map_err(err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(json!({ "month": r.get::<_, String>("month")?, "cnt": r.get::<_, i64>("cnt")? }))
        })
        .map_err(err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)
}

/// GET /api/insights/plugins — top 15 plugins by project count + total projects.
#[tauri::command]
pub fn insights_plugins(state: tauri::State<AppState>) -> CmdResult<Value> {
    let conn = state.db.lock().map_err(err)?;
    let plugins: Vec<Value> = {
        let mut stmt = conn
            .prepare(
                "SELECT plugin_name, COUNT(*) AS cnt FROM als_plugins
                 GROUP BY plugin_name ORDER BY cnt DESC LIMIT 15",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map([], |r| Ok(json!({ "plugin_name": r.get::<_, String>("plugin_name")?, "cnt": r.get::<_, i64>("cnt")? })))
            .map_err(err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?
    };
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM als_project_index", [], |r| r.get(0))
        .map_err(err)?;
    Ok(json!({ "plugins": plugins, "total": total }))
}

/// GET /api/insights/day-of-week — project count per weekday (0=Sun..6=Sat).
#[tauri::command]
pub fn insights_day_of_week(state: tauri::State<AppState>) -> CmdResult<Vec<Value>> {
    let conn = state.db.lock().map_err(err)?;
    let mut stmt = conn
        .prepare(
            "SELECT strftime('%w', datetime(file_mod_date, 'unixepoch')) AS dow, COUNT(*) AS cnt
             FROM als_project_index WHERE file_mod_date IS NOT NULL
             GROUP BY dow ORDER BY dow",
        )
        .map_err(err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(json!({ "dow": r.get::<_, String>("dow")?, "cnt": r.get::<_, i64>("cnt")? }))
        })
        .map_err(err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)
}

// ── ingestion / scanning (task #4) ─────────────────────────────────────────

/// Read the configured albums folder, or None if unset/empty.
fn albums_folder(conn: &rusqlite::Connection) -> CmdResult<Option<String>> {
    conn.query_row(
        "SELECT value FROM config WHERE key = 'albums_folder'",
        [],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(err)
    .map(|v| v.filter(|s| !s.is_empty()))
}

/// POST /api/config/albums-folder — persist the Music Folder and ingest it.
/// Ingestion runs synchronously here; the renderer refreshes its views once the
/// command resolves.
#[tauri::command]
pub fn set_albums_folder(
    app: tauri::AppHandle,
    state: tauri::State<AppState>,
    path: String,
) -> CmdResult<Value> {
    if path.is_empty() {
        return Err("path is required".into());
    }
    let mut conn = state.db.lock().map_err(err)?;
    conn.execute(
        "INSERT OR REPLACE INTO config (key, value) VALUES ('albums_folder', ?1)",
        [&path],
    )
    .map_err(err)?;
    // Extend the asset-protocol scope to the newly-chosen folder so covers/audio
    // load without a restart (the static scope only seeds the data dir — audit L7).
    if let Err(e) = app.asset_protocol_scope().allow_directory(&path, true) {
        eprintln!("[asset-scope] failed to allow albums folder {path}: {e}");
    }
    ingestion::ingest_albums_folder(&mut conn, &path)?;
    drop(conn);
    // M4: stamp last_sync so the Home/Library "synced" readout reflects this manual
    // scan (the startup ingest + watcher already do this; these commands didn't).
    touch_last_sync(&state);
    // M2: re-point the live watcher at the new folder so edits there sync without a
    // restart. Assigning the new debouncer drops the previous one (stopping the old
    // watch). Best-effort: a failure just means no live sync until next launch.
    match crate::start_albums_watcher(app.clone(), path.clone()) {
        Ok(debouncer) => {
            if let Ok(mut w) = state.watcher.lock() {
                *w = Some(debouncer);
            }
            println!("[watcher] re-pointed to {path}");
        }
        Err(e) => eprintln!("[watcher] failed to re-point to {path}: {e}"),
    }
    Ok(json!({ "ok": true, "albums_folder": path }))
}

/// POST /api/config/rescan-albums — re-ingest the configured folder (the Settings
/// "Re-scan Library" button). Returns fresh crate/track counts.
#[tauri::command]
pub fn rescan_albums(state: tauri::State<AppState>) -> CmdResult<Value> {
    let mut conn = state.db.lock().map_err(err)?;
    let folder = albums_folder(&conn)?.ok_or("Music Folder is not configured.")?;
    ingestion::ingest_albums_folder(&mut conn, &folder)?;
    let crates: i64 = conn
        .query_row("SELECT COUNT(*) FROM crates", [], |r| r.get(0))
        .map_err(err)?;
    let tracks: i64 = conn
        .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
        .map_err(err)?;
    drop(conn);
    touch_last_sync(&state); // M4: manual re-scan updates the synced readout too.
    Ok(json!({ "ok": true, "crates": crates, "tracks": tracks }))
}

/// POST /api/insights/reindex — re-scan the configured Ableton root, refreshing
/// als_project_index (powers the Career Arc / Insights views).
#[tauri::command]
pub fn reindex_als(state: tauri::State<AppState>) -> CmdResult<Value> {
    let mut conn = state.db.lock().map_err(err)?;
    let root: Option<String> = conn
        .query_row(
            "SELECT value FROM config WHERE key = 'ableton_root'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(err)?
        .filter(|s| !s.is_empty());
    let root = root.ok_or("Ableton Projects Folder is not configured.")?;
    let (scanned, failed, pruned) = als::index_als_root(&mut conn, &root)?;
    // M10: surface the failed names (not just a count) so the renderer can list
    // which projects wouldn't parse. `pruned` = orphan rows removed.
    Ok(json!({
        "ok": true,
        "scanned": scanned,
        "failed": failed.len(),
        "failed_files": failed,
        "pruned": pruned,
    }))
}

// ── loudness analysis (task #4) ────────────────────────────────────────────

/// POST /api/tracks/analyze-all — kick off background loudness analysis of every
/// track with a NULL replay_gain. Decoding is slow, so it runs on a worker
/// thread as a background job; the renderer polls analyze_status.
#[tauri::command]
pub fn analyze_all(app: tauri::AppHandle, state: tauri::State<AppState>) -> CmdResult<Value> {
    // Already running? Return the live job (matches the route's `already_running`).
    {
        let job = state.analysis.lock().map_err(err)?;
        if let Some(j) = job.as_ref() {
            if j.running {
                return Ok(json!({
                    "ok": true,
                    "already_running": true,
                    "running": j.running,
                    "total": j.total,
                    "completed": j.completed,
                    "failed": j.failed,
                    "done": j.done,
                }));
            }
        }
    }

    let count: i64 = {
        let conn = state.db.lock().map_err(err)?;
        conn.query_row(
            "SELECT COUNT(*) FROM tracks WHERE replay_gain IS NULL",
            [],
            |r| r.get(0),
        )
        .map_err(err)?
    };

    {
        let mut job = state.analysis.lock().map_err(err)?;
        *job = Some(AnalysisJob {
            running: count > 0,
            total: count,
            completed: 0,
            failed: 0,
            done: count == 0,
        });
    }

    if count > 0 {
        let app = app.clone();
        std::thread::spawn(move || run_analysis(app));
    }

    Ok(json!({ "ok": true, "total": count, "done": count == 0 }))
}

/// Worker: decode + measure each NULL-replay_gain track, updating the DB and the
/// shared job counters as it goes. Never holds the DB lock during a decode.
fn run_analysis(app: tauri::AppHandle) {
    let state = app.state::<AppState>();

    // Mark the shared job finished so the renderer's poller stops showing
    // "running" forever when the worker bails early.
    let finish_job = |state: &AppState| {
        if let Ok(mut job) = state.analysis.lock() {
            if let Some(j) = job.as_mut() {
                j.running = false;
                j.done = true;
            }
        }
    };

    let tracks: Vec<(i64, String, String)> = {
        let conn = match state.db.lock() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[analyze] DB lock poisoned: {e}");
                finish_job(&state);
                return;
            }
        };
        let mut stmt = match conn.prepare(
            "SELECT t.id, t.filename, c.folder
             FROM tracks t JOIN crates c ON c.id = t.crate_id
             WHERE t.replay_gain IS NULL",
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[analyze] prepare failed: {e}");
                finish_job(&state);
                return;
            }
        };
        let rows = match stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        }) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[analyze] query failed: {e}");
                finish_job(&state);
                return;
            }
        };
        rows.filter_map(Result::ok).collect()
    };

    let total = tracks.len() as i64;
    for (id, filename, folder) in tracks {
        let path = std::path::Path::new(&folder).join(&filename);
        match loudness::analyze_loudness(&path) {
            Ok(gain) => {
                // H2: only count this track as completed if the DB write actually
                // landed. Discarding the UPDATE result let the completion counter
                // tick up on a failed write, reporting progress that didn't happen.
                let stored = match state.db.lock() {
                    Ok(conn) => match conn.execute(
                        "UPDATE tracks SET replay_gain = ? WHERE id = ?",
                        rusqlite::params![gain, id],
                    ) {
                        Ok(n) => n > 0,
                        Err(e) => {
                            eprintln!("[analyze] UPDATE failed for track {id} ({filename}): {e}");
                            false
                        }
                    },
                    Err(e) => {
                        eprintln!("[analyze] DB lock poisoned writing track {id}: {e}");
                        false
                    }
                };
                if let Ok(mut job) = state.analysis.lock() {
                    if let Some(j) = job.as_mut() {
                        if stored {
                            j.completed += 1;
                        } else {
                            j.failed += 1;
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Loudness analysis failed for track {id} ({filename}): {e}");
                if let Ok(mut job) = state.analysis.lock() {
                    if let Some(j) = job.as_mut() {
                        j.failed += 1;
                    }
                }
            }
        }
    }

    // M12: a bulk run that mostly fails used to complete "successfully" with no
    // signal. Warn the user when a meaningful share of tracks couldn't be
    // analyzed (≥25% and at least 3) — likely a missing/moved folder or codec
    // issue, not a one-off bad file.
    let failed = state
        .analysis
        .lock()
        .ok()
        .and_then(|j| j.as_ref().map(|j| j.failed))
        .unwrap_or(0);
    if failed >= 3 && total > 0 && failed * 4 >= total {
        crate::notify(
            &app,
            &format!(
                "Loudness analysis finished, but {failed} of {total} beats couldn't be analyzed — some files may be missing or unreadable.",
            ),
            "err",
        );
    }

    finish_job(&state);
}

/// GET /api/tracks/analyze-status — poll the running/last job.
#[tauri::command]
pub fn analyze_status(state: tauri::State<AppState>) -> CmdResult<Value> {
    let job = state.analysis.lock().map_err(err)?;
    Ok(match job.as_ref() {
        Some(j) => json!({
            "running": j.running,
            "total": j.total,
            "completed": j.completed,
            "failed": j.failed,
            "done": j.done,
        }),
        None => json!({
            "running": false,
            "total": 0,
            "completed": 0,
            "failed": 0,
            "done": false,
        }),
    })
}
