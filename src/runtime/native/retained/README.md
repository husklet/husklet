# Retained execution core

This directory contains the minimal Linux/AArch64 runtime closure retained from
the original Husklet C engine. It is the performance reference and initial
production backend while equivalent components are replaced incrementally.

- Upstream repository: `../engine`
- Source revision: `7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`
- Closure manifest: `RUNTIME_SOURCES.manifest`
- Manifest SHA-256: `5e2451d87384e2f357337a0c7a94a7a53740873e4a472885a55452d4d6214415`
- Compiled-unit manifest: `COMPILED_TUS.tsv`
- Compiled-unit manifest SHA-256: `f0b74563a0686c6ef448ecd96cbbf9cd70580f6e596a9855efcf2e8831a5cfb1`
- License: MIT; see `LICENSE`

The closure intentionally excludes graphics, GPU, CUDA, OpenGL, Vulkan,
Wayland, macOS, Windows, activation, and fake-host implementations. Files are
kept under their upstream relative paths so updates can be audited mechanically.
Do not edit `../engine`; changes to this retained copy belong to Husklet.
