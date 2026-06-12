#include "Theme.h"

namespace BC
{
    namespace Font
    {
        juce::Font mono (float size, float weight)
        {
            const auto style = weight >= 700.0f ? "Bold"
                             : weight >= 500.0f ? "Medium"
                             :                    "Regular";
            return juce::Font (juce::FontOptions ("JetBrains Mono", size, juce::Font::plain).withStyle (style));
        }

        juce::Font sans (float size, float weight)
        {
            const auto style = weight >= 700.0f ? "Bold"
                             : weight >= 500.0f ? "Medium"
                             :                    "Regular";
            return juce::Font (juce::FontOptions ("Archivo", size, juce::Font::plain).withStyle (style));
        }

        juce::Font sansItalic (float size)
        {
            return juce::Font (juce::FontOptions ("Archivo", size, juce::Font::italic));
        }

        juce::Font monoItalic (float size)
        {
            return juce::Font (juce::FontOptions ("JetBrains Mono", size, juce::Font::italic));
        }
    }

    LookAndFeel::LookAndFeel()
    {
        // ComboBox colors — bgLift surface, rule border, honey accents
        setColour (juce::ComboBox::backgroundColourId,     Col::bgLift);
        setColour (juce::ComboBox::textColourId,           Col::fg);
        setColour (juce::ComboBox::outlineColourId,        Col::rule);
        setColour (juce::ComboBox::arrowColourId,          Col::accent);
        setColour (juce::ComboBox::focusedOutlineColourId, Col::accent);

        // PopupMenu (dropdown list)
        setColour (juce::PopupMenu::backgroundColourId,           Col::bgLift);
        setColour (juce::PopupMenu::textColourId,                 Col::fg);
        setColour (juce::PopupMenu::highlightedBackgroundColourId, Col::accentDim);
        setColour (juce::PopupMenu::highlightedTextColourId,      Col::accentHi);

        // TextEditor (inline-edit + add-note input)
        setColour (juce::TextEditor::backgroundColourId,      juce::Colours::transparentBlack);
        setColour (juce::TextEditor::textColourId,            Col::fg);
        setColour (juce::TextEditor::outlineColourId,         juce::Colours::transparentBlack);
        setColour (juce::TextEditor::focusedOutlineColourId,  juce::Colours::transparentBlack);
        setColour (juce::TextEditor::highlightColourId,       Col::accentDim);
        setColour (juce::TextEditor::highlightedTextColourId, Col::accentHi);
        setColour (juce::CaretComponent::caretColourId,       Col::accentHi);

        // Scrollbar thumb — honey hairline, no track
        setColour (juce::ScrollBar::backgroundColourId, juce::Colours::transparentBlack);
        setColour (juce::ScrollBar::thumbColourId,      Col::accentHair);
    }

    void LookAndFeel::drawComboBox (juce::Graphics& g, int w, int h, bool /*isDown*/,
                                    int /*buttonX*/, int /*buttonY*/, int /*buttonW*/, int /*buttonH*/,
                                    juce::ComboBox& box)
    {
        const float radius = 8.0f;
        const auto bounds  = juce::Rectangle<float> (0.0f, 0.0f, (float) w, (float) h);

        g.setColour (Col::bgLift);
        g.fillRoundedRectangle (bounds, radius);

        const auto borderCol = box.hasKeyboardFocus (true) ? Col::accent
                             : box.isMouseOver (true)      ? Col::accentLine
                             :                               Col::rule;
        g.setColour (borderCol);
        g.drawRoundedRectangle (bounds.reduced (0.5f), radius, 1.0f);

        // Honey caret on the right — chevron pointing down
        const float caretX = (float) w - 16.0f;
        const float caretY = (float) h * 0.5f;
        juce::Path caret;
        caret.startNewSubPath (caretX - 4.0f, caretY - 2.0f);
        caret.lineTo (caretX,         caretY + 2.5f);
        caret.lineTo (caretX + 4.0f,  caretY - 2.0f);
        g.setColour (Col::accent);
        g.strokePath (caret, juce::PathStrokeType (1.4f, juce::PathStrokeType::curved,
                                                          juce::PathStrokeType::rounded));
    }

    juce::Font LookAndFeel::getComboBoxFont (juce::ComboBox&)
    {
        return Font::sans (13.0f);
    }

    void LookAndFeel::positionComboBoxText (juce::ComboBox& box, juce::Label& label)
    {
        label.setBounds (14, 0, box.getWidth() - 32, box.getHeight());
        label.setFont (Font::sans (13.0f));
    }

    void LookAndFeel::drawPopupMenuBackground (juce::Graphics& g, int w, int h)
    {
        g.fillAll (Col::bgLift);
        g.setColour (Col::accentHair);
        g.drawRect (0, 0, w, h, 1);
    }

    juce::Font LookAndFeel::getPopupMenuFont()
    {
        return Font::sans (13.0f);
    }

