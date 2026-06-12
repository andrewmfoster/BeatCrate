#include "NoteRow.h"
#include "Theme.h"

#include <cmath>

namespace
{
    constexpr int kSidePad = 16;
    constexpr int kCheckColumnW = 20;
    constexpr int kCheckSize = 14;
    constexpr int kGap = 12;
    constexpr int kDeleteColumnW = 20;
    constexpr int kDeleteGlyphSize = 14;

    // Width available to the note text — mirrors paint() + resized() column math
    // so measuring and drawing always agree on where wrapping happens.
    int noteTextWidth (int rowWidth)
    {
        return rowWidth - (kSidePad * 2 + kCheckColumnW + kGap + kDeleteColumnW);
    }

    // One source of truth for wrapped text: used to measure row height AND to
    // paint, so the line count can never disagree between the two.
    juce::TextLayout makeNoteLayout (const juce::String& text, float width, bool completed)
    {
        juce::AttributedString s;
        s.setText (text);
        s.setFont (BC::Font::sans (13.5f));
        s.setColour (completed ? BC::Col::fgFaint : BC::Col::fg);
        s.setJustification (juce::Justification::topLeft);
        s.setWordWrap (juce::AttributedString::byWord);

        juce::TextLayout layout;
        layout.createLayout (s, width);
        return layout;
    }
}

NoteRow::NoteRow()
{
    setInterceptsMouseClicks (true, true);

    editor.setMultiLine (false);
    editor.setReturnKeyStartsNewLine (false);
    editor.setBorder ({ 0, 0, 0, 0 });
    editor.setIndents (0, 4);
    editor.setFont (BC::Font::sans (13.5f));
    editor.setColour (juce::TextEditor::backgroundColourId,      juce::Colours::transparentBlack);
    editor.setColour (juce::TextEditor::textColourId,            BC::Col::fg);
    editor.setColour (juce::TextEditor::outlineColourId,         juce::Colours::transparentBlack);
    editor.setColour (juce::TextEditor::focusedOutlineColourId,  juce::Colours::transparentBlack);
    editor.setColour (juce::TextEditor::highlightColourId,       BC::Col::accentDim);
    editor.addListener (this);
    editor.setVisible (false);
    addChildComponent (editor);

    auto styleHint = [] (juce::Label& l, const juce::String& text)
    {
        l.setText (text, juce::dontSendNotification);
        l.setFont (BC::Font::mono (9.0f));
        l.setColour (juce::Label::textColourId, BC::Col::fgFaint);
        l.setJustificationType (juce::Justification::centredLeft);
        l.setVisible (false);
    };
    styleHint (hintReturn, juce::String::fromUTF8 ("\xe2\x86\xb5 SAVE"));
    styleHint (hintEsc,    "ESC CANCEL");
    addChildComponent (hintReturn);
    addChildComponent (hintEsc);
}

NoteRow::~NoteRow()
{
    editor.removeListener (this);
}

void NoteRow::setNote (const Note& n)
{
    note = n;
    setSize (getWidth(), editing ? kEditRowHeight : preferredHeight (getWidth()));
    repaint();
}

void NoteRow::setEditing (bool shouldEdit)
{
    if (editing == shouldEdit) return;
    editing = shouldEdit;

    editor.setVisible (editing);
    hintReturn.setVisible (editing);
    hintEsc.setVisible (editing);

    if (editing)
    {
        editor.setText (note.text, juce::dontSendNotification);
        editor.selectAll();
        editor.grabKeyboardFocus();
    }

    setSize (getWidth(), editing ? kEditRowHeight : kRowHeight);
    resized();
    repaint();
    notifyEditState();
}

void NoteRow::notifyEditState()
{
    if (onEditState) onEditState (editing);
}

void NoteRow::commitEdit()
{
    if (! editing) return;
    if (restoringFocus) return;

    const auto newText = editor.getText().trim();
    if (newText.isEmpty() || newText == note.text)
    {
        cancelEdit();
        return;
    }

    bool ok = true;
    if (onEdit) ok = onEdit (note.id, newText);

    if (ok) note.text = newText;
    editing = false;
    editor.setVisible (false);
    hintReturn.setVisible (false);
    hintEsc.setVisible (false);
    setSize (getWidth(), kRowHeight);
    resized();
    repaint();
    notifyEditState();
}

void NoteRow::cancelEdit()
{
    if (! editing) return;
    editing = false;
    editor.setVisible (false);
    hintReturn.setVisible (false);
    hintEsc.setVisible (false);
    setSize (getWidth(), kRowHeight);
    resized();
    repaint();
    notifyEditState();
}

void NoteRow::textEditorReturnKeyPressed (juce::TextEditor&) { commitEdit(); }
void NoteRow::textEditorEscapeKeyPressed (juce::TextEditor&) { cancelEdit(); }
void NoteRow::textEditorFocusLost       (juce::TextEditor&) { if (editing) commitEdit(); }

void NoteRow::mouseDown (const juce::MouseEvent& e)
{
    if (editing) return;

    // Hit-test × delete column (right edge). Active whenever the cursor is
    // here — mouseEnter precedes mouseDown so 'hovered' is already true,
    // matching the glyph's visibility.
    const int xX = getWidth() - kSidePad - kDeleteGlyphSize;
    const int xY = (kRowHeight - kDeleteGlyphSize) / 2;
    juce::Rectangle<int> xBox (xX, xY, kDeleteGlyphSize, kDeleteGlyphSize);
    if (xBox.expanded (4).contains (e.getPosition()))
    {
        if (onDelete) onDelete (note.id);
        return;
    }

    // Hit-test the check circle
    const auto checkX = kSidePad;
    const auto checkY = (kRowHeight - kCheckSize) / 2;
    juce::Rectangle<int> checkBox (checkX, checkY, kCheckSize, kCheckSize);
    if (checkBox.expanded (4).contains (e.getPosition()))
    {
        note.completed = ! note.completed;
        repaint();
        if (onToggle) onToggle (note.id, note.completed);
    }
}

