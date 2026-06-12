#pragma once

#include <juce_gui_basics/juce_gui_basics.h>
#include "BeatCrateDB.h"

class NoteRow : public juce::Component,
                private juce::TextEditor::Listener
{
public:
    using ToggleCallback = std::function<void (int64_t noteId, bool completed)>;
    using EditCallback   = std::function<bool (int64_t noteId, const juce::String& newText)>;
    using EditStateCallback = std::function<void (bool editing)>;
    using DeleteCallback = std::function<void (int64_t noteId)>;

    NoteRow();
    ~NoteRow() override;

    void setNote (const Note& n);
    const Note& getNote() const { return note; }

    void setShowTopDivider (bool show) { showTopDivider = show; repaint(); }

    void setOnToggle    (ToggleCallback cb) { onToggle = std::move (cb); }
    void setOnEdit      (EditCallback   cb) { onEdit   = std::move (cb); }
    void setOnEditState (EditStateCallback cb) { onEditState = std::move (cb); }
    void setOnDelete    (DeleteCallback cb) { onDelete = std::move (cb); }

    void setEditing (bool shouldEdit);
    bool isEditing() const { return editing; }

    // Display height for the current note at a given row width. In view mode the
    // note text wraps, so the row grows past kRowHeight; editing keeps kEditRowHeight.
    int preferredHeight (int width) const;

    void paint (juce::Graphics&) override;
    void resized() override;
    void mouseDown (const juce::MouseEvent&) override;
    void mouseDoubleClick (const juce::MouseEvent&) override;
    void mouseEnter (const juce::MouseEvent&) override;
    void mouseExit (const juce::MouseEvent&) override;

    static constexpr int kRowHeight    = 38;   // minimum / single-line height
    static constexpr int kEditRowHeight = 58;
    static constexpr int kTextVPad     = 8;    // top+bottom breathing room per wrapped line block

private:
    void textEditorReturnKeyPressed (juce::TextEditor&) override;
    void textEditorEscapeKeyPressed (juce::TextEditor&) override;
    void textEditorFocusLost (juce::TextEditor&) override;

    void commitEdit();
    void cancelEdit();
    void notifyEditState();

    Note note;
    bool editing       = false;
    bool hovered       = false;
    bool showTopDivider = false;
    bool restoringFocus = false;

    juce::TextEditor editor;
    juce::Label hintReturn, hintEsc;

    ToggleCallback onToggle;
    EditCallback   onEdit;
    EditStateCallback onEditState;
    DeleteCallback onDelete;
};
