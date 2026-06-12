#include "PluginEditor.h"

#include <algorithm>

namespace
{
    constexpr int kPad = 16;
    constexpr int kHeaderH       = 50;
    constexpr int kStatusStripH  = 26;
    constexpr int kFormGap       = 10;
    constexpr int kFieldLabelH   = 14;
    constexpr int kComboH        = 32;
    constexpr int kSectionHeadH  = 26;
    constexpr int kInputRowH     = 50;
}

// ---------- NotesPanel ----------

NotesPanel::NotesPanel() = default;

void NotesPanel::setNotes (const std::vector<Note>& notes)
{
    rows.clear();
    removeAllChildren();

    for (size_t i = 0; i < notes.size(); ++i)
    {
        auto row = std::make_unique<NoteRow>();
        row->setNote (notes[i]);
        row->setShowTopDivider (i != 0);
        wireRow (*row);
        addAndMakeVisible (*row);
        rows.push_back (std::move (row));
    }

    lastSnapshot = notes;
    layoutRows();
}

void NotesPanel::updateNotes (const std::vector<Note>& latest)
{
    // If any row is mid-edit, defer structural change to avoid blowing up the edit.
    bool anyEditing = isAnyRowEditing();

    bool structural = latest.size() != lastSnapshot.size();
    if (! structural)
    {
        for (size_t i = 0; i < latest.size(); ++i)
        {
            if (latest[i].id != lastSnapshot[i].id || latest[i].text != lastSnapshot[i].text)
            { structural = true; break; }
        }
    }

    if (structural)
    {
        if (anyEditing)
        {
            // Snapshot still updates in-place on non-editing rows; rebuild on next poll
            for (size_t i = 0; i < latest.size() && i < rows.size(); ++i)
                if (! rows[i]->isEditing())
                {
                    Note n = latest[i];
                    rows[i]->setNote (n);
                }
            return;
        }
        setNotes (latest);
        return;
    }

    for (size_t i = 0; i < latest.size(); ++i)
    {
        if (i >= rows.size()) break;
        if (rows[i]->isEditing()) continue;
        if (latest[i].completed != lastSnapshot[i].completed)
        {
            Note n = latest[i];
            rows[i]->setNote (n);
        }
    }
    lastSnapshot = latest;
}

void NotesPanel::wireRow (NoteRow& row)
{
    row.setOnToggle ([this] (int64_t id, bool completed)
    {
        // Mirror into snapshot so the next diff doesn't fight it
        for (auto& n : lastSnapshot) if (n.id == id) n.completed = completed;
        if (onToggle) onToggle (id, completed);
    });

    row.setOnEdit ([this] (int64_t id, const juce::String& newText) -> bool
    {
        const bool ok = onEdit ? onEdit (id, newText) : false;
        if (ok) for (auto& n : lastSnapshot) if (n.id == id) n.text = newText;
        return ok;
    });

    row.setOnEditState ([this] (bool) { layoutRows(); });

    row.setOnDelete ([this] (int64_t id)
    {
        // Mirror into snapshot so the next poll-diff doesn't fight the delete.
        lastSnapshot.erase (
            std::remove_if (lastSnapshot.begin(), lastSnapshot.end(),
                            [id] (const Note& n) { return n.id == id; }),
            lastSnapshot.end());
        if (onDelete) onDelete (id);
    });
}

void NotesPanel::layoutRows()
{
    const int w = getWidth();
    int y = 0;
    for (auto& r : rows)
    {
        const int h = r->preferredHeight (w);
        r->setBounds (0, y, w, h);
        y += h;
    }
    setSize (w, juce::jmax (y, 1));
}

int NotesPanel::getContentHeight() const
{
    const int w = getWidth();
    int h = 0;
    for (auto& r : rows)
        h += r->preferredHeight (w);
    return juce::jmax (h, 1);
}

bool NotesPanel::isAnyRowEditing() const
{
    for (auto& r : rows) if (r->isEditing()) return true;
    return false;
}

