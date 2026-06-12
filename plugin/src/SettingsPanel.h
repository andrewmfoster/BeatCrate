#pragma once

#include <juce_gui_basics/juce_gui_basics.h>
#include "Theme.h"

class SettingsPanel : public juce::Component,
                      private juce::Button::Listener
{
public:
    using ChooseCb = std::function<void()>;
    using RevealCb = std::function<void()>;
    using CloseCb  = std::function<void()>;

    SettingsPanel();
    ~SettingsPanel() override;

    void setDbPath (const juce::String& path, bool missing);

    void setOnChoose (ChooseCb cb) { onChoose = std::move (cb); }
    void setOnReveal (RevealCb cb) { onReveal = std::move (cb); }
    void setOnClose  (CloseCb  cb) { onClose  = std::move (cb); }

    void paint (juce::Graphics&) override;
    void resized() override;

private:
    void buttonClicked (juce::Button*) override;

    BC::Disc miniDisc;
    juce::Label title { {}, "SETTINGS" };
    juce::TextButton closeBtn { juce::String::fromUTF8 ("\xc3\x97") };

    juce::Label dbLabel { {}, juce::String::fromUTF8 ("\xe2\x80\xa2 DATABASE") };
    juce::Label pathBox;
    juce::Label helper;
    juce::TextButton changeBtn { "CHANGE\xe2\x80\xa6" };
    juce::TextButton revealBtn { "REVEAL IN FINDER" };
    juce::Label futureLabel    { {}, juce::String::fromUTF8 ("\xe2\x80\x94 MORE ON THE B-SIDE \xe2\x80\x94") };

    bool dbMissing = false;

    ChooseCb onChoose;
    RevealCb onReveal;
    CloseCb  onClose;
};
