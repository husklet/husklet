# Retained execution core

This directory contains the minimal Linux/AArch64 runtime closure retained from
the original Husklet C engine. It is the performance reference and initial
production backend while equivalent components are replaced incrementally.

- Upstream repository: `../engine`
- Source revision: `7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`
- Closure manifest: `RUNTIME_SOURCES.manifest`
- Manifest SHA-256: `0b8dce3514190f205b3b563295459d3c359b123ddecc230c61506208978efa27`
- Compiled-unit manifest: `COMPILED_TUS.tsv`
- Compiled-unit manifest SHA-256: `8df39e6618305d80e38c4f605d36a19d9d75d53069466d71aa093097801e19f9`
- License: MIT; see `LICENSE`

The closure intentionally excludes graphics, GPU, CUDA, OpenGL, Vulkan,
Wayland, macOS, Windows, activation, and fake-host implementations. Files are
kept under their upstream relative paths so updates can be audited mechanically.
Do not edit `../engine`; changes to this retained copy belong to Husklet.