void NotesPanel::resized()
{
    layoutRows();
}

// ---------- BeatCrateEditor ----------

BeatCrateEditor::BeatCrateEditor (BeatCrateProcessor& p)
    : juce::AudioProcessorEditor (&p), processorRef (p)
{
    setLookAndFeel (&lf);

    const auto savedW = processorRef.getSavedWindowWidth();
    const auto savedH = processorRef.getSavedWindowHeight();
    setResizable (true, true);
    setResizeLimits (400, 480, 700, 1100);
    setSize (juce::jlimit (400, 700, savedW),
             juce::jlimit (480, 1100, savedH));

    addAndMakeVisible (headerDisc);

    wordmark.setFont (BC::Font::mono (16.0f, 500.0f));
    wordmark.setColour (juce::Label::textColourId, BC::Col::fg);
    wordmark.setJustificationType (juce::Justification::centredLeft);
    addAndMakeVisible (wordmark);

    gearBtn.setColour (juce::TextButton::buttonColourId,   juce::Colours::transparentBlack);
    gearBtn.setColour (juce::TextButton::buttonOnColourId, BC::Col::accentDim);
    gearBtn.setColour (juce::TextButton::textColourOffId,  BC::Col::fgSoft);
    gearBtn.setColour (juce::TextButton::textColourOnId,   BC::Col::accentHi);
    gearBtn.setTooltip ("Settings");
    gearBtn.addListener (this);
    addAndMakeVisible (gearBtn);

    refreshBtn.setColour (juce::TextButton::buttonColourId,   juce::Colours::transparentBlack);
    refreshBtn.setColour (juce::TextButton::buttonOnColourId, BC::Col::accentDim);
    refreshBtn.setColour (juce::TextButton::textColourOffId,  BC::Col::accent);
    refreshBtn.setColour (juce::TextButton::textColourOnId,   BC::Col::accentHi);
    refreshBtn.addListener (this);
    addAndMakeVisible (refreshBtn);

    statusText.setFont (BC::Font::mono (10.0f, 500.0f));
    statusText.setJustificationType (juce::Justification::centredLeft);
    addAndMakeVisible (statusText);

    auto styleField = [] (juce::Label& l, const juce::String& text)
    {
        l.setText (text, juce::dontSendNotification);
        l.setFont (BC::Font::mono (9.5f, 500.0f));
        l.setColour (juce::Label::textColourId, BC::Col::accent);
        l.setJustificationType (juce::Justification::centredLeft);
    };
    styleField (crateLabel, juce::String::fromUTF8 ("\xe2\x80\xa2 CRATE"));
    styleField (trackLabel, juce::String::fromUTF8 ("\xe2\x80\xa2 TRACK"));
    addAndMakeVisible (crateLabel);
    addAndMakeVisible (trackLabel);

    crateBox.addListener (this);
    trackBox.addListener (this);
    addAndMakeVisible (crateBox);
    addAndMakeVisible (trackBox);

    sideLabel.setFont (BC::Font::mono (10.0f, 500.0f));
    sideLabel.setColour (juce::Label::textColourId, BC::Col::accentHi);
    sideLabel.setJustificationType (juce::Justification::centredLeft);
    addAndMakeVisible (sideLabel);

    sectionLabel.setFont (BC::Font::mono (10.0f, 500.0f));
    sectionLabel.setColour (juce::Label::textColourId, BC::Col::fgSoft);
    sectionLabel.setJustificationType (juce::Justification::centredLeft);
    addAndMakeVisible (sectionLabel);

    sectionCount.setFont (BC::Font::mono (10.0f, 500.0f));
    sectionCount.setColour (juce::Label::textColourId, BC::Col::fgFaint);
    sectionCount.setJustificationType (juce::Justification::centredRight);
    addAndMakeVisible (sectionCount);

    notesView.setViewedComponent (&notesPanel, false);
    notesView.setScrollBarsShown (true, false);
    addAndMakeVisible (notesView);

    notesPanel.setOnToggle ([this] (int64_t noteId, bool completed)
    {
        const auto tid = getCurrentTrackId();
        if (tid <= 0) return;
        if (! processorRef.getDb().updateNoteCompleted (noteId, tid, completed))
        {
            DBG ("BeatCrate: toggle failed " << processorRef.getDb().lastError());
            populateNotesForSelectedTrack();
        }
        refreshSectionCount();
    });

    notesPanel.setOnEdit ([this] (int64_t noteId, const juce::String& newText) -> bool
    {
        const auto tid = getCurrentTrackId();
        if (tid <= 0) return false;
        const bool ok = processorRef.getDb().updateNoteText (noteId, tid, newText);
        if (! ok) DBG ("BeatCrate: updateNoteText failed " << processorRef.getDb().lastError());
        return ok;
    });

    notesPanel.setOnDelete ([this] (int64_t noteId)
    {
        const auto tid = getCurrentTrackId();
        if (tid <= 0) return;
        if (! processorRef.getDb().deleteNote (noteId, tid))
        {
            DBG ("BeatCrate: deleteNote failed " << processorRef.getDb().lastError());
            populateNotesForSelectedTrack(); // rollback to DB truth
            return;
        }
        populateNotesForSelectedTrack();
        refreshSectionCount();
    });

    // Empty-state cluster (hidden by default)
    addChildComponent (emptyDisc);
    emptyTitle.setFont (BC::Font::monoItalic (11.0f));
    emptyTitle.setColour (juce::Label::textColourId, BC::Col::fgFaint);
    emptyTitle.setJustificationType (juce::Justification::centred);
    addChildComponent (emptyTitle);

    emptyHint.setFont (BC::Font::sans (12.0f));
    emptyHint.setColour (juce::Label::textColourId, BC::Col::fgSoft);
    emptyHint.setJustificationType (juce::Justification::centredTop);
    addChildComponent (emptyHint);

    // Add-note row
    addInput.setMultiLine (false);
    addInput.setReturnKeyStartsNewLine (false);
    addInput.setTextToShowWhenEmpty (juce::String::fromUTF8 ("cut a new note\xe2\x80\xa6"),
                                     BC::Col::fgFaint);
    addInput.setFont (BC::Font::mono (12.0f));
    addInput.setColour (juce::TextEditor::backgroundColourId, juce::Colours::transparentBlack);
    addInput.setColour (juce::TextEditor::textColourId,       BC::Col::fg);
    addInput.setColour (juce::TextEditor::outlineColourId,    juce::Colours::transparentBlack);
    addInput.setColour (juce::TextEditor::focusedOutlineColourId, juce::Colours::transparentBlack);
    addInput.setBorder ({ 0, 0, 0, 0 });
    addInput.addListener (this);
    addAndMakeVisible (addInput);

    addButton.setColour (juce::TextButton::buttonColourId,   BC::Col::accentDim);
    addButton.setColour (juce::TextButton::buttonOnColourId, BC::Col::accentDim);
    addButton.setColour (juce::TextButton::textColourOffId,  BC::Col::accent);
    addButton.setColour (juce::TextButton::textColourOnId,   BC::Col::accentHi);
    addButton.addListener (this);
    addAndMakeVisible (addButton);

    // Settings overlay
    settingsPanel.setOnChoose ([this] { openDbChooser(); });
    settingsPanel.setOnReveal ([this] { revealDbInFinder(); });
    settingsPanel.setOnClose  ([this] { showSettings (false); });
    addChildComponent (settingsPanel);

    refreshStatusStrip();
    initialLoad();
    startTimer (3000);

    // If DB is missing on first load, pop the settings panel
    if (processorRef.getCrates().empty() && ! processorRef.getDbPath().existsAsFile())
        showSettings (true);
}

