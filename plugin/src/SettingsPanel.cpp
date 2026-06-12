#include "SettingsPanel.h"

namespace
{
    constexpr int kPad = 16;
}

SettingsPanel::SettingsPanel()
{
    addAndMakeVisible (miniDisc);

    title.setFont (BC::Font::mono (11.0f, 500.0f));
    title.setColour (juce::Label::textColourId, BC::Col::accentHi);
    title.setJustificationType (juce::Justification::centredLeft);
    addAndMakeVisible (title);

    closeBtn.setColour (juce::TextButton::buttonColourId,   juce::Colours::transparentBlack);
    closeBtn.setColour (juce::TextButton::buttonOnColourId, BC::Col::accentDim);
    closeBtn.setColour (juce::TextButton::textColourOffId,  BC::Col::fgSoft);
    closeBtn.setColour (juce::TextButton::textColourOnId,   BC::Col::accentHi);
    closeBtn.addListener (this);
    addAndMakeVisible (closeBtn);

    dbLabel.setFont (BC::Font::mono (9.5f, 500.0f));
    dbLabel.setColour (juce::Label::textColourId, BC::Col::accent);
    addAndMakeVisible (dbLabel);

    pathBox.setFont (BC::Font::mono (11.0f));
    pathBox.setJustificationType (juce::Justification::centredLeft);
    pathBox.setBorderSize ({ 0, 14, 0, 14 });
    addAndMakeVisible (pathBox);

    helper.setFont (BC::Font::sans (12.0f));
    helper.setColour (juce::Label::textColourId, BC::Col::fgSoft);
    helper.setJustificationType (juce::Justification::topLeft);
    addAndMakeVisible (helper);

    auto styleBtn = [] (juce::TextButton& b, bool primary)
    {
        b.setColour (juce::TextButton::buttonColourId,
                     primary ? BC::Col::accent : juce::Colours::transparentBlack);
        b.setColour (juce::TextButton::buttonOnColourId, BC::Col::accentDim);
        b.setColour (juce::TextButton::textColourOffId,
                     primary ? BC::Col::onAccent : BC::Col::accent);
        b.setColour (juce::TextButton::textColourOnId,  BC::Col::accentHi);
    };
    styleBtn (changeBtn, false);
    styleBtn (revealBtn, false);
    changeBtn.addListener (this);
    revealBtn.addListener (this);
    addAndMakeVisible (changeBtn);
    addAndMakeVisible (revealBtn);

    futureLabel.setFont (BC::Font::mono (10.0f));
    futureLabel.setColour (juce::Label::textColourId, BC::Col::fgFaint);
    futureLabel.setJustificationType (juce::Justification::centredLeft);
    addAndMakeVisible (futureLabel);
}

SettingsPanel::~SettingsPanel()
{
    closeBtn.removeListener (this);
    changeBtn.removeListener (this);
    revealBtn.removeListener (this);
}

void SettingsPanel::setDbPath (const juce::String& path, bool missing)
{
    dbMissing = missing;

    if (missing)
    {
        pathBox.setText (juce::String::fromUTF8 ("\xe2\x80\x94 beatcrate.db not located \xe2\x80\x94"),
                        juce::dontSendNotification);
        pathBox.setColour (juce::Label::textColourId, BC::Col::accentHi);
        helper.setText ("Point this at the beatcrate.db your desktop app writes to "
                        "\xe2\x80\x94 usually in ~/Library/Application Support/BeatCrate.",
                        juce::dontSendNotification);
        changeBtn.setButtonText (juce::String::fromUTF8 ("LOCATE BEATCRATE.DB\xe2\x80\xa6"));
    }
    else
    {
        pathBox.setText (path, juce::dontSendNotification);
        pathBox.setColour (juce::Label::textColourId, BC::Col::fgSoft);
        helper.setText ("The plugin reads notes from this database. "
                        "Same file your desktop app uses.",
                        juce::dontSendNotification);
        changeBtn.setButtonText (juce::String::fromUTF8 ("CHANGE\xe2\x80\xa6"));
    }

    revealBtn.setVisible (! missing);

    // Restyle change button: primary when missing, ghost otherwise
    if (missing)
    {
        changeBtn.setColour (juce::TextButton::buttonColourId,   BC::Col::accent);
        changeBtn.setColour (juce::TextButton::textColourOffId,  BC::Col::onAccent);
    }
    else
    {
        changeBtn.setColour (juce::TextButton::buttonColourId,   juce::Colours::transparentBlack);
        changeBtn.setColour (juce::TextButton::textColourOffId,  BC::Col::accent);
    }

    resized();
    repaint();
}

