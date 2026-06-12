#pragma once

#include <juce_core/juce_core.h>
#include <vector>

struct sqlite3;

struct Crate
{
    int64_t id = 0;
    juce::String name;
};

struct Track
{
    int64_t id = 0;
    juce::String title;
};

struct Note
{
    int64_t id = 0;
    juce::String text;
    bool completed = false;
    int sortOrder = 0;
};

class BeatCrateDB
{
public:
    BeatCrateDB() = default;
    ~BeatCrateDB();

    BeatCrateDB (const BeatCrateDB&) = delete;
    BeatCrateDB& operator= (const BeatCrateDB&) = delete;

    bool open (const juce::File& dbFile);
    void close();
    bool isOpen() const { return db != nullptr; }

    juce::String lastError() const { return lastErr; }

    std::vector<Crate> listCrates();
    std::vector<Track> listTracks (int64_t crateId);
    std::vector<Note>  listNotes  (int64_t trackId);

    bool    updateNoteCompleted (int64_t noteId, int64_t trackId, bool completed);
    bool    updateNoteText      (int64_t noteId, int64_t trackId, const juce::String& text);
    int64_t addNote             (int64_t trackId, const juce::String& text);
    bool    deleteNote          (int64_t noteId, int64_t trackId);

    static juce::File defaultDbPath();

private:
    sqlite3* db = nullptr;
    juce::String lastErr;

    // Serializes every handle operation. The editor timer (message thread) reads via
    // list*/note CRUD while setStateInformation (a host loader thread) can close+reopen
    // the connection via open(); without this lock that overlap is a cross-thread
    // use-after-free on the sqlite3* (FULLMUTEX only guards calls on a LIVE handle, not
    // close-during-step). juce::CriticalSection is reentrant, so open()->close() is safe.
    juce::CriticalSection lock;
};
