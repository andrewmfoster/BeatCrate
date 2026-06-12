// Ableton Live Set (.als) parsing + indexing.
//
// An .als is a gzipped XML document. We gunzip it (flate2) and walk it read-only
// with roxmltree (a borrowed DOM — a better fit for the descendant walks than a
// streaming parser). We extract BPM, key, time-sig, active-track count,
// plugin + sample lists, Ableton version, and the file/folder dates, then
// upsert into als_project_index (+ als_plugins / als_samples).
//
// NOTE: indexCrateFolder is intentionally omitted — it was dead code (never
// called, and wrote a tracks.ableton_project_path column that isn't in the
// schema).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use roxmltree::Node;
use rusqlite::{params, Connection, Transaction};

pub struct AlsMeta {
    pub bpm: Option<f64>,
    pub key_root: Option<i64>,
    // Scale NAME as Ableton wrote it (text). Newer Live (11/12) stores "Major"/
    // "Minor"; older Live stored an integer scale index (kept verbatim as a
    // string). See parse_als + the key_scale column note in db.rs.
    pub key_scale: Option<String>,
    pub key_confirmed: bool,
    pub time_sig_num: i64,
    pub time_sig_den: i64,
    pub active_track_count: i64,
    pub plugins: Vec<String>,
    pub samples: Vec<String>,
    pub ableton_version: Option<String>,
    pub file_mod_date: i64,
    pub created_date: i64,
}

// ── tree helpers (roxmltree) ─────────────────────────────────────────────────

/// First direct child element with the given tag name.
fn child<'a, 'i>(node: Node<'a, 'i>, tag: &str) -> Option<Node<'a, 'i>> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == tag)
}

/// Walk a chain of single-child tags (e.g. DeviceChain → Mixer → Tempo).
fn chain<'a, 'i>(node: Node<'a, 'i>, path: &[&str]) -> Option<Node<'a, 'i>> {
    let mut cur = node;
    for tag in path {
        cur = child(cur, tag)?;
    }
    Some(cur)
}

/// The `Value` attribute of an element.
fn get_val<'a>(node: Node<'a, '_>) -> Option<&'a str> {
    node.attribute("Value")
}

/// True if the node has at least one child element (Events/Devices contain
/// elements, not text).
fn has_element_children(node: Node) -> bool {
    node.children().any(|c| c.is_element())
}

/// Depth-first search preferring direct children: check this node's direct
/// children for `tag` first, then descend in order.
fn find_first<'a, 'i>(node: Node<'a, 'i>, tag: &str) -> Option<Node<'a, 'i>> {
    if let Some(c) = child(node, tag) {
        return Some(c);
    }
    for c in node.children().filter(|c| c.is_element()) {
        if let Some(found) = find_first(c, tag) {
            return Some(found);
        }
    }
    None
}

// ── parse ────────────────────────────────────────────────────────────────────