BeatCrateEditor::~BeatCrateEditor()
{
    stopTimer();
    crateBox.removeListener (this);
    trackBox.removeListener (this);
    addInput.removeListener (this);
    addButton.removeListener (this);
    refreshBtn.removeListener (this);
    gearBtn.removeListener (this);
    setLookAndFeel (nullptr);
}

void BeatCrateEditor::initialLoad()
{
    refreshStatusStrip();
    populateCrates (processorRef.getLastCrateId(), processorRef.getLastTrackId());
}

int64_t BeatCrateEditor::getCurrentTrackId() const
{
    const auto idx = trackBox.getSelectedId() - 1;
    if (idx < 0 || idx >= (int) currentTracks.size()) return 0;
    return currentTracks[(size_t) idx].id;
}

int64_t BeatCrateEditor::getSelectedCrateId() const
{
    const auto idx = crateBox.getSelectedId() - 1;
    const auto& crates = processorRef.getCrates();
    if (idx < 0 || idx >= (int) crates.size()) return 0;
    return crates[(size_t) idx].id;
}

void BeatCrateEditor::populateCrates (int64_t preferCrateId, int64_t preferTrackId)
{
    crateBox.clear (juce::dontSendNotification);
    const auto& crates = processorRef.getCrates();
    for (size_t i = 0; i < crates.size(); ++i)
        crateBox.addItem (crates[i].name, (int) i + 1);

    if (crates.empty())
    {
        notesPanel.setNotes ({});
        refreshSectionCount();
        resized();
        return;
    }

    int crateItemId = 1;
    if (preferCrateId > 0)
    {
        for (size_t i = 0; i < crates.size(); ++i)
            if (crates[i].id == preferCrateId) { crateItemId = (int) i + 1; break; }
    }
    crateBox.setSelectedId (crateItemId, juce::dontSendNotification);
    processorRef.setLastCrateId (crates[(size_t) (crateItemId - 1)].id);

    populateTracksForSelectedCrate (preferTrackId);
}

