// Album-folder ingestion.
//
// Scans the configured Music Folder one level deep: each immediate subfolder is
// a "crate", each audio file inside it a "track". Upserts crates/tracks and
// prunes tracks whose files vanished (FK ON DELETE CASCADE cleans up tags/notes/
// play_log). Track duration via lofty (metadata read — no full decode).

use rusqlite::{params, Connection, OptionalExtension, ToSql};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

const AUDIO_EXTENSIONS: &[&str] = &[".wav", ".aiff", ".aif", ".mp3", ".flac", ".m4a", ".ogg"];
const COVER_NAMES: &[&str] = &["cover.jpg", "cover.jpeg", "cover.png"];

/// How long a track (or a crate's last audio file) must stay missing before the
/// row is deleted. A DAW re-exporting over an existing file unlinks it and writes
/// a new one; the 1.5s-debounced watcher can scan inside that gap, and an
/// immediate delete there destroys the row plus everything cascading off it
/// (notes/tags/favorite/play_log). The grace turns that transient gap into a
/// no-op — a genuinely deleted file still goes on the next scan past the window.
const PRUNE_GRACE_SECS: i64 = 60;

/// Content fingerprint of an audio file: mtime (epoch millis) + byte size.
/// Both None if the metadata couldn't be read — treated the same as a stored
/// NULL, i.e. "unknown", never as "changed".
#[derive(Clone, Copy, PartialEq)]
pub struct FileMeta {
    pub mtime: Option<i64>,
    pub size: Option<i64>,
}

pub struct CrateScan {
    pub name: String,
    pub folder: String,
    pub cover_path: Option<String>,
    pub audio_files: Vec<String>,
    /// Parallel to `audio_files` — same index, same file.
    pub file_meta: Vec<FileMeta>,
}

fn read_file_meta(md: &std::fs::Metadata) -> FileMeta {
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64);
    FileMeta {
        mtime,
        size: Some(md.len() as i64),
    }
}

/// Read a track's duration in (fractional) seconds via lofty's metadata parse.
/// Mirrors `readDuration` — returns None on any failure (the upsert COALESCEs,
/// so a None never clobbers a previously-measured duration).
fn read_duration(path: &Path) -> Option<f64> {
    use lofty::file::AudioFile;
    let tagged = lofty::read_from_path(path).ok()?;
    let secs = tagged.properties().duration().as_secs_f64();
    if secs > 0.0 {
        Some(secs)
    } else {
        None
    }
}

fn has_audio_ext(lname: &str) -> bool {
    match lname.rfind('.') {
        Some(dot) => AUDIO_EXTENSIONS.contains(&&lname[dot..]),
        None => false,
    }
}

/// Scan one crate folder for its cover + sorted audio file list.
/// Returns None if the directory can't be read.
pub fn scan_crate(crate_folder: &Path) -> Option<CrateScan> {
    let name = crate_folder.file_name()?.to_string_lossy().into_owned();
    let mut cover_path = None;
    let mut audio_files = Vec::new();

    let entries = std::fs::read_dir(crate_folder).ok()?;
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            // A DirEntry that won't read (e.g. an over-long or non-UTF8 name)
            // would otherwise vanish silently and undercount the crate's tracks.
            Err(e) => {
                eprintln!("[ingest] skipped unreadable entry in {crate_folder:?}: {e}");
                continue;
            }
        };
        match entry.file_type() {
            Ok(ft) if ft.is_file() => {}
            _ => continue,
        }
        let fname = entry.file_name().to_string_lossy().into_owned();
        let lname = fname.to_lowercase();
        if COVER_NAMES.contains(&lname.as_str()) {
            cover_path = Some(crate_folder.join(&fname).to_string_lossy().into_owned());
        } else if has_audio_ext(&lname) {
            let meta = match entry.metadata() {
                Ok(md) => read_file_meta(&md),
                Err(e) => {
                    eprintln!("[ingest] no metadata for {fname}: {e}");
                    FileMeta {
                        mtime: None,
                        size: None,
                    }
                }
            };
            audio_files.push((fname, meta));
        }
    }

    audio_files.sort_by(|a, b| a.0.cmp(&b.0));
    let (audio_files, file_meta) = audio_files.into_iter().unzip();
    Some(CrateScan {
        name,
        folder: crate_folder.to_string_lossy().into_owned(),
        cover_path,
        audio_files,
        file_meta,
    })
}