void SettingsPanel::buttonClicked (juce::Button* b)
{
    if      (b == &closeBtn  && onClose)  onClose();
    else if (b == &changeBtn && onChoose) onChoose();
    else if (b == &revealBtn && onReveal) onReveal();
}

void SettingsPanel::paint (juce::Graphics& g)
{
    const auto bounds = getLocalBounds().toFloat();

    // Background — bg with a soft honey wash at top (matches CSS radial-gradient)
    g.fillAll (BC::Col::bg);
    juce::ColourGradient wash (BC::Col::accent.withAlpha (0.06f),
                               bounds.getCentreX(), 0.0f,
                               juce::Colours::transparentBlack,
                               bounds.getCentreX(), bounds.getHeight() * 0.5f,
                               false);
    g.setGradientFill (wash);
    g.fillRect (bounds);

    // Header groove line
    const float lineY = 50.0f;
    juce::ColourGradient line (juce::Colours::transparentBlack, (float) kPad, lineY,
                               juce::Colours::transparentBlack, (float) (getWidth() - kPad), lineY,
                               false);
    line.addColour (0.12, BC::Col::accentHair);
    line.addColour (0.88, BC::Col::accentHair);
    g.setGradientFill (line);
    g.fillRect ((float) kPad, lineY, (float) (getWidth() - kPad * 2), 1.0f);

    // Path box surface
    const auto pathR = pathBox.getBounds().toFloat();
    if (! pathR.isEmpty())
    {
        if (dbMissing)
        {
            g.setColour (BC::Col::accent.withAlpha (0.05f));
            g.fillRoundedRectangle (pathR, 8.0f);
            g.setColour (BC::Col::accentLine);
            g.drawRoundedRectangle (pathR.reduced (0.5f), 8.0f, 1.0f);
        }
        else
        {
            g.setColour (BC::Col::bgLift);
            g.fillRoundedRectangle (pathR, 8.0f);
            g.setColour (BC::Col::rule);
            g.drawRoundedRectangle (pathR.reduced (0.5f), 8.0f, 1.0f);
        }
    }

    // Button outlines (ghost variant)
    auto drawBtnOutline = [&] (juce::Component& c, bool primary)
    {
        if (! c.isVisible()) return;
        const auto r = c.getBounds().toFloat();
        if (! primary)
        {
            g.setColour (BC::Col::accentLine);
            g.drawRoundedRectangle (r.reduced (0.5f), r.getHeight() * 0.5f, 1.0f);
        }
        else
        {
            g.setColour (BC::Col::accent);
            g.fillRoundedRectangle (r, r.getHeight() * 0.5f);
        }
    };
    drawBtnOutline (changeBtn, dbMissing);
    drawBtnOutline (revealBtn, false);

    // Future-section top hairline
    if (futureLabel.isVisible())
    {
        const auto fr = futureLabel.getBounds();
        g.setColour (BC::Col::fgGhost);
        g.fillRect (fr.getX(), fr.getY() - 12, fr.getWidth(), 1);
    }
}

void SettingsPanel::resized()
{
    auto bounds = getLocalBounds();

    auto head = bounds.removeFromTop (50);
    head.removeFromLeft (kPad);
    head.removeFromRight (kPad);

    miniDisc.setBounds (head.removeFromLeft (18).withSizeKeepingCentre (18, 18));
    head.removeFromLeft (10);
    closeBtn.setBounds (head.removeFromRight (28).withSizeKeepingCentre (28, 28));
    title.setBounds (head);

    bounds.removeFromTop (16);
    bounds.removeFromLeft (kPad);
    bounds.removeFromRight (kPad);

    dbLabel.setBounds (bounds.removeFromTop (16));
    bounds.removeFromTop (8);
    pathBox.setBounds (bounds.removeFromTop (36));
    bounds.removeFromTop (10);

    const int helperH = juce::jmin (60, bounds.getHeight() - 80);
    helper.setBounds (bounds.removeFromTop (juce::jmax (40, helperH)));
    bounds.removeFromTop (10);

    auto btnRow = bounds.removeFromTop (34);
    if (revealBtn.isVisible())
    {
        changeBtn.setBounds (btnRow.removeFromLeft ((btnRow.getWidth() - 8) / 2));
        btnRow.removeFromLeft (8);
        revealBtn.setBounds (btnRow);
    }
    else
    {
        changeBtn.setBounds (btnRow);
    }

    bounds.removeFromTop (28);
    futureLabel.setBounds (bounds.removeFromTop (18));
}
