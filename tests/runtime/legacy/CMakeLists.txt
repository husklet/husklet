cmake_minimum_required(VERSION 3.24)
project(hl_compat_corpus LANGUAGES NONE)

set(HL_CORPUS_MODE "PREBUILT" CACHE STRING "PREBUILT or REBUILD smoke corpus")
set_property(CACHE HL_CORPUS_MODE PROPERTY STRINGS PREBUILT REBUILD)

include(cmake/Corpus.cmake)
hl_compat_smoke(compat-smoke)


find_package(Python3 COMPONENTS Interpreter)
if(Python3_Interpreter_FOUND AND EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/artifacts/manifest.tsv")
  add_custom_target(compat-full-verify
    COMMAND "${Python3_EXECUTABLE}" "${CMAKE_CURRENT_SOURCE_DIR}/corpus.py" verify
    WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}"
    VERBATIM)
endif()