void BeatCrateEditor::populateTracksForSelectedCrate (int64_t preferTrackId)
{
    trackBox.clear (juce::dontSendNotification);
    currentTracks.clear();

    const auto crateId = getSelectedCrateId();
    if (crateId == 0) { refreshSectionCount(); resized(); return; }

    currentTracks = processorRef.getDb().listTracks (crateId);
    for (size_t i = 0; i < currentTracks.size(); ++i)
        trackBox.addItem (currentTracks[i].title, (int) i + 1);

    if (currentTracks.empty())
    {
        processorRef.setLastTrackId (0);
        notesPanel.setNotes ({});
        refreshSectionCount();
        resized();
        return;
    }

    int trackItemId = 1;
    if (preferTrackId > 0)
    {
        for (size_t i = 0; i < currentTracks.size(); ++i)
            if (currentTracks[i].id == preferTrackId) { trackItemId = (int) i + 1; break; }
    }
    trackBox.setSelectedId (trackItemId, juce::dontSendNotification);
    processorRef.setLastTrackId (currentTracks[(size_t) (trackItemId - 1)].id);

    populateNotesForSelectedTrack();
}

void BeatCrateEditor::populateNotesForSelectedTrack()
{
    const auto tid = getCurrentTrackId();
    if (tid <= 0)
        notesPanel.setNotes ({});
    else
        notesPanel.setNotes (processorRef.getDb().listNotes (tid));

    notesPanel.setSize (notesView.getWidth(), notesPanel.getContentHeight());
    refreshSectionCount();
    resized();
}

void BeatCrateEditor::pollNotes()
{
    const auto tid = getCurrentTrackId();
    if (tid <= 0) return;

    const auto vp = notesView.getViewPosition();
    notesPanel.updateNotes (processorRef.getDb().listNotes (tid));
    notesPanel.setSize (notesView.getWidth(), notesPanel.getContentHeight());
    notesView.setViewPosition (vp);
    refreshSectionCount();
}