fn systime_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn parse_als(als_path: &Path) -> Option<AlsMeta> {
    let file_meta = std::fs::metadata(als_path).ok()?;
    let file_mod_date = systime_secs(file_meta.modified().ok()?);

    // Use the project *folder* birthtime — Ableton rewrites the .als on every
    // save, so the file's own birthtime tracks the last save, not the original
    // creation. The folder is created once and never recreated.
    let created_date = als_path
        .parent()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.created().ok())
        .map(systime_secs)
        .filter(|&s| s > 0)
        .unwrap_or(file_mod_date);

    let compressed = std::fs::read(als_path).ok()?;
    let mut xml = String::new();
    if flate2::read::GzDecoder::new(&compressed[..])
        .read_to_string(&mut xml)
        .is_err()
    {
        eprintln!("[als-parser] gunzip/read failed {}", als_path.display());
        return None;
    }

    let doc = match roxmltree::Document::parse(&xml) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[als-parser] xml parse failed {}: {e}", als_path.display());
            return None;
        }
    };

    let ableton = doc.root_element();
    let ableton_version = ableton.attribute("Creator").map(|s| s.to_string());
    let live_set = child(ableton, "LiveSet")?;

    // BPM: MainTrack mixer tempo, else top-level Tempo, else anywhere.
    let tempo_node = chain(live_set, &["MainTrack", "DeviceChain", "Mixer", "Tempo"])
        .or_else(|| child(live_set, "Tempo"))
        .or_else(|| find_first(live_set, "Tempo"));
    let bpm = tempo_node
        .and_then(|t| child(t, "Manual"))
        .and_then(get_val)
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|&f| f != 0.0);

    // Key (only when InKey is true).
    let mut key_root = None;
    let mut key_scale = None;
    let mut key_confirmed = false;
    if let Some(scale_info) = child(live_set, "ScaleInformation") {
        if child(live_set, "InKey").and_then(get_val) == Some("true") {
            key_confirmed = true;
            key_root = child(scale_info, "Root")
                .and_then(get_val)
                .and_then(|v| v.parse::<i64>().ok());
            // Store the scale NAME verbatim as text. Newer Live writes "Major"/
            // "Minor"; older Live wrote an integer index. The original port did
            // `.parse::<i64>()` here, which silently dropped every text name
            // (NULLing key_scale for any newer-Live confirmed key). Reading it as
            // a string captures both formats. (audit M1, 2026-05-31)
            key_scale = child(scale_info, "Name")
                .and_then(get_val)
                .map(|v| v.to_string())
                .filter(|s| !s.is_empty());
        }
    }

    // Time signature (default 4/4).
    let mut time_sig_num = 4i64;
    let mut time_sig_den = 4i64;
    if let Some(rts) = find_first(live_set, "RemoteableTimeSignature") {
        if let Some(n) = child(rts, "Numerator")
            .and_then(get_val)
            .and_then(|v| v.parse::<i64>().ok())
        {
            time_sig_num = n;
        }
        if let Some(d) = child(rts, "Denominator")
            .and_then(get_val)
            .and_then(|v| v.parse::<i64>().ok())
        {
            time_sig_den = d;
        }
    }

    // Active track counts (Master always counts as 1).
    let mut active_track_count = 1i64;
    if let Some(tracks) = child(live_set, "Tracks") {
        for t in tracks.children().filter(|c| c.is_element()) {
            let counts = match t.tag_name().name() {
                "MidiTrack" => chain(
                    t,
                    &[
                        "DeviceChain",
                        "MainSequencer",
                        "ClipTimeable",
                        "ArrangerAutomation",
                        "Events",
                    ],
                )
                .map(has_element_children)
                .unwrap_or(false),
                "AudioTrack" => chain(
                    t,
                    &[
                        "DeviceChain",
                        "MainSequencer",
                        "Sample",
                        "ArrangerAutomation",
                        "Events",
                    ],
                )
                .map(has_element_children)
                .unwrap_or(false),
                "GroupTrack" | "ReturnTrack" => chain(t, &["DeviceChain", "Devices"])
                    .map(has_element_children)
                    .unwrap_or(false),
                _ => false,
            };
            if counts {
                active_track_count += 1;
            }
        }
    }

    // Plugins + samples — full-document walks with global dedup, insertion order
    // preserved.
    let mut plugins = Vec::new();
    let mut seen_plugin = std::collections::HashSet::new();
    for desc in doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "PluginDesc")
    {
        for path in [
            ["VstPluginInfo", "PlugName"],
            ["Vst3PluginInfo", "Name"],
            ["AuPluginInfo", "Name"],
        ] {
            if let Some(name) = chain(desc, &path).and_then(get_val) {
                if !name.is_empty() && seen_plugin.insert(name.to_string()) {
                    plugins.push(name.to_string());
                }
            }
        }
    }

    let mut samples = Vec::new();
    let mut seen_sample = std::collections::HashSet::new();
    for sref in doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "SampleRef")
    {
        if let Some(name) = chain(sref, &["FileRef", "Name"]).and_then(get_val) {
            if !name.trim().is_empty() {
                let base = Path::new(name)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| name.to_string());
                if seen_sample.insert(base.clone()) {
                    samples.push(base);
                }
            }
        }
    }

    Some(AlsMeta {
        bpm,
        key_root,
        key_scale,
        key_confirmed,
        time_sig_num,
        time_sig_den,
        active_track_count,
        plugins,
        samples,
        ableton_version,
        file_mod_date,
        created_date,
    })
}

// ── indexer ────────────────────────────────────────────────────────────────

/// Recursively collect all .als paths under `root`, skipping `Backup` folders.
///
/// M3: the *root* must be readable — an unreadable root (moved/deleted/no
/// permission) is a configuration error, not a legitimate "0 projects" result, so
/// it returns Err and the caller surfaces it. Unreadable *sub*directories are
/// logged and skipped: one bad subfolder shouldn't abort the whole index.
pub fn find_als_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(root).map_err(|e| {
        format!(
            "Couldn't read the Ableton projects folder ({}): {e}",
            root.display()
        )
    })?;
    let mut out = Vec::new();
    walk_entries(entries, &mut out);
    Ok(out)
}

fn walk_als(dir: &Path, out: &mut Vec<PathBuf>) {
    match std::fs::read_dir(dir) {
        Ok(entries) => walk_entries(entries, out),
        Err(e) => eprintln!(
            "[als-indexer] skipping unreadable dir {}: {e}",
            dir.display()
        ),
    }
}

fn walk_entries(entries: std::fs::ReadDir, out: &mut Vec<PathBuf>) {
    for entry in entries.flatten() {
        if entry.file_name() == std::ffi::OsStr::new("Backup") {
            continue;
        }
        let full = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => walk_als(&full, out),
            Ok(ft)
                if ft.is_file()
                    && full
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.eq_ignore_ascii_case("als"))
                        .unwrap_or(false) =>
            {
                out.push(full);
            }
            _ => {}
        }
    }
}

