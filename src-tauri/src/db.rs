// SQLite schema + idempotent migrations (rusqlite).
//
// DB PATH: we deliberately do NOT use Tauri's bundle-identifier-derived dir.
// The VST3 plugin hardcodes ~/Library/Application Support/BeatCrate/beatcrate.db,
// so we keep pointing there. BEATCRATE_DATA_DIR overrides it (throwaway-DB
// testing).

use rusqlite::Connection;
use std::path::PathBuf;

/// Resolve the data directory: $BEATCRATE_DATA_DIR if set, else
/// ~/Library/Application Support/BeatCrate (NOT the bundle-id dir — see above).
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("BEATCRATE_DATA_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let home = std::env::var("HOME").expect("HOME not set");
    PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("BeatCrate")
}

pub fn db_path() -> PathBuf {
    data_dir().join("beatcrate.db")
}

/// Open the DB at the resolved path, applying schema + migrations. Idempotent.
/// Returns a human-readable error (instead of panicking) when the data dir can't
/// be created or the DB can't be opened, so the caller can surface it.
pub fn open_and_init() -> Result<Connection, String> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create data dir {}: {e}", dir.display()))?;
    let conn = Connection::open(db_path()).map_err(|e| e.to_string())?;
    init_schema(&conn).map_err(|e| e.to_string())?;
    Ok(conn)
}

/// Run an idempotent migration statement. The "duplicate column" / "already
/// exists" errors are the expected no-op on re-run; anything else is logged so
/// a genuinely broken migration is visible instead of leaving the schema
/// silently half-initialized.
fn try_exec(conn: &Connection, sql: &str) {
    if let Err(e) = conn.execute_batch(sql) {
        let msg = e.to_string();
        if !(msg.contains("duplicate column") || msg.contains("already exists")) {
            eprintln!("[db] unexpected migration error ({msg}) for: {sql}");
        }
    }
}