bool BeatCrateEditor::structureChanged()
{
    auto& db = processorRef.getDb();
    if (! db.isOpen()) return false;

    // Crate set drifted (added/removed/renamed)?
    const auto freshCrates = db.listCrates();
    const auto& curCrates  = processorRef.getCrates();
    if (freshCrates.size() != curCrates.size()) return true;
    for (size_t i = 0; i < freshCrates.size(); ++i)
        if (freshCrates[i].id != curCrates[i].id || freshCrates[i].name != curCrates[i].name)
            return true;

    // Track set of the selected crate drifted?
    const auto crateId = getSelectedCrateId();
    if (crateId > 0)
    {
        const auto freshTracks = db.listTracks (crateId);
        if (freshTracks.size() != currentTracks.size()) return true;
        for (size_t i = 0; i < freshTracks.size(); ++i)
            if (freshTracks[i].id != currentTracks[i].id
                || freshTracks[i].title != currentTracks[i].title)
                return true;
    }

    return false;
}

void BeatCrateEditor::refreshFromDb()
{
    // Capture the live selection by id BEFORE reloading (indices may shift).
    const auto sc = getSelectedCrateId();
    const auto st = getCurrentTrackId();
    const auto preferCrateId = sc > 0 ? sc : processorRef.getLastCrateId();
    const auto preferTrackId = st > 0 ? st : processorRef.getLastTrackId();

    processorRef.reloadCrates();
    refreshStatusStrip();
    populateCrates (preferCrateId, preferTrackId); // remaps by id; falls back to first if gone
}

void BeatCrateEditor::timerCallback()
{
    // Don't blow up an in-progress edit with a structural rebuild; the note
    // diff-poll already tolerates editing rows.
    if (! notesPanel.isAnyRowEditing() && structureChanged())
        refreshFromDb();
    else
        pollNotes();
}

void BeatCrateEditor::submitNewNote()
{
    const auto tid = getCurrentTrackId();
    if (tid <= 0) return;
    if (addInput.getText().trim().isEmpty()) return;

    if (processorRef.getDb().addNote (tid, addInput.getText()) > 0)
    {
        addInput.setText ({}, juce::dontSendNotification);
        populateNotesForSelectedTrack();
        notesView.setViewPositionProportionately (0.0, 1.0);
    }
    else
    {
        DBG ("BeatCrate: addNote failed " << processorRef.getDb().lastError());
    }
}

void BeatCrateEditor::openDbChooser()
{
    auto startDir = processorRef.getDbPath();
    if (! startDir.existsAsFile())
        startDir = BeatCrateDB::defaultDbPath();
    if (! startDir.existsAsFile())
        startDir = juce::File::getSpecialLocation (juce::File::userHomeDirectory);

    fileChooser = std::make_unique<juce::FileChooser> (
        "Locate beatcrate.db",
        startDir.existsAsFile() ? startDir.getParentDirectory() : startDir,
        "*.db");

    fileChooser->launchAsync (
        juce::FileBrowserComponent::openMode | juce::FileBrowserComponent::canSelectFiles,
        [this] (const juce::FileChooser& fc)
        {
            const auto picked = fc.getResult();
            if (! picked.existsAsFile()) return;

            processorRef.setDbPath (picked);
            processorRef.setLastCrateId (0);
            processorRef.setLastTrackId (0);
            initialLoad();
            showSettings (false);
        });
}

void BeatCrateEditor::revealDbInFinder()
{
    const auto path = processorRef.getDbPath();
    if (path.existsAsFile())
        path.revealToUser();
}