/// Re-scan the Ableton root and refresh als_project_index. Parsing happens
/// before the transaction is opened so the DB lock is held only for the writes.
/// Returns (scanned, failed_filenames, pruned) — M10: the failed *names* (not
/// just a count) so the renderer can tell the user which projects wouldn't parse;
/// `pruned` is how many orphan rows (projects whose .als no longer exists) were
/// removed so Career Arc / insights match what's actually on disk.
pub fn index_als_root(
    conn: &mut Connection,
    root: &str,
) -> Result<(i64, Vec<String>, i64), String> {
    let files = find_als_files(Path::new(root))?;
    // Full path set of every .als on disk (parsed or not) — the prune keyset.
    let on_disk: std::collections::HashSet<String> = files
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let mut parsed: Vec<(String, AlsMeta)> = Vec::new();
    let mut failed: Vec<String> = Vec::new();

    for als in &files {
        match parse_als(als) {
            Some(meta) => parsed.push((als.to_string_lossy().into_owned(), meta)),
            None => {
                eprintln!("[als-indexer] FAILED  {}", als.display());
                failed.push(
                    als.file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| als.to_string_lossy().into_owned()),
                );
            }
        }
    }

    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for (path, meta) in &parsed {
        upsert_one(&tx, path, meta)?;
    }

    // Prune orphan rows: indexed projects whose .als no longer exists on disk
    // (deleted/moved). The indexer is otherwise insert-only, so these accumulate
    // and inflate Career Arc. GUARDED: only prune when the scan actually found
    // files — a misconfigured/unreadable root yields an empty set, and we must
    // not wipe the whole index in that case. Failed-to-parse files stay (they're
    // still on disk, so they're in `on_disk`). Deleting the project row cascades
    // its plugins/samples via FK ON DELETE CASCADE.
    let mut pruned = 0i64;
    if !on_disk.is_empty() {
        let existing: Vec<String> = {
            let mut stmt = tx
                .prepare("SELECT als_path FROM als_project_index")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            rows.filter_map(Result::ok).collect()
        };
        for path in existing {
            if !on_disk.contains(&path) {
                tx.execute("DELETE FROM als_project_index WHERE als_path = ?", [&path])
                    .map_err(|e| e.to_string())?;
                pruned += 1;
            }
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    if pruned > 0 {
        println!("[als-indexer] pruned {pruned} orphan project row(s)");
    }

    Ok((parsed.len() as i64, failed, pruned))
}

fn upsert_one(tx: &Transaction, als_path: &str, meta: &AlsMeta) -> Result<(), String> {
    // INSERT OR REPLACE drops the old row (cascading away its plugins/samples via
    // FK ON DELETE CASCADE), then we repopulate them under the fresh id.
    tx.execute(
        "INSERT OR REPLACE INTO als_project_index
           (als_path, bpm, key_root, key_scale, key_confirmed, time_sig_num, time_sig_den,
            active_track_count, plugin_count, ableton_version, file_mod_date, created_date, parsed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, unixepoch())",
        params![
            als_path,
            meta.bpm,
            meta.key_root,
            meta.key_scale,
            meta.key_confirmed as i64,
            meta.time_sig_num,
            meta.time_sig_den,
            meta.active_track_count,
            meta.plugins.len() as i64,
            meta.ableton_version,
            meta.file_mod_date,
            meta.created_date,
        ],
    )
    .map_err(|e| e.to_string())?;

    let id: i64 = tx
        .query_row(
            "SELECT id FROM als_project_index WHERE als_path = ?",
            [als_path],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;

    tx.execute("DELETE FROM als_plugins WHERE project_id = ?", [id])
        .map_err(|e| e.to_string())?;
    {
        let mut ins = tx
            .prepare("INSERT INTO als_plugins (project_id, plugin_name) VALUES (?, ?)")
            .map_err(|e| e.to_string())?;
        for name in &meta.plugins {
            ins.execute(params![id, name]).map_err(|e| e.to_string())?;
        }
    }

    tx.execute("DELETE FROM als_samples WHERE project_id = ?", [id])
        .map_err(|e| e.to_string())?;
    {
        let mut ins = tx
            .prepare("INSERT INTO als_samples (project_id, sample_name) VALUES (?, ?)")
            .map_err(|e| e.to_string())?;
        for name in &meta.samples {
            ins.execute(params![id, name]).map_err(|e| e.to_string())?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M3: an unreadable/nonexistent root is an error, not a clean "0 sessions".
    #[test]
    fn unreadable_root_is_an_error() {
        let missing = std::path::Path::new("/no/such/ableton/root/xyzzy");
        assert!(find_als_files(missing).is_err());
    }

    /// A readable root with no .als files is a legitimate empty result, not an error.
    #[test]
    fn empty_root_is_ok_and_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let files = find_als_files(tmp.path()).expect("readable empty root must be Ok");
        assert!(files.is_empty());
    }

    /// .als files are found recursively; `Backup` folders are skipped.
    #[test]
    fn finds_als_recursively_and_skips_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let proj = root.join("Song Project");
        std::fs::create_dir(&proj).unwrap();
        std::fs::write(proj.join("Song.als"), b"x").unwrap();
        let backup = proj.join("Backup");
        std::fs::create_dir(&backup).unwrap();
        std::fs::write(backup.join("Song [2024-01-01].als"), b"x").unwrap();

        let files = find_als_files(root).unwrap();
        assert_eq!(files.len(), 1, "Backup/ copies must be skipped");
        assert!(files[0].ends_with("Song.als"));
    }
}