    void LookAndFeel::drawPopupMenuItem (juce::Graphics& g, const juce::Rectangle<int>& area,
                                         bool isSeparator, bool /*isActive*/, bool isHighlighted,
                                         bool isTicked, bool /*hasSubMenu*/,
                                         const juce::String& text,
                                         const juce::String& /*shortcutKeyText*/,
                                         const juce::Drawable* /*icon*/,
                                         const juce::Colour* /*textColour*/)
    {
        if (isSeparator)
        {
            g.setColour (Col::rule);
            g.fillRect (area.reduced (8, 0).withHeight (1).withY (area.getCentreY()));
            return;
        }

        if (isHighlighted)
        {
            g.setColour (Col::accentDim);
            g.fillRect (area);
        }

        g.setColour (isHighlighted || isTicked ? Col::accentHi : Col::fg);
        g.setFont   (Font::sans (13.0f));
        g.drawText  (text, area.reduced (12, 0), juce::Justification::centredLeft, true);
    }

    void LookAndFeel::drawScrollbar (juce::Graphics& g, juce::ScrollBar&,
                                     int x, int y, int w, int h,
                                     bool vertical, int thumbStart, int thumbSize,
                                     bool isMouseOver, bool /*isMouseDown*/)
    {
        if (thumbSize <= 0) return;

        const auto track = vertical ? juce::Rectangle<int> (x + w/2 - 1, y, 2, h).toFloat()
                                    : juce::Rectangle<int> (x, y + h/2 - 1, w, 2).toFloat();
        juce::ignoreUnused (track);

        const auto thumb = vertical
            ? juce::Rectangle<float> ((float) x + (float) (w - 4) * 0.5f, (float) thumbStart, 4.0f, (float) thumbSize)
            : juce::Rectangle<float> ((float) thumbStart, (float) y + (float) (h - 4) * 0.5f, (float) thumbSize, 4.0f);

        g.setColour (isMouseOver ? Col::accentLine : Col::accentHair);
        g.fillRoundedRectangle (thumb, 2.0f);
    }

    // ---- Disc ----

    void Disc::paint (juce::Graphics& g)
    {
        const auto r = getLocalBounds().toFloat();
        const float diameter = std::min (r.getWidth(), r.getHeight());
        const auto disc = juce::Rectangle<float> (diameter, diameter).withCentre (r.getCentre());
        const float cx = disc.getCentreX();
        const float cy = disc.getCentreY();
        const float radius = diameter * 0.5f;

        if (variant == Variant::Ghost)
        {
            // Dashed honey-line outline + ghost concentric grooves + tiny center dot.
            // Matches .c-empty-disc.
            const float dashLengths[] = { 3.0f, 3.0f };
            juce::Path outline;
            outline.addEllipse (disc.reduced (0.5f));
            juce::Path dashed;
            juce::PathStrokeType (1.0f).createDashedStroke (dashed, outline, dashLengths, 2);
            g.setColour (Col::accentLine);
            g.fillPath (dashed);

            g.setColour (Col::fg.withAlpha (0.05f));
            for (float gr = radius - 3.0f; gr > radius * 0.40f; gr -= 3.0f)
                g.drawEllipse (cx - gr, cy - gr, gr * 2.0f, gr * 2.0f, 1.0f);

            const float centerDot = diameter * 0.24f;
            g.setColour (Col::accentLine);
            g.fillEllipse (cx - centerDot * 0.5f, cy - centerDot * 0.5f, centerDot, centerDot);
            return;
        }

        // Solid variant — base disc with concentric groove rings.
        g.setColour (juce::Colour::fromString ("FF0C0906"));
        g.fillEllipse (disc);

        // Repeating ring pattern, 1.2px stride alternating colors — analog of
        // repeating-radial-gradient(circle, #1a1410 0 1.2px, #0c0906 1.2px 2.2px).
        const float stride = juce::jmax (1.1f, diameter * 0.04f);
        const float minR   = diameter * 0.36f; // stop short of where the honey label sits
        const auto ringHi  = juce::Colour::fromString ("FF1A1410");
        for (float ringR = radius - stride * 0.5f; ringR > minR; ringR -= stride)
        {
            g.setColour (ringHi);
            g.drawEllipse (cx - ringR, cy - ringR, ringR * 2.0f, ringR * 2.0f, 0.6f);
        }

        // Subtle honey rim
        g.setColour (Col::accent.withAlpha (0.35f));
        g.drawEllipse (disc.reduced (0.5f), 0.6f);

        // Honey label — radial gradient honey-hi -> honey -> dark-honey
        const float labelInset = diameter * 0.32f;
        const auto label = disc.reduced (labelInset);
        juce::ColourGradient grad (Col::accentHi,
                                   label.getX() + label.getWidth() * 0.30f,
                                   label.getY() + label.getHeight() * 0.30f,
                                   juce::Colour::fromString ("FF8A6E3A"),
                                   label.getRight(),
                                   label.getBottom(),
                                   true);
        grad.addColour (0.6, Col::accent);
        g.setGradientFill (grad);
        g.fillEllipse (label);

        g.setColour (juce::Colours::black.withAlpha (0.40f));
        g.drawEllipse (label.reduced (0.25f), 0.5f);

        // Center hole
        const float hole = juce::jmax (2.0f, diameter * 0.08f);
        g.setColour (Col::bg);
        g.fillEllipse (cx - hole * 0.5f, cy - hole * 0.5f, hole, hole);
    }
}