void NoteRow::mouseDoubleClick (const juce::MouseEvent& e)
{
    if (editing) return;
    // Don't enter edit on the check column or the × delete column
    if (e.getPosition().getX() < kSidePad + kCheckColumnW) return;
    if (e.getPosition().getX() > getWidth() - kSidePad - kDeleteColumnW) return;
    setEditing (true);
}

void NoteRow::mouseEnter (const juce::MouseEvent&)
{
    if (hovered) return;
    hovered = true;
    repaint();
}

void NoteRow::mouseExit (const juce::MouseEvent&)
{
    if (! hovered) return;
    hovered = false;
    repaint();
}

void NoteRow::paint (juce::Graphics& g)
{
    const auto r = getLocalBounds();

    // Row background — hover / playing washes
    if (hovered)
    {
        g.setColour (BC::Col::accent.withAlpha (0.06f));
        g.fillRect (r);
    }
    if (editing)
    {
        g.setColour (BC::Col::accent.withAlpha (0.05f));
        g.fillRect (r);
    }

    // Groove divider on top edge (only when not first row)
    if (showTopDivider)
    {
        g.setColour (BC::Col::accentHair.withAlpha (BC::Col::accentHair.getFloatAlpha() * 0.5f));
        g.fillRect (kSidePad, 0, r.getWidth() - kSidePad * 2, 1);
    }

    // Round-dot check
    const float checkX = (float) kSidePad;
    const float checkY = (float) (kRowHeight - kCheckSize) / 2.0f;
    const juce::Rectangle<float> checkBox (checkX, checkY, (float) kCheckSize, (float) kCheckSize);

    const auto borderCol = note.completed ? BC::Col::accentHi
                         : hovered        ? BC::Col::accent
                         :                  BC::Col::accentLine;
    if (note.completed)
    {
        g.setColour (BC::Col::accentHi);
        g.fillEllipse (checkBox);
        // Rotated checkmark inside, dark
        juce::Path tick;
        tick.startNewSubPath (checkBox.getX() + 3.2f, checkBox.getY() + 7.2f);
        tick.lineTo          (checkBox.getX() + 6.0f, checkBox.getY() + 10.0f);
        tick.lineTo          (checkBox.getX() + 10.5f, checkBox.getY() + 4.0f);
        g.setColour (BC::Col::bg);
        g.strokePath (tick, juce::PathStrokeType (1.6f, juce::PathStrokeType::curved,
                                                          juce::PathStrokeType::rounded));
    }
    else
    {
        g.setColour (borderCol);
        g.drawEllipse (checkBox.reduced (0.5f), 1.0f);
    }

    if (editing) return; // text + hints painted by children; skip the static label

    // Note text — wraps across as many lines as the width needs; the row grows
    // to fit (see preferredHeight). Right edge reserved for × delete button so
    // the wrap point doesn't jump on hover. The text block is vertically centred
    // in the row, so a single line aligns with the check dot.
    const int textX = kSidePad + kCheckColumnW + kGap;
    const int textW = noteTextWidth (r.getWidth());
    if (textW > 0)
    {
        auto layout = makeNoteLayout (note.text, (float) textW, note.completed);
        const int textH = (int) std::ceil (layout.getHeight());
        const int textY = juce::jmax (0, (r.getHeight() - textH) / 2);
        layout.draw (g, juce::Rectangle<float> ((float) textX, (float) textY,
                                                (float) textW, (float) textH));
    }

    // × delete glyph — fades in on row hover. Same right-edge affordance as
    // the desktop's .inspector-note-del.
    if (hovered)
    {
        const int xX = r.getWidth() - kSidePad - kDeleteGlyphSize;
        const int xY = (kRowHeight - kDeleteGlyphSize) / 2;
        juce::Rectangle<int> xBox (xX, xY, kDeleteGlyphSize, kDeleteGlyphSize);
        g.setColour (BC::Col::fgFaint);
        g.setFont (BC::Font::sans (16.0f, 400.0f));
        g.drawText (juce::String::fromUTF8 ("\xc3\x97"), xBox,
                    juce::Justification::centred, false);
    }
}

int NoteRow::preferredHeight (int width) const
{
    if (editing) return kEditRowHeight;

    const int textW = noteTextWidth (width);
    if (textW <= 0 || note.text.isEmpty()) return kRowHeight;

    auto layout = makeNoteLayout (note.text, (float) textW, note.completed);
    const int textH = (int) std::ceil (layout.getHeight());
    return juce::jmax (kRowHeight, textH + 2 * kTextVPad);
}

void NoteRow::resized()
{
    const auto w = getWidth();
    const auto textX = kSidePad + kCheckColumnW + kGap;
    const auto textW = w - textX - kSidePad;

    if (editing)
    {
        // Inline edit: editor on row 1, hints below
        editor.setBounds (textX, 6, textW, 22);
        hintReturn.setBounds (textX,        32, 60, 14);
        hintEsc.setBounds    (textX + 70,   32, 80, 14);
    }
}
