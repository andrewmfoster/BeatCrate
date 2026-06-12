#pragma once

#include <juce_audio_processors/juce_audio_processors.h>
#include "BeatCrateDB.h"

class BeatCrateProcessor : public juce::AudioProcessor
{
public:
    BeatCrateProcessor();
    ~BeatCrateProcessor() override = default;

    // Returns a snapshot copy under cratesLock: openDbAndLoad() may rebuild the vector
    // from a host loader thread (setStateInformation) while the editor reads it on the
    // message thread, so handing back a reference would be a data race on the container.
    std::vector<Crate> getCrates() const { const juce::ScopedLock sl (cratesLock); return crates; }
    const juce::String& getDbStatus() const { return dbStatus; }
    BeatCrateDB& getDb() { return db; }

    const juce::File& getDbPath() const { return dbPath; }
    void setDbPath (const juce::File& file); // reopens DB + reloads crates
    void reloadCrates();                      // re-query crates on the open DB (no reopen)

    int64_t getLastCrateId() const { return lastCrateId; }
    int64_t getLastTrackId() const { return lastTrackId; }
    void setLastCrateId (int64_t id) { lastCrateId = id; }
    void setLastTrackId (int64_t id) { lastTrackId = id; }

    int getSavedWindowWidth() const  { return windowWidth; }
    int getSavedWindowHeight() const { return windowHeight; }
    void setSavedWindowSize (int w, int h) { windowWidth = w; windowHeight = h; }

    void prepareToPlay (double sampleRate, int samplesPerBlock) override;
    void releaseResources() override;
    void processBlock (juce::AudioBuffer<float>&, juce::MidiBuffer&) override;

    juce::AudioProcessorEditor* createEditor() override;
    bool hasEditor() const override { return true; }

    const juce::String getName() const override { return "BeatCrate"; }
    bool acceptsMidi() const override { return false; }
    bool producesMidi() const override { return false; }
    bool isMidiEffect() const override { return false; }
    double getTailLengthSeconds() const override { return 0.0; }

    int getNumPrograms() override { return 1; }
    int getCurrentProgram() override { return 0; }
    void setCurrentProgram (int) override {}
    const juce::String getProgramName (int) override { return {}; }
    void changeProgramName (int, const juce::String&) override {}

    void getStateInformation (juce::MemoryBlock&) override;
    void setStateInformation (const void*, int) override;

    bool isBusesLayoutSupported (const BusesLayout& layouts) const override;

private:
    void openDbAndLoad();

    BeatCrateDB db;
    juce::File dbPath;
    std::vector<Crate> crates;
    juce::String dbStatus;
    mutable juce::CriticalSection cratesLock; // guards crates against the loader-thread rebuild

    int64_t lastCrateId = 0;
    int64_t lastTrackId = 0;
    int windowWidth  = 480;
    int windowHeight = 600;

    JUCE_DECLARE_NON_COPYABLE_WITH_LEAK_DETECTOR (BeatCrateProcessor)
};
