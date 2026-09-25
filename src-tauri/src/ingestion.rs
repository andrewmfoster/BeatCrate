// Album-folder ingestion.
//
// Scans the configured Music Folder one level deep: each immediate subfolder is
// a "crate", each audio file inside it a "track". Upserts crates/tracks and
// prunes tracks whose files vanished (FK ON DELETE CASCADE cleans up tags/notes/
// play_log). Track duration via lofty (metadata read — no full decode).

use rusqlite::{params, Connection, OptionalExtension, ToSql};
use std::collections::{BTreeMap, BTreeSet, HashSet};
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
    #[cfg(test)]
    tests::DURATION_READS.with(|n| n.set(n.get() + 1));
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

    // H1: same for individual files renamed in place, re-cased, or moved to
    // another crate — re-attach the missing row to the new file before the
    // per-crate loop can insert a fresh row and stamp the old one for pruning.
    reconcile_moved_files(conn, &scans, &empty_folders)?;

    // A2 (09-23 audit H2): lofty only opens files that are new, re-exported, or
    // missing a duration. The watcher holds the DB lock for the whole ingest, and
    // parsing every file on every scan is what froze the UI during DAW exports.
    // A skipped file passes None, which the upsert's COALESCE keeps as-is.
    let mut invalidated: Vec<i64> = Vec::new();
    for scan in &scans {
        let stored = stored_fingerprints(conn, &scan.folder)?;
        let durations: Vec<Option<f64>> = scan
            .audio_files
            .iter()
            .zip(&scan.file_meta)
            .map(|(f, meta)| {
                let unchanged = matches!(
                    stored.get(f),
                    Some(&(Some(m), Some(s), Some(_))) if meta.mtime == Some(m) && meta.size == Some(s)
                );
                if unchanged {
                    None
                } else {
                    read_duration(&PathBuf::from(&scan.folder).join(f))
                }
            })
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

/// Detect audio files renamed (including a case-only rename) or moved between
/// crates, and re-point the existing track row at the new file in place — same
/// id, so notes/tags/favorite/play_log survive — instead of letting the per-crate
/// loop insert an empty row and prune the old one after PRUNE_GRACE_SECS.
///
/// A move is a track whose file is absent from its (on-disk) crate folder — just
/// now, or stamped `missing_since` within the grace window — paired with a file
/// that has no row in its crate, by exact `(file_mtime, file_size)`. rename(2)
/// keeps both. The pool is library-wide so a cross-crate move matches. A NULL
/// stored fingerprint is *unknown* and never matches. Only unambiguous 1:1
/// matches are acted on (a fingerprint shared by two missing rows or two new
/// files pairs nothing); anything else falls through to insert + stamp as before.
/// Not a content change, so nothing is invalidated: replay_gain stays.
fn reconcile_moved_files(
    conn: &mut Connection,
    scans: &[CrateScan],
    empty_folders: &[String],
) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;

    // Keyed by (mtime, size). Missing side: (track id, crate id, filename).
    // New side: (scan index, file index).
    type Fingerprint = (i64, i64);
    let mut missing: BTreeMap<Fingerprint, Vec<(i64, i64, String)>> = BTreeMap::new();
    let mut fresh: BTreeMap<Fingerprint, Vec<(usize, usize)>> = BTreeMap::new();
    let mut crate_ids: Vec<Option<i64>> = Vec::with_capacity(scans.len());

    {
        let mut crate_stmt = tx
            .prepare("SELECT id FROM crates WHERE folder = ?")
            .map_err(|e| e.to_string())?;
        let mut rows_stmt = tx
            .prepare(&format!(
                "SELECT id, filename, file_mtime, file_size,
                        missing_since IS NULL
                          OR missing_since > unixepoch() - {PRUNE_GRACE_SECS}
                 FROM tracks WHERE crate_id = ?"
            ))
            .map_err(|e| e.to_string())?;

        // Emptied folders contribute missing rows only (a move out of a crate's
        // last file); scans contribute both sides.
        let no_files: &[String] = &[];
        let folders = scans
            .iter()
            .map(|s| (s.folder.as_str(), s.audio_files.as_slice()))
            .chain(empty_folders.iter().map(|f| (f.as_str(), no_files)));
        for (idx, (folder, audio_files)) in folders.enumerate() {
            let crate_id: Option<i64> = crate_stmt
                .query_row([folder], |r| r.get(0))
                .optional()
                .map_err(|e| e.to_string())?;
            if idx < scans.len() {
                crate_ids.push(crate_id);
            }

            let mut known: HashSet<String> = HashSet::new();
            if let Some(crate_id) = crate_id {
                let on_disk: HashSet<&str> = audio_files.iter().map(|f| f.as_str()).collect();
                let rows = rows_stmt
                    .query_map([crate_id], |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, Option<i64>>(2)?,
                            r.get::<_, Option<i64>>(3)?,
                            r.get::<_, bool>(4)?,
                        ))
                    })
                    .map_err(|e| e.to_string())?;
                for row in rows {
                    let (id, filename, mtime, size, in_grace) = row.map_err(|e| e.to_string())?;
                    if !on_disk.contains(filename.as_str()) && in_grace {
                        if let (Some(mtime), Some(size)) = (mtime, size) {
                            missing.entry((mtime, size)).or_default().push((
                                id,
                                crate_id,
                                filename.clone(),
                            ));
                        }
                    }
                    known.insert(filename);
                }
            }

            if idx < scans.len() {
                for (fi, filename) in audio_files.iter().enumerate() {
                    let meta = scans[idx].file_meta[fi];
                    if known.contains(filename) {
                        continue;
                    }
                    if let (Some(mtime), Some(size)) = (meta.mtime, meta.size) {
                        fresh.entry((mtime, size)).or_default().push((idx, fi));
                    }
                }
            }
        }
    }

    let mut moved = 0;
    for (fingerprint, olds) in &missing {
        let news = match fresh.get(fingerprint) {
            Some(n) => n,
            None => continue,
        };
        if olds.len() != 1 || news.len() != 1 {
            continue;
        }
        let (id, old_crate_id, old_filename) = &olds[0];
        let (si, fi) = news[0];
        let scan = &scans[si];
        let filename = &scan.audio_files[fi];

        // A move into a folder that has no crate row yet creates it here; the
        // per-crate loop's upsert of the same folder is then a no-op refresh.
        let crate_id = match crate_ids[si] {
            Some(id) => id,
            None => {
                let id = upsert_crate(&tx, scan)?;
                crate_ids[si] = Some(id);
                id
            }
        };

        // Titles are only ever derived from the filename (nothing lets the user
        // edit one), so a rename recomputes it. A same-crate rename keeps
        // sort_order/track_num; a cross-crate move lands at the bottom of the new
        // crate the way a new file does (NULL sort_order, track_num by position).
        if crate_id == *old_crate_id {
            tx.execute(
                "UPDATE tracks SET filename = ?2, title = ?3, missing_since = NULL
                 WHERE id = ?1",
                params![id, filename, title_from_filename(filename)],
            )
            .map_err(|e| e.to_string())?;
            println!("[ingest] track renamed: {old_filename} → {filename}");
        } else {
            tx.execute(
                "UPDATE tracks SET crate_id = ?2, filename = ?3, title = ?4, track_num = ?5,
                                   sort_order = NULL, missing_since = NULL
                 WHERE id = ?1",
                params![
                    id,
                    crate_id,
                    filename,
                    title_from_filename(filename),
                    (fi as i64) + 1
                ],
            )
            .map_err(|e| e.to_string())?;
            println!(
                "[ingest] track moved: {old_filename} → {}",
                PathBuf::from(&scan.folder).join(filename).display()
            );
        }
        moved += 1;
    }

    tx.commit().map_err(|e| e.to_string())?;
    if moved > 0 {
        println!("[ingest] re-attached {moved} renamed/moved track(s)");
    }
    Ok(())
}