/// Derive a display title from a filename. Mirrors `titleFromFilename`:
///  - Beat-A-Day pattern "1.14 North Country" → "North Country"
///  - else strip leading track numbers like "01 ", "01. ", "01 - "
fn title_from_filename(filename: &str) -> String {
    let base = Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);

    if let Some(rest) = strip_beat_a_day(base) {
        let t = rest.trim();
        return if t.is_empty() {
            base.to_string()
        } else {
            t.to_string()
        };
    }

    let rest = strip_leading_tracknum(base);
    let t = rest.trim();
    if t.is_empty() {
        base.to_string()
    } else {
        t.to_string()
    }
}

// /^\d{1,2}\.\d{1,2}\s+/ — returns the remainder after the match, or None.
fn strip_beat_a_day(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    let mut i = 0;
    let d1 = count_while(b, i, 2, |c| c.is_ascii_digit());
    if d1 == 0 {
        return None;
    }
    i += d1;
    if b.get(i) != Some(&b'.') {
        return None;
    }
    i += 1;
    let d2 = count_while(b, i, 2, |c| c.is_ascii_digit());
    if d2 == 0 {
        return None;
    }
    i += d2;
    let ws = count_while(b, i, usize::MAX, |c| c.is_ascii_whitespace());
    if ws == 0 {
        return None;
    }
    i += ws;
    Some(&s[i..])
}

// /^\d+[\s.\-_]+/ — returns the remainder after the match, or the input unchanged
// when it doesn't match (the strip is a no-op in that case).
fn strip_leading_tracknum(s: &str) -> &str {
    let b = s.as_bytes();
    let digits = count_while(b, 0, usize::MAX, |c| c.is_ascii_digit());
    if digits == 0 {
        return s;
    }
    let seps = count_while(b, digits, usize::MAX, |c| {
        c.is_ascii_whitespace() || matches!(c, b'.' | b'-' | b'_')
    });
    if seps == 0 {
        return s;
    }
    &s[digits + seps..]
}

fn count_while(b: &[u8], start: usize, max: usize, pred: impl Fn(u8) -> bool) -> usize {
    let mut n = 0;
    while n < max && start + n < b.len() && pred(b[start + n]) {
        n += 1;
    }
    n
}