pub(crate) fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // Wait-and-retry for up to 5s when the DB is locked (e.g. the external VST3
    // plugin holds a write lock) instead of failing immediately with SQLITE_BUSY.
    conn.busy_timeout(std::time::Duration::from_secs(5))?;

    // ── Core tables ──────────────────────────────────────────────────────────
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS config (
          key   TEXT PRIMARY KEY,
          value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS crates (
          id         INTEGER PRIMARY KEY AUTOINCREMENT,
          name       TEXT NOT NULL,
          folder     TEXT NOT NULL UNIQUE,
          cover_path TEXT,
          created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS tracks (
          id         INTEGER PRIMARY KEY AUTOINCREMENT,
          crate_id   INTEGER NOT NULL REFERENCES crates(id) ON DELETE CASCADE,
          filename   TEXT NOT NULL,
          title      TEXT NOT NULL,
          track_num  INTEGER,
          duration   REAL,
          favorited  INTEGER NOT NULL DEFAULT 0,
          created_at INTEGER NOT NULL DEFAULT (unixepoch()),
          UNIQUE(crate_id, filename)
        );

        CREATE TABLE IF NOT EXISTS track_tags (
          id       INTEGER PRIMARY KEY AUTOINCREMENT,
          track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
          tag      TEXT NOT NULL,
          UNIQUE(track_id, tag)
        );

        CREATE TABLE IF NOT EXISTS track_notes (
          id         INTEGER PRIMARY KEY AUTOINCREMENT,
          track_id   INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
          note       TEXT NOT NULL,
          created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );
        "#,
    )?;

    // ── Idempotent column migrations ──────────────────────────────────────────
    try_exec(conn, "ALTER TABLE tracks ADD COLUMN notes TEXT");
    try_exec(conn, "ALTER TABLE tracks ADD COLUMN favorited_at INTEGER");
    try_exec(
        conn,
        "ALTER TABLE track_notes ADD COLUMN sort_order INTEGER",
    );
    try_exec(conn, "ALTER TABLE tracks ADD COLUMN replay_gain REAL");
    try_exec(
        conn,
        "ALTER TABLE track_notes ADD COLUMN completed INTEGER NOT NULL DEFAULT 0",
    );

    // Scratchpad — single-row freeform note.
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS scratchpad (
          id      INTEGER PRIMARY KEY CHECK (id = 1),
          content TEXT    NOT NULL DEFAULT ''
        );
        "#,
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO scratchpad (id, content) VALUES (1, '')",
        [],
    )?;

    try_exec(conn, "ALTER TABLE crates ADD COLUMN producer TEXT");
    try_exec(conn, "ALTER TABLE crates ADD COLUMN scratch_pad TEXT");
    try_exec(
        conn,
        "ALTER TABLE crates ADD COLUMN status TEXT NOT NULL DEFAULT 'unreleased'",
    );

    // Todos.
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS todos (
          id         INTEGER PRIMARY KEY AUTOINCREMENT,
          text       TEXT NOT NULL,
          completed  INTEGER NOT NULL DEFAULT 0,
          created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );
        "#,
    )?;
    try_exec(conn, "ALTER TABLE todos ADD COLUMN sort_order INTEGER");

    // tracks.sort_order + per-crate backfill. The backfill only runs when the
    // ALTER actually adds the column (fresh DB) — on the existing DB the column
    // is present, the ALTER no-ops, and the data already carries sort_order.
    if conn
        .execute("ALTER TABLE tracks ADD COLUMN sort_order INTEGER", [])
        .is_ok()
    {
        backfill_track_sort_order(conn)?;
    }

    // User profile.
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS user_profile (
          id          INTEGER PRIMARY KEY CHECK (id = 1),
          name        TEXT    NOT NULL DEFAULT '',
          avatar_path TEXT
        );
        "#,
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO user_profile (id, name, avatar_path) VALUES (1, '', NULL)",
        [],
    )?;

    // Ableton Live Set index + related tables.
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS als_project_index (
          id                 INTEGER PRIMARY KEY AUTOINCREMENT,
          als_path           TEXT NOT NULL UNIQUE,
          bpm                REAL,
          key_root           INTEGER,
          -- Scale NAME as text ("Major"/"Minor" on Live 11/12; an integer index
          -- as a string on older Live). INTEGER affinity is intentionally kept
          -- (SQLite stores TEXT here fine — no retype needed); als.rs writes the
          -- raw string. Was parsed as i64 originally, which dropped text names.
          key_scale          INTEGER,
          time_sig_num       INTEGER,
          time_sig_den       INTEGER,
          active_track_count INTEGER,
          plugin_count       INTEGER,
          ableton_version    TEXT,
          file_mod_date      INTEGER,
          parsed_at          INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS als_plugins (
          id          INTEGER PRIMARY KEY AUTOINCREMENT,
          project_id  INTEGER NOT NULL REFERENCES als_project_index(id) ON DELETE CASCADE,
          plugin_name TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS als_samples (
          id          INTEGER PRIMARY KEY AUTOINCREMENT,
          project_id  INTEGER NOT NULL REFERENCES als_project_index(id) ON DELETE CASCADE,
          sample_name TEXT NOT NULL
        );
        "#,
    )?;
    try_exec(
        conn,
        "ALTER TABLE als_project_index ADD COLUMN key_confirmed INTEGER NOT NULL DEFAULT 0",
    );
    try_exec(
        conn,
        "ALTER TABLE als_project_index ADD COLUMN created_date INTEGER",
    );

    // Play log.
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS play_log (
          id        INTEGER PRIMARY KEY AUTOINCREMENT,
          track_id  INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
          played_at INTEGER NOT NULL DEFAULT (unixepoch())
        );
        "#,
    )?;

    try_exec(
        conn,
        "ALTER TABLE track_tags ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0",
    );
    try_exec(conn, "ALTER TABLE todos ADD COLUMN completed_at INTEGER");

    // Tidy any NULL sort_order in the append-ordered lists (notes/todos) so the
    // MAX(sort_order)+1 append contract stays collision-free. Idempotent — a
    // no-op once every row carries a value. (audit M4, 2026-05-31)
    backfill_null_list_sort_order(conn)?;

    // Content fingerprint + prune-grace stamps. A track re-exported over its own
    // path keeps its row (and its notes/tags/plays/sort_order); the fingerprint is
    // what tells the ingest the bytes changed so it can re-measure. NULL fingerprint
    // means "never recorded" — the ingest adopts it silently rather than treating
    // every pre-migration row as changed. (No backfill here: reading mtime/size for
    // the whole library at migration time would duplicate the ingest's own scan.)
    try_exec(conn, "ALTER TABLE tracks ADD COLUMN file_mtime INTEGER");
    try_exec(conn, "ALTER TABLE tracks ADD COLUMN file_size INTEGER");
    try_exec(conn, "ALTER TABLE tracks ADD COLUMN missing_since INTEGER");
    try_exec(conn, "ALTER TABLE crates ADD COLUMN emptied_since INTEGER");

    // Drop the deprecated crate_tags table (crate-level tagging was removed).
    try_exec(conn, "DROP TABLE IF EXISTS crate_tags");

    Ok(())
}