/// Insert-or-refresh a crate row by folder and return its id.
/// emptied_since clears here: the folder has audio again (or never lost it),
/// so any pending crate-level prune is cancelled.
type StoredFingerprints =
    std::collections::HashMap<String, (Option<i64>, Option<i64>, Option<f64>)>;

/// Stored (mtime, size, duration) per filename for the crate at `folder`. Read
/// after rename/move reconciliation, so a re-attached row is found under its new
/// name. Empty for a crate that has no row yet.
fn stored_fingerprints(conn: &Connection, folder: &str) -> Result<StoredFingerprints, String> {
    let mut stmt = conn
        .prepare(
            "SELECT t.filename, t.file_mtime, t.file_size, t.duration
             FROM tracks t JOIN crates c ON c.id = t.crate_id WHERE c.folder = ?",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([folder], |r| {
            Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

fn upsert_crate(conn: &Connection, scan: &CrateScan) -> Result<i64, String> {
    conn.execute(
        "INSERT INTO crates (name, folder, cover_path) VALUES (?1, ?2, ?3)
         ON CONFLICT(folder) DO UPDATE SET name=excluded.name, cover_path=excluded.cover_path,
           emptied_since=NULL",
        params![scan.name, scan.folder, scan.cover_path],
    )
    .map_err(|e| e.to_string())?;

    conn.query_row(
        "SELECT id FROM crates WHERE folder = ?",
        [&scan.folder],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

fn ingest_one_crate(
    conn: &mut Connection,
    scan: &CrateScan,
    durations: &[Option<f64>],
) -> Result<Vec<i64>, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;

    let crate_id = upsert_crate(&tx, scan)?;

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
                    let changed = known && (stored_mtime != meta.mtime || stored_size != meta.size);
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
        println!(
            "[ingest] pruned {removed} track(s) missing from {}",
            scan.folder
        );
    }

    tx.commit().map_err(|e| e.to_string())?;
    Ok(invalidated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    thread_local! {
        pub(super) static DURATION_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn duration_reads() -> usize {
        DURATION_READS.with(|n| n.replace(0))
    }

    /// A2: a rescan opens only files that are new, re-exported, or still missing a
    /// duration, and a skipped file keeps its stored duration.
    #[test]
    fn unchanged_files_are_not_reparsed() {
        let tmp = tempfile::tempdir().unwrap();
        let albums = tmp.path();
        let crate_dir = albums.join("Crate");
        std::fs::create_dir(&crate_dir).unwrap();
        write_at(&crate_dir, "a.wav", b"aaaa", 1_000);
        write_at(&crate_dir, "b.wav", b"bbbb", 1_000);
        let mut conn = mem_db();
        let folder = albums.to_str().unwrap();

        ingest_albums_folder(&mut conn, folder).unwrap();
        assert_eq!(duration_reads(), 2, "first scan reads every file");

        // Stub files have no parseable duration, so stand in a measured one.
        conn.execute("UPDATE tracks SET duration = 42.0", [])
            .unwrap();
        ingest_albums_folder(&mut conn, folder).unwrap();
        assert_eq!(duration_reads(), 0, "unchanged files are skipped");

        write_at(&crate_dir, "b.wav", b"bbbbbb", 2_000);
        write_at(&crate_dir, "c.wav", b"cccc", 1_000);
        ingest_albums_folder(&mut conn, folder).unwrap();
        assert_eq!(
            duration_reads(),
            2,
            "the re-export and the new file are read"
        );

        let a: Option<f64> = conn
            .query_row(
                "SELECT duration FROM tracks WHERE filename = 'a.wav'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(a, Some(42.0), "a skipped file keeps its duration");
    }

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
            &format!(
                "UPDATE crates SET emptied_since = unixepoch() - {}",
                PRUNE_GRACE_SECS + 1
            ),
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
            .query_row(
                "SELECT replay_gain FROM tracks WHERE id=?",
                [track_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            gain2,
            Some(-7.0),
            "unchanged file must keep its measurement"
        );
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
        assert!(
            invalidated.is_empty(),
            "unknown fingerprint is not a change"
        );
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
            .query_row(
                "SELECT missing_since FROM tracks WHERE id=?",
                [track_id],
                |r| r.get(0),
            )
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

    /// Write a file with a pinned mtime, so tests can make two fingerprints
    /// collide on purpose (or keep them apart) regardless of clock resolution.
    fn write_at(dir: &Path, name: &str, bytes: &[u8], mtime_secs: u64) {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime_secs))
            .unwrap();
    }

    fn track_id_of(conn: &Connection, filename: &str) -> Option<i64> {
        conn.query_row(
            "SELECT id FROM tracks WHERE filename = ?",
            [filename],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
    }

    fn count_for(conn: &Connection, table: &str, track_id: i64) -> i64 {
        conn.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE track_id = ?"),
            [track_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// H1: renaming a file in place keeps the row — id, notes, tags, favorite,
    /// position and measurement — and only the filename/title follow the file.
    #[test]
    fn file_rename_in_crate_keeps_row() {
        let tmp = tempfile::tempdir().unwrap();
        let albums = tmp.path();
        let mut conn = mem_db();

        let a = albums.join("Album A");
        std::fs::create_dir(&a).unwrap();
        write_at(&a, "01 beat.wav", b"beat bytes", 1_000);
        write_at(&a, "02 other.wav", b"other", 1_000);
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();

        let track_id = track_id_of(&conn, "01 beat.wav").unwrap();
        conn.execute(
            "UPDATE tracks SET sort_order=1, favorited=1, replay_gain=-9.5 WHERE id=?",
            [track_id],
        )
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

        std::fs::rename(a.join("01 beat.wav"), a.join("01 beat v2.wav")).unwrap();
        let invalidated = ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();
        assert!(invalidated.is_empty(), "a rename is not a content change");

        let (id2, title, sort_order, fav, gain, missing): (
            i64,
            String,
            Option<i64>,
            i64,
            Option<f64>,
            Option<i64>,
        ) = conn
            .query_row(
                "SELECT id, title, sort_order, favorited, replay_gain, missing_since
                 FROM tracks WHERE filename='01 beat v2.wav'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(id2, track_id, "track id must survive a file rename");
        assert_eq!(title, "beat v2", "title follows the new filename");
        assert_eq!(sort_order, Some(1), "position must not move");
        assert_eq!(fav, 1);
        assert_eq!(gain, Some(-9.5), "measurement kept");
        assert_eq!(missing, None);
        assert_eq!(count_for(&conn, "track_notes", track_id), 1);
        assert_eq!(count_for(&conn, "track_tags", track_id), 1);
        let tracks: i64 = conn
            .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tracks, 2, "no duplicate row for the renamed file");
    }

    /// H1: a case-only rename (`Beat.wav` → `beat.wav`) pairs rather than
    /// colliding — the filename column compares case-sensitively (BINARY), so
    /// the old spelling reads as missing and the new one as new.
    #[test]
    fn case_only_rename_keeps_row() {
        let tmp = tempfile::tempdir().unwrap();
        let albums = tmp.path();
        let mut conn = mem_db();

        let a = albums.join("Album A");
        std::fs::create_dir(&a).unwrap();
        write_at(&a, "Beat.wav", b"beat bytes", 1_000);
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();
        let track_id = track_id_of(&conn, "Beat.wav").unwrap();

        std::fs::rename(a.join("Beat.wav"), a.join("beat.wav")).unwrap();
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();

        assert_eq!(track_id_of(&conn, "beat.wav"), Some(track_id));
        assert_eq!(track_id_of(&conn, "Beat.wav"), None);
        let tracks: i64 = conn
            .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tracks, 1);
    }

    /// H1: dragging a file into another crate folder keeps its row (and notes),
    /// re-parented, landing at the bottom (NULL sort_order) like a new file.
    /// Moving a crate's last file into a folder with no crate yet works too.
    #[test]
    fn cross_crate_move_keeps_row() {
        let tmp = tempfile::tempdir().unwrap();
        let albums = tmp.path();
        let mut conn = mem_db();

        let a = albums.join("Album A");
        let b = albums.join("Album B");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        write_at(&a, "mover.wav", b"mover bytes", 1_000);
        write_at(&a, "last.wav", b"last one", 2_000);
        write_at(&b, "resident.wav", b"resident", 1_000);
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();

        let crate_b: i64 = conn
            .query_row("SELECT id FROM crates WHERE name='Album B'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let track_id = track_id_of(&conn, "mover.wav").unwrap();
        conn.execute("UPDATE tracks SET sort_order=0", []).unwrap();
        conn.execute(
            "INSERT INTO track_notes (track_id, note) VALUES (?, 'keep me')",
            [track_id],
        )
        .unwrap();

        std::fs::rename(a.join("mover.wav"), b.join("mover.wav")).unwrap();
        let invalidated = ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();
        assert!(invalidated.is_empty(), "a move is not a content change");

        let (id2, crate_id, sort_order, missing): (i64, i64, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT id, crate_id, sort_order, missing_since FROM tracks
                 WHERE filename='mover.wav'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(id2, track_id, "track id must survive a cross-crate move");
        assert_eq!(crate_id, crate_b, "re-parented to the destination crate");
        assert_eq!(sort_order, None, "lands at the bottom like a new file");
        assert_eq!(missing, None);
        assert_eq!(count_for(&conn, "track_notes", track_id), 1);

        // The source crate's last file, into a folder that has no crate row yet.
        let last_id = track_id_of(&conn, "last.wav").unwrap();
        let c = albums.join("Album C");
        std::fs::create_dir(&c).unwrap();
        std::fs::rename(a.join("last.wav"), c.join("last.wav")).unwrap();
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();

        let crate_name: String = conn
            .query_row(
                "SELECT c.name FROM tracks t JOIN crates c ON c.id = t.crate_id WHERE t.id = ?",
                [last_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(crate_name, "Album C");
        let tracks: i64 = conn
            .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tracks, 3, "no duplicate rows after either move");
    }

    /// Two missing rows sharing a fingerprint can't be told apart — neither is
    /// paired, and the new file gets a fresh row (today's behaviour).
    #[test]
    fn ambiguous_missing_rows_do_not_pair() {
        let tmp = tempfile::tempdir().unwrap();
        let albums = tmp.path();
        let mut conn = mem_db();

        let a = albums.join("Album A");
        std::fs::create_dir(&a).unwrap();
        write_at(&a, "x.wav", b"same", 1_000);
        write_at(&a, "y.wav", b"same", 1_000);
        write_at(&a, "keep.wav", b"keeper", 1_000);
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();
        let x_id = track_id_of(&conn, "x.wav").unwrap();
        let y_id = track_id_of(&conn, "y.wav").unwrap();

        std::fs::remove_file(a.join("y.wav")).unwrap();
        std::fs::rename(a.join("x.wav"), a.join("x2.wav")).unwrap();
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();

        let x2_id = track_id_of(&conn, "x2.wav").unwrap();
        assert!(x2_id != x_id && x2_id != y_id, "ambiguous → fresh row");
        let stamped: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tracks WHERE missing_since IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stamped, 2, "both old rows stamped as before");
    }

    /// Two new files sharing the missing row's fingerprint (a rename plus a
    /// copy) can't be told apart either — no pairing.
    #[test]
    fn ambiguous_new_files_do_not_pair() {
        let tmp = tempfile::tempdir().unwrap();
        let albums = tmp.path();
        let mut conn = mem_db();

        let a = albums.join("Album A");
        std::fs::create_dir(&a).unwrap();
        write_at(&a, "x.wav", b"same", 1_000);
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();
        let x_id = track_id_of(&conn, "x.wav").unwrap();

        std::fs::rename(a.join("x.wav"), a.join("x2.wav")).unwrap();
        write_at(&a, "x copy.wav", b"same", 1_000);
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();

        let x2_id = track_id_of(&conn, "x2.wav").unwrap();
        let copy_id = track_id_of(&conn, "x copy.wav").unwrap();
        assert!(x2_id != x_id && copy_id != x_id, "ambiguous → fresh rows");
        let missing: Option<i64> = conn
            .query_row("SELECT missing_since FROM tracks WHERE id=?", [x_id], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(missing.is_some(), "old row stamped as before");
    }

    /// A NULL stored fingerprint is *unknown*, never a match — even when it's
    /// the only missing row and the only new file.
    #[test]
    fn null_fingerprint_never_pairs() {
        let tmp = tempfile::tempdir().unwrap();
        let albums = tmp.path();
        let mut conn = mem_db();

        let a = albums.join("Album A");
        std::fs::create_dir(&a).unwrap();
        write_at(&a, "one.wav", b"one", 1_000);
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();
        let track_id = track_id_of(&conn, "one.wav").unwrap();
        conn.execute("UPDATE tracks SET file_mtime=NULL, file_size=NULL", [])
            .unwrap();

        std::fs::rename(a.join("one.wav"), a.join("one v2.wav")).unwrap();
        ingest_albums_folder(&mut conn, albums.to_str().unwrap()).unwrap();

        assert_ne!(track_id_of(&conn, "one v2.wav"), Some(track_id));
        let missing: Option<i64> = conn
            .query_row(
                "SELECT missing_since FROM tracks WHERE id=?",
                [track_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(missing.is_some(), "old row stamped as before");
    }
}