/// Ingest every crate-folder under `albums_folder`. Mirrors `ingestAlbumsFolder`:
/// upsert each crate + its tracks in a transaction, prune missing tracks.
/// Returns the ids of tracks whose audio changed on disk and therefore had their
/// `replay_gain` invalidated. Callers with an AppHandle re-run the loudness
/// worker and tell the renderer to drop those decoded buffers (lib.rs).
/// M5: a music folder that can't be read (moved, deleted, permissions) is an
/// *error*, not a silent no-op — otherwise a misconfigured path yields an
/// empty-looking library with no explanation. Callers surface it (the watcher
/// toasts it; the explicit rescan/set-folder commands return it to the renderer).
/// No DB mutation has happened yet at this point, so the existing library is left
/// untouched on failure.
pub fn ingest_albums_folder(
    conn: &mut Connection,
    albums_folder: &str,
) -> Result<Vec<i64>, String> {
    let entries = match std::fs::read_dir(albums_folder) {
        Ok(e) => e,
        Err(err) => {
            eprintln!("Cannot read albums folder: {err}");
            return Err(format!(
                "Couldn't read the music folder ({albums_folder}): {err}"
            ));
        }
    };

    // Scan all crate folders up front so we can reconcile renames before any
    // upsert (a rename detector needs the full picture of on-disk vs DB folders).
    let mut scans = Vec::new();
    // Folders that exist on disk but currently hold no audio. If one matches an
    // existing crate, that crate's last beat was removed in place — we delete the
    // crate (H2). Kept separate from `scans` so empty folders never become empty
    // crate rows and are excluded from rename reconciliation.
    let mut empty_folders: Vec<String> = Vec::new();
    for entry in entries {
        // L4: don't `flatten()` away unreadable top-level entries — log them like
        // scan_crate() already does for per-crate entries, so a permission/IO
        // glitch on one folder is visible rather than silently dropped.
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                eprintln!("[ingest] skipping unreadable entry in {albums_folder}: {err}");
                continue;
            }
        };
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => {}
            _ => continue,
        }
        if let Some(scan) = scan_crate(&entry.path()) {
            if scan.audio_files.is_empty() {
                empty_folders.push(scan.folder);
            } else {
                scans.push(scan);
            }
        }
    }

    // Preserve crate identity (and all attached track notes/tags/favorites/plays
    // + the crate's producer/status/scratchpad) across folder renames. We match a
    // renamed folder to its orphaned crate by *content* (identical filename set),
    // which is robust to macOS's imprecise filesystem events and also works for
    // renames made while the app was closed (startup ingest).
    reconcile_renames(conn, &scans)?;

    let mut invalidated: Vec<i64> = Vec::new();
    for scan in &scans {
        let durations: Vec<Option<f64>> = scan
            .audio_files
            .iter()
            .map(|f| read_duration(&PathBuf::from(&scan.folder).join(f)))
            .collect();
        invalidated.extend(ingest_one_crate(conn, scan, &durations)?);
    }

    // H2: a crate whose folder is still on disk but no longer holds any audio is
    // deleted outright (cascading its tracks/notes/tags) — "no beats, no crate".
    // Brand-new empty folders have no crate row and are no-ops here. (A whole
    // folder *removed* from disk still leaves an orphan crate; only
    // emptied-in-place folders prune.)
    if !empty_folders.is_empty() {
        prune_emptied_crates(conn, &empty_folders)?;
    }

    Ok(invalidated)
}