void BeatCrateEditor::refreshStatusStrip()
{
    const auto& crates = processorRef.getCrates();
    const bool dbOk = ! crates.empty() && processorRef.getDbPath().existsAsFile();

    juce::AttributedString s;
    auto dot = juce::String::fromUTF8 ("\xe2\x97\x8f "); // small filled dot
    auto sep = juce::String::fromUTF8 ("  \xc2\xb7  ");
    const auto mono = BC::Font::mono (10.0f, 500.0f);

    s.append (dot, mono, BC::Col::accent);
    if (dbOk)
    {
        s.append (juce::String::fromUTF8 ("33\xe2\x85\x93 RPM"), mono, BC::Col::fgSoft);
        s.append (sep, mono, BC::Col::fgGhost);
        s.append (juce::String (crates.size()) + " CRATES LOADED", mono, BC::Col::fgFaint);
    }
    else
    {
        s.append ("NO RECORD", mono, BC::Col::fgSoft);
        s.append (sep, mono, BC::Col::fgGhost);
        s.append ("LOCATE BEATCRATE.DB", mono, BC::Col::fgFaint);
    }

    statusText.setText ({}, juce::dontSendNotification);
    statusText.setColour (juce::Label::textColourId, BC::Col::fgFaint);

    // JUCE Label only supports plain text, so we render the attributed line via paint
    // through a small helper child. Simpler: collapse to a plain string with the same content.
    juce::String plain;
    if (dbOk)  plain << juce::String::fromUTF8 ("\xe2\x97\x8f 33\xe2\x85\x93 RPM  \xc2\xb7  ")
                     << crates.size() << " CRATES LOADED";
    else       plain << juce::String::fromUTF8 ("\xe2\x97\x8f NO RECORD  \xc2\xb7  LOCATE BEATCRATE.DB");
    statusText.setText (plain, juce::dontSendNotification);

    // Sync settings overlay
    settingsPanel.setDbPath (processorRef.getDbPath().getFullPathName(), ! dbOk);
}

void BeatCrateEditor::refreshSectionCount()
{
    int done = 0, total = 0;
    const auto& snap = const_cast<NotesPanel&> (notesPanel);
    juce::ignoreUnused (snap);

    // Pull from current track's notes via DB? Simpler: read snapshot via re-querying current.
    const auto tid = getCurrentTrackId();
    const auto& crates = processorRef.getCrates();
    const bool dbOk = ! crates.empty() && processorRef.getDbPath().existsAsFile();
    if (tid > 0 && dbOk)
    {
        const auto notes = processorRef.getDb().listNotes (tid);
        total = (int) notes.size();
        for (const auto& n : notes) if (n.completed) ++done;
    }

    // Update empty state visibility
    const bool noCrates  = crates.empty();
    const bool noTracks  = currentTracks.empty();
    const bool noNotes   = (tid > 0 && total == 0);
    const bool showEmpty = noCrates || noTracks || noNotes;

    emptyDisc.setVisible (showEmpty);
    emptyTitle.setVisible (showEmpty);
    emptyHint.setVisible  (showEmpty);
    notesView.setVisible (! showEmpty);

    if (showEmpty)
    {
        if (noCrates)
        {
            emptyTitle.setText ("no record loaded", juce::dontSendNotification);
            emptyHint.setText  ("Drop a needle on the right database. "
                                "Open settings (\xe2\x9a\x99) to locate beatcrate.db.",
                                juce::dontSendNotification);
        }
        else if (noTracks)
        {
            emptyTitle.setText ("no tracks pressed", juce::dontSendNotification);
            emptyHint.setText  ("This crate is empty. Add tracks in the desktop app, then refresh.",
                                juce::dontSendNotification);
        }
        else
        {
            emptyTitle.setText ("no grooves cut yet", juce::dontSendNotification);
            emptyHint.setText  ("Drop a needle and write a note. They sync back to the crate.",
                                juce::dontSendNotification);
        }
        sectionCount.setText (noTracks ? juce::String::fromUTF8 ("\xe2\x80\x94") : "0", juce::dontSendNotification);
    }
    else
    {
        sectionCount.setText (juce::String (done) + " / " + juce::String (total),
                              juce::dontSendNotification);
    }
}

void BeatCrateEditor::showSettings (bool visible)
{
    settingsOpen = visible;
    settingsPanel.setVisible (visible);
    settingsPanel.toFront (false);
    if (visible)
        settingsPanel.setDbPath (processorRef.getDbPath().getFullPathName(),
                                ! processorRef.getDbPath().existsAsFile()
                                || processorRef.getCrates().empty());
    resized();
}

