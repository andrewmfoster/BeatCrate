#include "BeatCrateDB.h"
#include "sqlite3.h"

namespace
{
    juce::String columnText (sqlite3_stmt* stmt, int col)
    {
        if (auto* text = reinterpret_cast<const char*> (sqlite3_column_text (stmt, col)))
            return juce::String::fromUTF8 (text);
        return {};
    }
}

BeatCrateDB::~BeatCrateDB()
{
    close();
}

juce::File BeatCrateDB::defaultDbPath()
{
   #if JUCE_MAC
    // userApplicationDataDirectory on macOS = ~/Library; the BeatCrate desktop app
    // writes to ~/Library/Application Support/BeatCrate/, so reach in there explicitly.
    return juce::File::getSpecialLocation (juce::File::userApplicationDataDirectory)
        .getChildFile ("Application Support")
        .getChildFile ("BeatCrate")
        .getChildFile ("beatcrate.db");
   #else
    return juce::File::getSpecialLocation (juce::File::userApplicationDataDirectory)
        .getChildFile ("BeatCrate")
        .getChildFile ("beatcrate.db");
   #endif
}

bool BeatCrateDB::open (const juce::File& dbFile)
{
    const juce::ScopedLock sl (lock);
    close();

    if (! dbFile.existsAsFile())
    {
        lastErr = "DB file not found: " + dbFile.getFullPathName();
        return false;
    }

    const int flags = SQLITE_OPEN_READWRITE | SQLITE_OPEN_FULLMUTEX;
    const int rc = sqlite3_open_v2 (dbFile.getFullPathName().toRawUTF8(), &db, flags, nullptr);

    if (rc != SQLITE_OK)
    {
        lastErr = "sqlite3_open_v2 failed: " + juce::String (sqlite3_errmsg (db));
        sqlite3_close (db);
        db = nullptr;
        return false;
    }

    sqlite3_busy_timeout (db, 2000);
    return true;
}

void BeatCrateDB::close()
{
    const juce::ScopedLock sl (lock);
    if (db != nullptr)
    {
        sqlite3_close (db);
        db = nullptr;
    }
}

std::vector<Crate> BeatCrateDB::listCrates()
{
    const juce::ScopedLock sl (lock);
    std::vector<Crate> out;
    if (! isOpen()) return out;

    const char* sql = "SELECT id, name FROM crates ORDER BY name COLLATE NOCASE ASC";
    sqlite3_stmt* stmt = nullptr;
    if (sqlite3_prepare_v2 (db, sql, -1, &stmt, nullptr) != SQLITE_OK)
    {
        lastErr = "listCrates prepare: " + juce::String (sqlite3_errmsg (db));
        return out;
    }

    int rc;
    while ((rc = sqlite3_step (stmt)) == SQLITE_ROW)
    {
        Crate c;
        c.id   = sqlite3_column_int64 (stmt, 0);
        c.name = columnText (stmt, 1);
        out.push_back (std::move (c));
    }

    if (rc != SQLITE_DONE)
    {
        // SQLITE_BUSY/IOERR/etc. mid-iteration: discard the partial set rather than
        // silently showing a truncated crate list.
        lastErr = "listCrates step: " + juce::String (sqlite3_errmsg (db));
        out.clear();
    }

    sqlite3_finalize (stmt);
    return out;
}

std::vector<Track> BeatCrateDB::listTracks (int64_t crateId)
{
    const juce::ScopedLock sl (lock);
    std::vector<Track> out;
    if (! isOpen()) return out;

    const char* sql =
        "SELECT id, title FROM tracks "
        "WHERE crate_id = ? "
        "ORDER BY COALESCE(sort_order, track_num, id) ASC";

    sqlite3_stmt* stmt = nullptr;
    if (sqlite3_prepare_v2 (db, sql, -1, &stmt, nullptr) != SQLITE_OK)
    {
        lastErr = "listTracks prepare: " + juce::String (sqlite3_errmsg (db));
        return out;
    }

    sqlite3_bind_int64 (stmt, 1, crateId);

    int rc;
    while ((rc = sqlite3_step (stmt)) == SQLITE_ROW)
    {
        Track t;
        t.id    = sqlite3_column_int64 (stmt, 0);
        t.title = columnText (stmt, 1);
        out.push_back (std::move (t));
    }

    if (rc != SQLITE_DONE)
    {
        lastErr = "listTracks step: " + juce::String (sqlite3_errmsg (db));
        out.clear();
    }

    sqlite3_finalize (stmt);
    return out;
}

bool BeatCrateDB::updateNoteCompleted (int64_t noteId, int64_t trackId, bool completed)
{
    const juce::ScopedLock sl (lock);
    if (! isOpen()) return false;

    const char* sql = "UPDATE track_notes SET completed = ? WHERE id = ? AND track_id = ?";
    sqlite3_stmt* stmt = nullptr;
    if (sqlite3_prepare_v2 (db, sql, -1, &stmt, nullptr) != SQLITE_OK)
    {
        lastErr = "updateNoteCompleted prepare: " + juce::String (sqlite3_errmsg (db));
        return false;
    }

    sqlite3_bind_int   (stmt, 1, completed ? 1 : 0);
    sqlite3_bind_int64 (stmt, 2, noteId);
    sqlite3_bind_int64 (stmt, 3, trackId);

    const bool ok = sqlite3_step (stmt) == SQLITE_DONE;
    if (! ok) lastErr = "updateNoteCompleted step: " + juce::String (sqlite3_errmsg (db));
    sqlite3_finalize (stmt);
    return ok;
}