/// Delete crates whose folder still exists on disk but no longer contains any
/// audio (H2). Only folders that actually have a crate row are removed; unknown
/// empty folders are ignored. The crate delete cascades to its tracks (and their
/// notes/tags) via `ON DELETE CASCADE`.
///
/// Same PRUNE_GRACE_SECS stamp-then-delete as the per-track prune, and for a
/// worse version of the same race: a one-track crate being re-exported reads as
/// "emptied" for the length of the write, and deleting it there would take the
/// crate's producer/status/scratchpad with it. The stamp is cleared by the crate
/// upsert as soon as the folder has audio again.
fn prune_emptied_crates(conn: &mut Connection, empty_folders: &[String]) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for folder in empty_folders {
        tx.execute(
            "UPDATE crates SET emptied_since = unixepoch()
             WHERE folder = ? AND emptied_since IS NULL",
            [folder],
        )
        .map_err(|e| e.to_string())?;
        let removed = tx
            .execute(
                &format!(
                    "DELETE FROM crates WHERE folder = ?
                       AND emptied_since IS NOT NULL
                       AND emptied_since <= unixepoch() - {PRUNE_GRACE_SECS}"
                ),
                [folder],
            )
            .map_err(|e| e.to_string())?;
        if removed > 0 {
            println!("[ingest] crate emptied (no audio left), removed: {folder}");
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

/// Detect crate-folder renames and update the existing crate row in place
/// (keeping its id, its tracks, and all metadata) instead of letting the upsert
/// loop insert a fresh crate and orphan the old one.
///
/// A rename is an orphan crate (DB folder no longer on disk) whose track-filename
/// set exactly equals a new on-disk folder's audio file set. Only unambiguous
/// 1:1 matches are acted on; anything else falls through to a fresh insert (same
/// outcome as the simple full-rescan — no data loss beyond the unrenamed case).
fn reconcile_renames(conn: &mut Connection, scans: &[CrateScan]) -> Result<(), String> {
    let db_crates: Vec<(i64, String)> = {
        let mut stmt = conn
            .prepare("SELECT id, folder FROM crates")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?
    };

    let disk_folders: HashSet<&str> = scans.iter().map(|s| s.folder.as_str()).collect();
    let db_folders: HashSet<&str> = db_crates.iter().map(|(_, f)| f.as_str()).collect();

    let orphans: Vec<&(i64, String)> = db_crates
        .iter()
        .filter(|(_, f)| !disk_folders.contains(f.as_str()))
        .collect();
    let new_scans: Vec<&CrateScan> = scans
        .iter()
        .filter(|s| !db_folders.contains(s.folder.as_str()))
        .collect();
    if orphans.is_empty() || new_scans.is_empty() {
        return Ok(());
    }

    let mut claimed: HashSet<&str> = HashSet::new();
    for (orphan_id, orphan_folder) in orphans {
        let orphan_files: BTreeSet<String> = {
            let mut stmt = conn
                .prepare("SELECT filename FROM tracks WHERE crate_id = ?")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([orphan_id], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            rows.collect::<rusqlite::Result<BTreeSet<_>>>()
                .map_err(|e| e.to_string())?
        };
        if orphan_files.is_empty() {
            continue;
        }

        let matches: Vec<&&CrateScan> = new_scans
            .iter()
            .filter(|s| {
                !claimed.contains(s.folder.as_str())
                    && s.audio_files.iter().cloned().collect::<BTreeSet<_>>() == orphan_files
            })
            .collect();
        if matches.len() != 1 {
            continue;
        }

        let s = matches[0];
        claimed.insert(s.folder.as_str());
        conn.execute(
            "UPDATE crates SET folder = ?1, name = ?2, cover_path = ?3 WHERE id = ?4",
            params![s.folder, s.name, s.cover_path, orphan_id],
        )
        .map_err(|e| e.to_string())?;
        println!("[ingest] crate renamed: {orphan_folder} → {}", s.folder);
    }

    Ok(())
}

fn ingest_one_crate(
    conn: &mut Connection,
    scan: &CrateScan,
    durations: &[Option<f64>],
) -> Result<Vec<i64>, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;

    // emptied_since clears here: the folder has audio again (or never lost it),
    // so any pending crate-level prune is cancelled.
    tx.execute(
        "INSERT INTO crates (name, folder, cover_path) VALUES (?1, ?2, ?3)
         ON CONFLICT(folder) DO UPDATE SET name=excluded.name, cover_path=excluded.cover_path,
           emptied_since=NULL",
        params![scan.name, scan.folder, scan.cover_path],
    )
    .map_err(|e| e.to_string())?;

    let crate_id: i64 = tx
        .query_row(
            "SELECT id FROM crates WHERE folder = ?",
            [&scan.folder],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;

    let mut invalidated: Vec<i64> = Vec::new();
    {
        // New tracks insert with NULL sort_order (we deliberately don't set it;
        // the per-crate ordering UI assigns it later).
        //
        // An existing row is never replaced — a re-export over the same path keeps
        // its id, and with it sort_order/track_num/title/favorited plus every
        // FK-attached note, tag and play. What the fingerprint decides is whether
        // the *measurements* still describe the bytes on disk:
        //   - stored fingerprint NULL (pre-migration row, or unreadable metadata):
        //     adopt what's on disk, invalidate nothing.
        //   - fingerprint unchanged: today's behaviour — refresh duration only.
        //   - fingerprint changed: re-read duration and NULL replay_gain so the
        //     loudness worker re-measures. Without this, preserving the row would
        //     silently keep the *old* loudness forever (analysis only ever visits
        //     rows WHERE replay_gain IS NULL).
        let mut ins = tx
            .prepare(
                "INSERT INTO tracks (crate_id, filename, title, track_num, duration,
                                     file_mtime, file_size)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .map_err(|e| e.to_string())?;
        let mut upd = tx
            .prepare(
                "UPDATE tracks SET duration = COALESCE(?2, duration),
                                   file_mtime = ?3, file_size = ?4, missing_since = NULL
                 WHERE id = ?1",
            )
            .map_err(|e| e.to_string())?;
        let mut upd_changed = tx
            .prepare(
                "UPDATE tracks SET duration = COALESCE(?2, duration),
                                   file_mtime = ?3, file_size = ?4, missing_since = NULL,
                                   replay_gain = NULL
                 WHERE id = ?1",
            )
            .map_err(|e| e.to_string())?;

        for (i, filename) in scan.audio_files.iter().enumerate() {
            let meta = scan.file_meta[i];
            let existing: Option<(i64, Option<i64>, Option<i64>)> = tx
                .query_row(
                    "SELECT id, file_mtime, file_size FROM tracks
                     WHERE crate_id = ? AND filename = ?",
                    params![crate_id, filename],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(|e| e.to_string())?;

            match existing {
                None => {
                    ins.execute(params![
                        crate_id,
                        filename,
                        title_from_filename(filename),
                        (i as i64) + 1,
                        durations[i],
                        meta.mtime,
                        meta.size,
                    ])
                    .map_err(|e| e.to_string())?;
                }
                Some((id, stored_mtime, stored_size)) => {
                    let known = stored_mtime.is_some() && stored_size.is_some();
                    let changed =
                        known && (stored_mtime != meta.mtime || stored_size != meta.size);
                    let stmt = if changed { &mut upd_changed } else { &mut upd };
                    stmt.execute(params![id, durations[i], meta.mtime, meta.size])
                        .map_err(|e| e.to_string())?;
                    if changed {
                        println!("[ingest] content changed, re-measuring: {filename}");
                        invalidated.push(id);
                    }
                }
            }
        }
    }

    // Tracks whose files aren't in this scan don't die on sight — they're stamped,
    // and only deleted once they've been missing for PRUNE_GRACE_SECS (see the
    // const). Present files cleared their stamp in the upsert above, so a file that
    // reappears resets the clock instead of creeping toward the cutoff.
    // Callers only reach this with a non-empty audio set — a folder emptied of all
    // audio is handled by prune_emptied_crates() (H2), which prunes the whole crate.
    let placeholders = vec!["?"; scan.audio_files.len()].join(",");
    let mut sql_params: Vec<&dyn ToSql> = Vec::with_capacity(scan.audio_files.len() + 1);
    sql_params.push(&crate_id);
    for f in &scan.audio_files {
        sql_params.push(f);
    }

    tx.execute(
        &format!(
            "UPDATE tracks SET missing_since = unixepoch()
             WHERE crate_id = ? AND filename NOT IN ({placeholders})
               AND missing_since IS NULL"
        ),
        sql_params.as_slice(),
    )
    .map_err(|e| e.to_string())?;

    let removed = tx
        .execute(
            &format!(
                "DELETE FROM tracks
                 WHERE crate_id = ? AND filename NOT IN ({placeholders})
                   AND missing_since IS NOT NULL
                   AND missing_since <= unixepoch() - {PRUNE_GRACE_SECS}"
            ),
            sql_params.as_slice(),
        )
        .map_err(|e| e.to_string())?;
    if removed > 0 {
        println!("[ingest] pruned {removed} track(s) missing from {}", scan.folder);
    }

    tx.commit().map_err(|e| e.to_string())?;
    Ok(invalidated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"").unwrap();
    }

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn title_stripping_matches_js() {
        assert_eq!(
            title_from_filename("1.14 North Country.wav"),
            "North Country"
        );
        assert_eq!(
            title_from_filename("01 - Chelsea Piers.wav"),
            "Chelsea Piers"
        );
        assert_eq!(title_from_filename("01. Cold toes.aiff"), "Cold toes");
        assert_eq!(title_from_filename("April showers.wav"), "April showers");
        // Pure-number stems fall back to the base.
        assert_eq!(title_from_filename("12.wav"), "12");
    }

    /// A crate-folder rename must keep the crate id, its tracks, and all attached
    /// metadata — the whole point of content-based reconcile_renames.
    #[test]
    fn rename_preserves_crate_identity_and_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let albums = tmp.path();
        let mut conn = mem_db();

        // Crate "Album A" with two tracks.
        let crate_a = albums.join("Album A");
        std::fs::create_dir(&crate_a).unwrap();
        touch(&crate_a, "one.wav");
        touch(&crate_a, "two.wav");

        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();

        let crate_id: i64 = conn
            .query_row("SELECT id FROM crates", [], |r| r.get(0))
            .unwrap();
        let track_id: i64 = conn
            .query_row("SELECT id FROM tracks WHERE filename='one.wav'", [], |r| {
                r.get(0)
            })
            .unwrap();

        // Attach metadata that a rename must not lose.
        conn.execute(
            "UPDATE crates SET producer='Andrew', status='released' WHERE id=?",
            [crate_id],
        )
        .unwrap();
        conn.execute("UPDATE tracks SET favorited=1 WHERE id=?", [track_id])
            .unwrap();
        conn.execute(
            "INSERT INTO track_notes (track_id, note) VALUES (?, 'keep me')",
            [track_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO track_tags (track_id, tag, created_at) VALUES (?, 'lofi', 0)",
            [track_id],
        )
        .unwrap();

        // Rename the folder on disk, then re-ingest.
        let crate_b = albums.join("Album B");
        std::fs::rename(&crate_a, &crate_b).unwrap();
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();

        // Exactly one crate, same id, new folder/name.
        let crate_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM crates", [], |r| r.get(0))
            .unwrap();
        assert_eq!(crate_count, 1, "rename must not create a duplicate crate");
        let (id2, folder2, name2): (i64, String, String) = conn
            .query_row("SELECT id, folder, name FROM crates", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(id2, crate_id, "crate id must survive the rename");
        assert_eq!(folder2, crate_b.to_str().unwrap());
        assert_eq!(name2, "Album B");

        // Crate + track metadata preserved.
        let producer: String = conn
            .query_row("SELECT producer FROM crates", [], |r| r.get(0))
            .unwrap();
        let status: String = conn
            .query_row("SELECT status FROM crates", [], |r| r.get(0))
            .unwrap();
        assert_eq!(producer, "Andrew");
        assert_eq!(status, "released");

        let (tid2, fav): (i64, i64) = conn
            .query_row(
                "SELECT id, favorited FROM tracks WHERE filename='one.wav'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(tid2, track_id, "track id must survive the rename");
        assert_eq!(fav, 1);
        let notes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM track_notes WHERE track_id=?",
                [track_id],
                |r| r.get(0),
            )
            .unwrap();
        let tags: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM track_tags WHERE track_id=?",
                [track_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(notes, 1, "track note must survive the rename");
        assert_eq!(tags, 1, "track tag must survive the rename");
    }

    /// A genuinely new folder (different file set) is NOT mistaken for a rename.
    #[test]
    fn distinct_content_is_not_treated_as_rename() {
        let tmp = tempfile::tempdir().unwrap();
        let albums = tmp.path();
        let mut conn = mem_db();

        let a = albums.join("Album A");
        std::fs::create_dir(&a).unwrap();
        touch(&a, "one.wav");
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();

        // Remove A, add B with a DIFFERENT file set.
        std::fs::remove_dir_all(&a).unwrap();
        let b = albums.join("Album B");
        std::fs::create_dir(&b).unwrap();
        touch(&b, "different.wav");
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();

        // Orphan A remains (we never prune vanished crates) + fresh B → 2.
        let crate_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM crates", [], |r| r.get(0))
            .unwrap();
        assert_eq!(crate_count, 2);
    }

    /// H2: a crate folder that stays on disk but loses all its audio is deleted,
    /// taking its tracks (and their notes/tags) with it via cascade — but only
    /// after PRUNE_GRACE_SECS, so a re-export of a one-track crate doesn't wipe it.
    #[test]
    fn emptied_crate_folder_is_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let albums = tmp.path();
        let mut conn = mem_db();

        let a = albums.join("Album A");
        std::fs::create_dir(&a).unwrap();
        touch(&a, "one.wav");
        touch(&a, "two.wav");
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();

        // Attach metadata so we also prove the cascade reaches notes/tags.
        let track_id: i64 = conn
            .query_row("SELECT id FROM tracks WHERE filename='one.wav'", [], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO track_notes (track_id, note) VALUES (?, 'gone soon')",
            [track_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO track_tags (track_id, tag, created_at) VALUES (?, 'lofi', 0)",
            [track_id],
        )
        .unwrap();

        // Remove every audio file but keep the (now empty) folder on disk.
        std::fs::remove_file(a.join("one.wav")).unwrap();
        std::fs::remove_file(a.join("two.wav")).unwrap();
        assert!(a.is_dir(), "folder must still exist for this case");
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();

        // Inside the grace window: stamped, not deleted.
        let crates_now: i64 = conn
            .query_row("SELECT COUNT(*) FROM crates", [], |r| r.get(0))
            .unwrap();
        assert_eq!(crates_now, 1, "crate must survive the grace window");

        // Age the stamp past the window, then re-ingest.
        conn.execute(
            &format!("UPDATE crates SET emptied_since = unixepoch() - {}", PRUNE_GRACE_SECS + 1),
            [],
        )
        .unwrap();
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();

        let crates: i64 = conn
            .query_row("SELECT COUNT(*) FROM crates", [], |r| r.get(0))
            .unwrap();
        let tracks: i64 = conn
            .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
            .unwrap();
        let notes: i64 = conn
            .query_row("SELECT COUNT(*) FROM track_notes", [], |r| r.get(0))
            .unwrap();
        let tags: i64 = conn
            .query_row("SELECT COUNT(*) FROM track_tags", [], |r| r.get(0))
            .unwrap();
        assert_eq!(crates, 0, "emptied crate must be deleted");
        assert_eq!(tracks, 0, "tracks must cascade-delete with the crate");
        assert_eq!(notes, 0, "notes must cascade-delete with the track");
        assert_eq!(tags, 0, "tags must cascade-delete with the track");
    }

    /// Re-exporting a track over its own path must be seamless: same row (so
    /// sort_order, notes, tags, favorite all survive), but the measurements
    /// re-taken — replay_gain nulled for the worker, duration re-read.
    #[test]
    fn reexport_preserves_row_and_invalidates_measurements() {
        let tmp = tempfile::tempdir().unwrap();
        let albums = tmp.path();
        let mut conn = mem_db();

        let a = albums.join("Album A");
        std::fs::create_dir(&a).unwrap();
        std::fs::write(a.join("one.wav"), b"original").unwrap();
        std::fs::write(a.join("two.wav"), b"other").unwrap();
        assert!(ingest_albums_folder(&mut conn, albums.to_str().unwrap())
            .unwrap()
            .is_empty());

        let track_id: i64 = conn
            .query_row("SELECT id FROM tracks WHERE filename='one.wav'", [], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "UPDATE tracks SET sort_order=0, favorited=1, replay_gain=-9.5 WHERE id=?",
            [track_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO track_notes (track_id, note) VALUES (?, 'keep me')",
            [track_id],
        )
        .unwrap();

        // Re-export: same path, different bytes (and therefore a different size).
        std::fs::write(a.join("one.wav"), b"re-exported, louder kick").unwrap();
        let invalidated = ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();
        assert_eq!(invalidated, vec![track_id], "only the changed file");

        let (id2, sort_order, fav, gain): (i64, Option<i64>, i64, Option<f64>) = conn
            .query_row(
                "SELECT id, sort_order, favorited, replay_gain FROM tracks WHERE filename='one.wav'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(id2, track_id, "the row must survive a re-export");
        assert_eq!(sort_order, Some(0), "position must not move");
        assert_eq!(fav, 1);
        assert_eq!(gain, None, "replay_gain must be re-measured, not kept");
        let notes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM track_notes WHERE track_id=?",
                [track_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(notes, 1, "notes must survive a re-export");

        // The untouched sibling keeps its measurement.
        let other_gain: Option<f64> = conn
            .query_row(
                "SELECT replay_gain FROM tracks WHERE filename='two.wav'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(other_gain, None, "sibling was never analysed here");

        // A no-op re-ingest must not invalidate anything again.
        conn.execute("UPDATE tracks SET replay_gain=-7.0 WHERE id=?", [track_id])
            .unwrap();
        assert!(ingest_albums_folder(&mut conn, albums.to_str().unwrap())
            .unwrap()
            .is_empty());
        let gain2: Option<f64> = conn
            .query_row("SELECT replay_gain FROM tracks WHERE id=?", [track_id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(gain2, Some(-7.0), "unchanged file must keep its measurement");
    }

    /// Pre-migration rows carry a NULL fingerprint. The first ingest after the
    /// upgrade must adopt what's on disk, NOT read "unknown" as "changed" — that
    /// would null replay_gain across the whole library and trigger a full
    /// re-analysis of every track.
    #[test]
    fn null_fingerprint_is_adopted_not_invalidated() {
        let tmp = tempfile::tempdir().unwrap();
        let albums = tmp.path();
        let mut conn = mem_db();

        let a = albums.join("Album A");
        std::fs::create_dir(&a).unwrap();
        std::fs::write(a.join("one.wav"), b"original").unwrap();
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();

        // Simulate a row that predates the fingerprint columns.
        conn.execute(
            "UPDATE tracks SET file_mtime=NULL, file_size=NULL, replay_gain=-9.5",
            [],
        )
        .unwrap();

        let invalidated = ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();
        assert!(invalidated.is_empty(), "unknown fingerprint is not a change");
        let (mtime, size, gain): (Option<i64>, Option<i64>, Option<f64>) = conn
            .query_row(
                "SELECT file_mtime, file_size, replay_gain FROM tracks",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(mtime.is_some() && size.is_some(), "fingerprint adopted");
        assert_eq!(gain, Some(-9.5), "measurement kept");
    }

    /// A file that vanishes for one scan (the DAW's delete-then-write window) must
    /// not take its row — and everything cascading off it — with it. Only a file
    /// still missing past PRUNE_GRACE_SECS is pruned.
    #[test]
    fn missing_file_survives_the_grace_window() {
        let tmp = tempfile::tempdir().unwrap();
        let albums = tmp.path();
        let mut conn = mem_db();

        let a = albums.join("Album A");
        std::fs::create_dir(&a).unwrap();
        touch(&a, "one.wav");
        touch(&a, "two.wav");
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();
        let track_id: i64 = conn
            .query_row("SELECT id FROM tracks WHERE filename='one.wav'", [], |r| {
                r.get(0)
            })
            .unwrap();

        // Mid-export: the file is momentarily gone.
        std::fs::remove_file(a.join("one.wav")).unwrap();
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();
        let (still_there, missing_since): (i64, Option<i64>) = conn
            .query_row(
                "SELECT COUNT(*), MAX(missing_since) FROM tracks WHERE id=?",
                [track_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(still_there, 1, "a transient gap must not delete the row");
        assert!(missing_since.is_some(), "row is stamped while missing");

        // The file comes back (the export finished) — the stamp must clear, or a
        // file that flickers repeatedly would eventually cross the cutoff.
        touch(&a, "one.wav");
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();
        let missing_since: Option<i64> = conn
            .query_row("SELECT missing_since FROM tracks WHERE id=?", [track_id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(missing_since, None, "reappearing file resets the clock");

        // Genuinely deleted: stamped, then pruned once the stamp ages out.
        std::fs::remove_file(a.join("one.wav")).unwrap();
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();
        conn.execute(
            &format!(
                "UPDATE tracks SET missing_since = unixepoch() - {} WHERE id = ?",
                PRUNE_GRACE_SECS + 1
            ),
            [track_id],
        )
        .unwrap();
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();
        let gone: i64 = conn
            .query_row("SELECT COUNT(*) FROM tracks WHERE id=?", [track_id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(gone, 0, "a file missing past the window is pruned");
    }

    /// H2 corollary: a brand-new folder that has never held audio must NOT create
    /// a phantom empty crate row.
    #[test]
    fn empty_folder_creates_no_crate() {
        let tmp = tempfile::tempdir().unwrap();
        let albums = tmp.path();
        let mut conn = mem_db();

        std::fs::create_dir(albums.join("Empty Folder")).unwrap();
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();

        let crates: i64 = conn
            .query_row("SELECT COUNT(*) FROM crates", [], |r| r.get(0))
            .unwrap();
        assert_eq!(crates, 0, "an empty folder must not become a crate");
    }
}