void BeatCrateEditor::comboBoxChanged (juce::ComboBox* box)
{
    if (box == &crateBox)
    {
        const auto crateId = getSelectedCrateId();
        processorRef.setLastCrateId (crateId);
        processorRef.setLastTrackId (0);
        populateTracksForSelectedCrate();
    }
    else if (box == &trackBox)
    {
        processorRef.setLastTrackId (getCurrentTrackId());
        populateNotesForSelectedTrack();
    }
}

void BeatCrateEditor::textEditorReturnKeyPressed (juce::TextEditor& ed)
{
    if (&ed == &addInput) submitNewNote();
}

void BeatCrateEditor::buttonClicked (juce::Button* b)
{
    if      (b == &addButton)  submitNewNote();
    else if (b == &refreshBtn) refreshFromDb();
    else if (b == &gearBtn)    showSettings (! settingsOpen);
}

void BeatCrateEditor::paint (juce::Graphics& g)
{
    // Background — bg with soft honey wash at the top, mirroring .dirC background
    g.fillAll (BC::Col::bg);
    juce::ColourGradient wash (BC::Col::accent.withAlpha (0.06f),
                               (float) getWidth() * 0.5f, 0.0f,
                               juce::Colours::transparentBlack,
                               (float) getWidth() * 0.5f, (float) getHeight() * 0.35f,
                               false);
    g.setGradientFill (wash);
    g.fillRect (getLocalBounds());

    // Header groove line
    auto drawFadingHairline = [&] (float y)
    {
        const float x1 = (float) kPad;
        const float x2 = (float) (getWidth() - kPad);
        juce::ColourGradient line (juce::Colours::transparentBlack, x1, y,
                                   juce::Colours::transparentBlack, x2, y, false);
        line.addColour (0.12, BC::Col::accentHair);
        line.addColour (0.88, BC::Col::accentHair);
        g.setGradientFill (line);
        g.fillRect (x1, y, x2 - x1, 1.0f);
    };

    drawFadingHairline ((float) kHeaderH);

    // Hairline above the add-note input row
    const float inputTop = (float) (getHeight() - kInputRowH);
    drawFadingHairline (inputTop);

    // Section header fading rule (between section label and count)
    const auto secBounds = sectionLabel.getBounds();
    const float ruleY = (float) secBounds.getCentreY();
    const float ruleX1 = (float) (sectionLabel.getRight() + 8);
    const float ruleX2 = (float) (sectionCount.getX() - 8);
    if (ruleX2 > ruleX1)
    {
        juce::ColourGradient rule (BC::Col::accentHair, ruleX1, ruleY,
                                   juce::Colours::transparentBlack, ruleX2, ruleY, false);
        g.setGradientFill (rule);
        g.fillRect (ruleX1, ruleY, ruleX2 - ruleX1, 1.0f);
    }

    // JUCE Label only paints one color, so paint the bicolor wordmark directly.
    const auto wmFont = BC::Font::mono (16.0f, 500.0f);
    g.setFont (wmFont);
    g.setColour (BC::Col::fg);
    const auto wm = wordmark.getBounds();
    g.drawText ("Beat", wm, juce::Justification::centredLeft, false);
    juce::GlyphArrangement ga;
    ga.addLineOfText (wmFont, "Beat", 0.0f, 0.0f);
    const auto beatW = ga.getBoundingBox (0, -1, true).getWidth();
    g.setColour (BC::Col::accentHi);
    g.drawText ("Crate",
                juce::Rectangle<int> (wm.getX() + (int) beatW, wm.getY(), wm.getWidth(), wm.getHeight()),
                juce::Justification::centredLeft, false);

    // Honey needle dot to the left of the add-note input
    const float needleX = (float) (kPad + 5);
    const float needleY = inputTop + (kInputRowH * 0.5f);
    g.setColour (BC::Col::accent.withAlpha (0.12f));
    g.fillEllipse (needleX - 7.0f, needleY - 7.0f, 14.0f, 14.0f);
    g.setColour (BC::Col::accent);
    g.fillEllipse (needleX - 4.0f, needleY - 4.0f, 8.0f, 8.0f);

    // Status strip dot
    const float dotX = (float) (kPad + 3);
    const float dotY = (float) (kHeaderH + kStatusStripH * 0.5f);
    g.setColour (BC::Col::accent.withAlpha (0.50f));
    g.fillEllipse (dotX - 5.0f, dotY - 5.0f, 10.0f, 10.0f);
    g.setColour (BC::Col::accent);
    g.fillEllipse (dotX - 2.5f, dotY - 2.5f, 5.0f, 5.0f);
}