/// Append a sort_order to any NULL rows in the append-ordered lists, continuing
/// past the current max within each list so values stay unique and the
/// MAX(sort_order)+1 insert contract holds. NULLs get ordered among themselves
/// by created_at then id (matches each list's read ORDER BY), so the visible
/// order doesn't jump. Idempotent: once filled there are no NULLs to touch.
///
/// `tracks` is deliberately NOT included — its NULL sort_order is intentional
/// (ingestion inserts NULL; the reorder UI sets it later) and the read path is
/// already NULL-safe via `ORDER BY sort_order IS NULL, sort_order, ...`. Forcing
/// values there would fight that design. (audit M4)
fn backfill_null_list_sort_order(conn: &Connection) -> rusqlite::Result<()> {
    // todos — single flat list.
    {
        let max: i64 =
            conn.query_row("SELECT COALESCE(MAX(sort_order), -1) FROM todos", [], |r| {
                r.get(0)
            })?;
        let ids: Vec<i64> = {
            let mut stmt = conn
                .prepare("SELECT id FROM todos WHERE sort_order IS NULL ORDER BY created_at, id")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (i, id) in ids.iter().enumerate() {
            conn.execute(
                "UPDATE todos SET sort_order = ? WHERE id = ?",
                rusqlite::params![max + 1 + i as i64, id],
            )?;
        }
    }

    // track_notes — one list per track_id.
    let track_ids: Vec<i64> = {
        let mut stmt =
            conn.prepare("SELECT DISTINCT track_id FROM track_notes WHERE sort_order IS NULL")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for tid in track_ids {
        let max: i64 = conn.query_row(
            "SELECT COALESCE(MAX(sort_order), -1) FROM track_notes WHERE track_id = ?",
            [tid],
            |r| r.get(0),
        )?;
        let ids: Vec<i64> = {
            let mut stmt = conn.prepare(
                "SELECT id FROM track_notes WHERE track_id = ? AND sort_order IS NULL
                 ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map([tid], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (i, id) in ids.iter().enumerate() {
            conn.execute(
                "UPDATE track_notes SET sort_order = ? WHERE id = ?",
                rusqlite::params![max + 1 + i as i64, id],
            )?;
        }
    }
    Ok(())
}

/// Per-crate sequential sort_order from current (track_num, filename) order.
fn backfill_track_sort_order(conn: &Connection) -> rusqlite::Result<()> {
    let crate_ids: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM crates")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for crate_id in crate_ids {
        let track_ids: Vec<i64> = {
            let mut stmt = conn
                .prepare("SELECT id FROM tracks WHERE crate_id = ? ORDER BY track_num, filename")?;
            let rows = stmt.query_map([crate_id], |r| r.get::<_, i64>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (i, track_id) in track_ids.iter().enumerate() {
            conn.execute(
                "UPDATE tracks SET sort_order = ? WHERE id = ?",
                rusqlite::params![i as i64, track_id],
            )?;
        }
    }
    Ok(())
}
