#pragma once

#include <juce_gui_basics/juce_gui_basics.h>

namespace BC
{
    // Aurora — Honey palette. Mirrors tokens.css.
    namespace Col
    {
        inline const auto bg            = juce::Colour::fromString ("FF060A07");
        inline const auto bgLift        = juce::Colour::fromString ("FF0C1410");
        inline const auto topbar        = juce::Colour::fromString ("FF080604");
        inline const auto fg            = juce::Colour::fromString ("FFEDE5D3");
        inline const auto fgSoft        = fg.withAlpha (0.62f);
        inline const auto fgFaint       = fg.withAlpha (0.40f);
        inline const auto fgGhost       = fg.withAlpha (0.10f);
        inline const auto accent        = juce::Colour::fromString ("FFC8A35A");
        inline const auto accentHi      = juce::Colour::fromString ("FFE6C485");
        inline const auto accentDim     = accent.withAlpha (0.16f);
        inline const auto accentLine    = accent.withAlpha (0.32f);
        inline const auto accentHair    = accent.withAlpha (0.18f);
        inline const auto rule          = fg.withAlpha (0.10f);
        // Honey-on-honey contrast text (matches Direction C primary button)
        inline const auto onAccent      = juce::Colour::fromString ("FF1A1208");
    }

    namespace Font
    {
        juce::Font mono  (float size, float weight = 500.0f);
        juce::Font sans  (float size, float weight = 400.0f);
        juce::Font sansItalic (float size);
        juce::Font monoItalic (float size);
    }

    // LookAndFeel wraps the JUCE default but recolors + repaints the controls
    // Direction C uses (ComboBox, ScrollBar, PopupMenu, TextEditor).
    class LookAndFeel : public juce::LookAndFeel_V4
    {
    public:
        LookAndFeel();

        void drawComboBox (juce::Graphics&, int w, int h, bool isDown,
                           int buttonX, int buttonY, int buttonW, int buttonH,
                           juce::ComboBox&) override;
        juce::Font getComboBoxFont (juce::ComboBox&) override;
        void positionComboBoxText (juce::ComboBox&, juce::Label&) override;

        void drawPopupMenuBackground (juce::Graphics&, int w, int h) override;
        void drawPopupMenuItem (juce::Graphics&, const juce::Rectangle<int>& area,
                                bool isSeparator, bool isActive, bool isHighlighted,
                                bool isTicked, bool hasSubMenu,
                                const juce::String& text,
                                const juce::String& shortcutKeyText,
                                const juce::Drawable* icon,
                                const juce::Colour* textColour) override;
        juce::Font getPopupMenuFont() override;

        void drawScrollbar (juce::Graphics&, juce::ScrollBar&,
                            int x, int y, int w, int h,
                            bool vertical, int thumbStart, int thumbSize,
                            bool isMouseOver, bool isMouseDown) override;
    };

    // Procedurally painted micro-vinyl. Used at 30 (header), 18 (settings head),
    // and 56 (empty-state ghost) — the ghost variant skips the honey label and
    // uses a dashed honey-line border to match c-empty-disc in the CSS.
    class Disc : public juce::Component
    {
    public:
        enum class Variant { Solid, Ghost };
        explicit Disc (Variant v = Variant::Solid) : variant (v) {}
        void paint (juce::Graphics&) override;

    private:
        Variant variant;
    };
}