bool BeatCrateDB::updateNoteText (int64_t noteId, int64_t trackId, const juce::String& text)
{
    const juce::ScopedLock sl (lock);
    if (! isOpen()) return false;
    const auto trimmed = text.trim();
    if (trimmed.isEmpty()) return false;

    const char* sql = "UPDATE track_notes SET note = ? WHERE id = ? AND track_id = ?";
    sqlite3_stmt* stmt = nullptr;
    if (sqlite3_prepare_v2 (db, sql, -1, &stmt, nullptr) != SQLITE_OK)
    {
        lastErr = "updateNoteText prepare: " + juce::String (sqlite3_errmsg (db));
        return false;
    }

    sqlite3_bind_text  (stmt, 1, trimmed.toRawUTF8(), -1, SQLITE_TRANSIENT);
    sqlite3_bind_int64 (stmt, 2, noteId);
    sqlite3_bind_int64 (stmt, 3, trackId);

    const bool ok = sqlite3_step (stmt) == SQLITE_DONE;
    if (! ok) lastErr = "updateNoteText step: " + juce::String (sqlite3_errmsg (db));
    sqlite3_finalize (stmt);
    return ok;
}

bool BeatCrateDB::deleteNote (int64_t noteId, int64_t trackId)
{
    const juce::ScopedLock sl (lock);
    if (! isOpen()) return false;

    const char* sql = "DELETE FROM track_notes WHERE id = ? AND track_id = ?";
    sqlite3_stmt* stmt = nullptr;
    if (sqlite3_prepare_v2 (db, sql, -1, &stmt, nullptr) != SQLITE_OK)
    {
        lastErr = "deleteNote prepare: " + juce::String (sqlite3_errmsg (db));
        return false;
    }

    sqlite3_bind_int64 (stmt, 1, noteId);
    sqlite3_bind_int64 (stmt, 2, trackId);

    const bool ok = sqlite3_step (stmt) == SQLITE_DONE;
    if (! ok) lastErr = "deleteNote step: " + juce::String (sqlite3_errmsg (db));
    sqlite3_finalize (stmt);
    return ok;
}

int64_t BeatCrateDB::addNote (int64_t trackId, const juce::String& text)
{
    const juce::ScopedLock sl (lock);
    if (! isOpen()) return 0;
    const auto trimmed = text.trim();
    if (trimmed.isEmpty()) return 0;

    int nextOrder = 1;
    {
        sqlite3_stmt* stmt = nullptr;
        if (sqlite3_prepare_v2 (db, "SELECT COALESCE(MAX(sort_order), 0) FROM track_notes WHERE track_id = ?",
                                -1, &stmt, nullptr) != SQLITE_OK)
        {
            lastErr = "addNote max prepare: " + juce::String (sqlite3_errmsg (db));
            return 0;
        }
        sqlite3_bind_int64 (stmt, 1, trackId);
        if (sqlite3_step (stmt) == SQLITE_ROW)
            nextOrder = sqlite3_column_int (stmt, 0) + 1;
        sqlite3_finalize (stmt);
    }

    sqlite3_stmt* stmt = nullptr;
    if (sqlite3_prepare_v2 (db,
            "INSERT INTO track_notes (track_id, note, sort_order) VALUES (?, ?, ?)",
            -1, &stmt, nullptr) != SQLITE_OK)
    {
        lastErr = "addNote insert prepare: " + juce::String (sqlite3_errmsg (db));
        return 0;
    }

    sqlite3_bind_int64 (stmt, 1, trackId);
    sqlite3_bind_text  (stmt, 2, trimmed.toRawUTF8(), -1, SQLITE_TRANSIENT);
    sqlite3_bind_int   (stmt, 3, nextOrder);

    const bool ok = sqlite3_step (stmt) == SQLITE_DONE;
    if (! ok) lastErr = "addNote step: " + juce::String (sqlite3_errmsg (db));
    sqlite3_finalize (stmt);

    return ok ? sqlite3_last_insert_rowid (db) : 0;
}

std::vector<Note> BeatCrateDB::listNotes (int64_t trackId)
{
    const juce::ScopedLock sl (lock);
    std::vector<Note> out;
    if (! isOpen()) return out;

    const char* sql =
        "SELECT id, note, completed, COALESCE(sort_order, 0) "
        "FROM track_notes WHERE track_id = ? "
        "ORDER BY sort_order ASC, id ASC";

    sqlite3_stmt* stmt = nullptr;
    if (sqlite3_prepare_v2 (db, sql, -1, &stmt, nullptr) != SQLITE_OK)
    {
        lastErr = "listNotes prepare: " + juce::String (sqlite3_errmsg (db));
        return out;
    }

    sqlite3_bind_int64 (stmt, 1, trackId);

    int rc;
    while ((rc = sqlite3_step (stmt)) == SQLITE_ROW)
    {
        Note n;
        n.id        = sqlite3_column_int64 (stmt, 0);
        n.text      = columnText (stmt, 1);
        n.completed = sqlite3_column_int (stmt, 2) != 0;
        n.sortOrder = sqlite3_column_int (stmt, 3);
        out.push_back (std::move (n));
    }

    if (rc != SQLITE_DONE)
    {
        lastErr = "listNotes step: " + juce::String (sqlite3_errmsg (db));
        out.clear();
    }

    sqlite3_finalize (stmt);
    return out;
}
