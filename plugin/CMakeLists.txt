cmake_minimum_required(VERSION 3.22)

project(BeatCratePlugin VERSION 0.1.0 LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_OSX_DEPLOYMENT_TARGET "11.0" CACHE STRING "macOS deployment target")
# Apple Silicon only — Universal Binary is intentionally out of scope (see project scope).
set(CMAKE_OSX_ARCHITECTURES "arm64" CACHE STRING "Build arm64 only")

# Privacy: rewrite the build machine's home path (incl. username) out of absolute
# paths the compiler embeds — __FILE__, debug info, and symbol source-locations
# (e.g. JUCE's HarfBuzz template instantiations). Mirrors the desktop app's
# --remap-path-prefix=$HOME=/build, but uses $ENV{HOME} resolved at configure
# time so this tracked file never contains the literal home path / username.
# Placed before add_subdirectory(JUCE) so JUCE sources inherit it too.
if(CMAKE_CXX_COMPILER_ID MATCHES "Clang" AND DEFINED ENV{HOME})
    add_compile_options(-ffile-prefix-map=$ENV{HOME}=/build)
endif()

# Locate JUCE. Default: per-project checkout at ../JUCE relative to this file
# (i.e. apps/BeatCrate/JUCE), pinned to the 8.0.12 tag and gitignored — it is
# AGPLv3/commercial dual-licensed and this repo is MIT, so it is never committed.
# A fresh clone needs it restored before the plugin will build:
#   git clone --depth 1 --branch 8.0.12 https://github.com/juce-framework/JUCE.git JUCE
# Override with -DJUCE_PATH=... or env $JUCE_PATH.
if(NOT DEFINED JUCE_PATH)
    if(DEFINED ENV{JUCE_PATH})
        set(JUCE_PATH "$ENV{JUCE_PATH}")
    else()
        set(JUCE_PATH "${CMAKE_CURRENT_SOURCE_DIR}/../JUCE")
    endif()
endif()

if(NOT EXISTS "${JUCE_PATH}/CMakeLists.txt")
    message(FATAL_ERROR "JUCE not found at ${JUCE_PATH}. Set -DJUCE_PATH=/path/to/JUCE")
endif()

add_subdirectory("${JUCE_PATH}" "${CMAKE_BINARY_DIR}/JUCE" EXCLUDE_FROM_ALL)

# Copy-after-build installs straight into ~/Library/Audio/Plug-Ins/VST3/. Convenient
# for local iteration, dangerous for audit/CI builds (a plain build mutates the live
# plugin the host loads). Default OFF; opt in for local install builds with
#   cmake -DBEATCRATE_COPY_PLUGIN_AFTER_BUILD=ON ...
option(BEATCRATE_COPY_PLUGIN_AFTER_BUILD "Copy the built VST3 into the user plug-in dir" OFF)

juce_add_plugin(BeatCrate
    COMPANY_NAME              "Wander Foster"
    BUNDLE_ID                 "com.beatcrate.plugin"
    PLUGIN_MANUFACTURER_CODE  Bcrt
    PLUGIN_CODE               Bc01
    FORMATS                   AU VST3
    PRODUCT_NAME              "BeatCrate"
    IS_SYNTH                  FALSE
    NEEDS_MIDI_INPUT          FALSE
    NEEDS_MIDI_OUTPUT         FALSE
    COPY_PLUGIN_AFTER_BUILD   ${BEATCRATE_COPY_PLUGIN_AFTER_BUILD}
)

target_sources(BeatCrate PRIVATE
    src/PluginProcessor.cpp
    src/PluginEditor.cpp
    src/BeatCrateDB.cpp
    src/Theme.cpp
    src/NoteRow.cpp
    src/SettingsPanel.cpp
    src/sqlite3.c
)

# Silence the SQLite amalgamation's expected warnings
set_source_files_properties(src/sqlite3.c PROPERTIES
    COMPILE_FLAGS "-w"
)

target_compile_definitions(BeatCrate PRIVATE
    JUCE_WEB_BROWSER=0
    JUCE_USE_CURL=0
    JUCE_VST3_CAN_REPLACE_VST2=0
    SQLITE_THREADSAFE=1
    SQLITE_DEFAULT_MEMSTATUS=0
    SQLITE_OMIT_DEPRECATED=1
    SQLITE_OMIT_LOAD_EXTENSION=1
)

target_link_libraries(BeatCrate PRIVATE
    juce::juce_audio_utils
    juce::juce_audio_processors
    juce::juce_gui_basics
    juce::juce_core
    juce::juce_recommended_config_flags
    juce::juce_recommended_lto_flags
    juce::juce_recommended_warning_flags
)

# Strip local symbols from the binary (size + privacy). Mirrors the app's strip=true.
# -x removes local symbols only; exported VST3 entry points (global) are preserved.
target_link_options(BeatCrate PRIVATE -Wl,-x)

# Re-seal the bundles after JUCE finishes with them. JUCE signs the .vst3 and THEN
# regenerates Contents/Resources/moduleinfo.json, which invalidates the seal
# ("a sealed resource is missing or invalid"); Ableton silently rejects the plugin
# on rescan, so a broken build looks like a build that simply never appears.
# Registered after juce_add_plugin so these run last in the POST_BUILD chain.
foreach(_fmt VST3 AU)
    if(TARGET BeatCrate_${_fmt})
        add_custom_command(TARGET BeatCrate_${_fmt} POST_BUILD
            COMMAND codesign --force --deep --sign - "$<TARGET_BUNDLE_DIR:BeatCrate_${_fmt}>"
            COMMENT "Re-sealing ${_fmt} bundle (JUCE breaks the seal writing moduleinfo.json)"
            VERBATIM)
    endif()
endforeach()
