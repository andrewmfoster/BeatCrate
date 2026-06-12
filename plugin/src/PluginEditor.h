#pragma once

#include <juce_gui_basics/juce_gui_basics.h>
#include "PluginProcessor.h"
#include "Theme.h"
#include "NoteRow.h"
#include "SettingsPanel.h"

class NotesPanel : public juce::Component
{
public:
    using ToggleCallback = std::function<void (int64_t noteId, bool completed)>;
    using EditCallback   = std::function<bool (int64_t noteId, const juce::String& newText)>;
    using DeleteCallback = std::function<void (int64_t noteId)>;

    NotesPanel();

    void setNotes (const std::vector<Note>& n);          // full rebuild
    void updateNotes (const std::vector<Note>& latest);  // diff-aware (polling path)
    void setOnToggle (ToggleCallback cb) { onToggle = std::move (cb); }
    void setOnEdit   (EditCallback   cb) { onEdit   = std::move (cb); }
    void setOnDelete (DeleteCallback cb) { onDelete = std::move (cb); }
    int  getContentHeight() const;
    bool isAnyRowEditing() const;

    void resized() override;

private:
    void wireRow (NoteRow& row);
    void layoutRows();

    std::vector<std::unique_ptr<NoteRow>> rows;
    std::vector<Note> lastSnapshot;
    ToggleCallback onToggle;
    EditCallback   onEdit;
    DeleteCallback onDelete;
};

class BeatCrateEditor : public juce::AudioProcessorEditor,
                        private juce::ComboBox::Listener,
                        private juce::TextEditor::Listener,
                        private juce::Button::Listener,
                        private juce::Timer
{
public:
    explicit BeatCrateEditor (BeatCrateProcessor&);
    ~BeatCrateEditor() override;

    void paint (juce::Graphics&) override;
    void resized() override;

    void comboBoxChanged (juce::ComboBox* box) override;
    void textEditorReturnKeyPressed (juce::TextEditor& ed) override;
    void buttonClicked (juce::Button* b) override;

private:
    void initialLoad();
    void populateCrates (int64_t preferCrateId, int64_t preferTrackId);
    void populateTracksForSelectedCrate (int64_t preferTrackId = 0);
    void populateNotesForSelectedTrack();
    void pollNotes();
    void refreshFromDb();        // full preserve-selection reconcile: crates -> tracks -> notes
    bool structureChanged();     // cheap check: did the crate/track id-set or names drift?
    void openDbChooser();
    void revealDbInFinder();
    void refreshStatusStrip();
    void refreshSectionCount();
    void showSettings (bool visible);

    void timerCallback() override;

    int64_t getCurrentTrackId() const;
    int64_t getSelectedCrateId() const;
    void submitNewNote();

    BeatCrateProcessor& processorRef;
    BC::LookAndFeel lf;

    // Header
    BC::Disc headerDisc;
    juce::Label wordmark;
    juce::TextButton gearBtn   { juce::String::fromUTF8 ("\xe2\x9a\x99") };
    juce::TextButton refreshBtn { "REFRESH" };

    // Status strip ("33 1/3 rpm  ·  14 crates loaded")
    juce::Label statusText;

    // Form
    juce::Label crateLabel, trackLabel;
    juce::ComboBox crateBox, trackBox;

    // Section header "SIDE B · NOTES ·····  X / Y"
    juce::Label sideLabel    { {}, "SIDE B" };
    juce::Label sectionLabel { {}, "NOTES"  };
    juce::Label sectionCount;

    // Notes list
    NotesPanel notesPanel;
    juce::Viewport notesView;

    // Empty-state widget
    BC::Disc emptyDisc { BC::Disc::Variant::Ghost };
    juce::Label emptyTitle;
    juce::Label emptyHint;

    // Add-note row
    juce::TextEditor addInput;
    juce::TextButton addButton { "PRESS" };

    // Settings overlay
    SettingsPanel settingsPanel;

    std::vector<Track> currentTracks;
    std::unique_ptr<juce::FileChooser> fileChooser;

    bool settingsOpen = false;

    JUCE_DECLARE_NON_COPYABLE_WITH_LEAK_DETECTOR (BeatCrateEditor)
};