void BeatCrateEditor::resized()
{
    // Persist window size
    processorRef.setSavedWindowSize (getWidth(), getHeight());

    auto bounds = getLocalBounds();

    // Header (50px tall)
    auto header = bounds.removeFromTop (kHeaderH);
    header.removeFromLeft (kPad);
    header.removeFromRight (kPad);
    header.removeFromBottom (10);
    header.removeFromTop (14);

    headerDisc.setBounds (header.removeFromLeft (30).withSizeKeepingCentre (30, 30));
    header.removeFromLeft (12);
    refreshBtn.setBounds (header.removeFromRight (78));
    header.removeFromRight (6);
    gearBtn.setBounds (header.removeFromRight (28));
    header.removeFromRight (6);
    wordmark.setBounds (header);

    // Status strip
    auto strip = bounds.removeFromTop (kStatusStripH);
    strip.removeFromLeft (kPad + 14); // leave room for the painted dot
    strip.removeFromRight (kPad);
    statusText.setBounds (strip);

    bounds.removeFromTop (4);

    // Form
    auto form = bounds.removeFromTop (kFieldLabelH * 2 + kComboH * 2 + kFormGap * 2 + 12);
    form.removeFromLeft (kPad);
    form.removeFromRight (kPad);

    crateLabel.setBounds (form.removeFromTop (kFieldLabelH));
    form.removeFromTop (4);
    crateBox.setBounds   (form.removeFromTop (kComboH));
    form.removeFromTop (kFormGap);

    trackLabel.setBounds (form.removeFromTop (kFieldLabelH));
    form.removeFromTop (4);
    trackBox.setBounds   (form.removeFromTop (kComboH));

    // Section header
    auto sec = bounds.removeFromTop (kSectionHeadH);
    sec.removeFromLeft (kPad);
    sec.removeFromRight (kPad);
    sideLabel.setBounds (sec.removeFromLeft (60));
    sec.removeFromLeft (6);
    sectionLabel.setBounds (sec.removeFromLeft (54));
    sectionCount.setBounds (sec.removeFromRight (70));

    // Add-note row (50px high)
    auto inputRow = bounds.removeFromBottom (kInputRowH);
    inputRow.removeFromLeft (kPad + 18); // leave room for painted needle
    inputRow.removeFromRight (kPad);
    addButton.setBounds (inputRow.removeFromRight (74).withSizeKeepingCentre (74, 30));
    inputRow.removeFromRight (10);
    addInput.setBounds (inputRow.withSizeKeepingCentre (inputRow.getWidth(), 26));

    bounds.removeFromBottom (4);

    // Notes list / empty state
    notesView.setBounds (bounds);
    notesPanel.setSize (notesView.getWidth(), notesPanel.getContentHeight());

    const auto emptyBounds = bounds;
    emptyDisc.setBounds (emptyBounds.withSizeKeepingCentre (56, 56)
                                    .translated (0, -emptyBounds.getHeight() / 6));
    emptyTitle.setBounds (emptyBounds.withSizeKeepingCentre (260, 20)
                                     .translated (0, 24));
    emptyHint.setBounds  (emptyBounds.withSizeKeepingCentre (260, 60)
                                     .translated (0, 70));

    settingsPanel.setBounds (getLocalBounds());
}
