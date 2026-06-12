#include "PluginProcessor.h"
#include "PluginEditor.h"

BeatCrateProcessor::BeatCrateProcessor()
    : juce::AudioProcessor (BusesProperties()
        .withInput  ("Input",  juce::AudioChannelSet::stereo(), true)
        .withOutput ("Output", juce::AudioChannelSet::stereo(), true))
{
    dbPath = BeatCrateDB::defaultDbPath();
    openDbAndLoad();
}

void BeatCrateProcessor::openDbAndLoad()
{
    // May be invoked off the message thread (setStateInformation). Do the DB work first
    // (db.open()/listCrates() are internally locked), then publish the result under
    // cratesLock so the editor never observes a half-rebuilt vector.
    std::vector<Crate> loaded;
    juce::String status;

    if (db.open (dbPath))
    {
        loaded = db.listCrates();
        status = "DB OK: " + juce::String (loaded.size()) + " crates";
    }
    else
    {
        status = "DB not found - click DB to locate";
    }

    const juce::ScopedLock sl (cratesLock);
    crates   = std::move (loaded);
    dbStatus = status;
}

void BeatCrateProcessor::setDbPath (const juce::File& file)
{
    dbPath = file;
    openDbAndLoad();
}

void BeatCrateProcessor::reloadCrates()
{
    if (! db.isOpen()) return;
    auto loaded = db.listCrates();
    const juce::ScopedLock sl (cratesLock);
    crates = std::move (loaded);
}

void BeatCrateProcessor::prepareToPlay (double, int) {}
void BeatCrateProcessor::releaseResources() {}

bool BeatCrateProcessor::isBusesLayoutSupported (const BusesLayout& layouts) const
{
    const auto& mainIn  = layouts.getMainInputChannelSet();
    const auto& mainOut = layouts.getMainOutputChannelSet();
    return mainIn == mainOut
        && (mainOut == juce::AudioChannelSet::mono()
         || mainOut == juce::AudioChannelSet::stereo());
}

void BeatCrateProcessor::processBlock (juce::AudioBuffer<float>& buffer, juce::MidiBuffer&)
{
    juce::ScopedNoDenormals noDenormals;
    const auto totalNumInputChannels  = getTotalNumInputChannels();
    const auto totalNumOutputChannels = getTotalNumOutputChannels();

    for (int i = totalNumInputChannels; i < totalNumOutputChannels; ++i)
        buffer.clear (i, 0, buffer.getNumSamples());
}

juce::AudioProcessorEditor* BeatCrateProcessor::createEditor()
{
    return new BeatCrateEditor (*this);
}

void BeatCrateProcessor::getStateInformation (juce::MemoryBlock& destData)
{
    juce::XmlElement xml ("BeatCrateState");
    xml.setAttribute ("dbPath",      dbPath.getFullPathName());
    xml.setAttribute ("lastCrateId", juce::String (lastCrateId));
    xml.setAttribute ("lastTrackId", juce::String (lastTrackId));
    xml.setAttribute ("windowWidth",  windowWidth);
    xml.setAttribute ("windowHeight", windowHeight);
    copyXmlToBinary (xml, destData);
}

void BeatCrateProcessor::setStateInformation (const void* data, int sizeInBytes)
{
    auto xml = getXmlFromBinary (data, sizeInBytes);
    if (xml == nullptr || ! xml->hasTagName ("BeatCrateState")) return;

    const auto savedPath = xml->getStringAttribute ("dbPath");
    if (savedPath.isNotEmpty())
    {
        const juce::File f (savedPath);
        if (f.existsAsFile()) // only adopt the saved path if it still exists
            dbPath = f;
    }

    lastCrateId = xml->getStringAttribute ("lastCrateId", "0").getLargeIntValue();
    lastTrackId = xml->getStringAttribute ("lastTrackId", "0").getLargeIntValue();

    windowWidth  = xml->getIntAttribute ("windowWidth",  windowWidth);
    windowHeight = xml->getIntAttribute ("windowHeight", windowHeight);

    openDbAndLoad();
}

juce::AudioProcessor* JUCE_CALLTYPE createPluginFilter()
{
    return new BeatCrateProcessor();
}
