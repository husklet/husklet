# Retained execution core

This directory contains the retained C runtime closure imported from the
original Husklet engine. It is Husklet's production execution backend and the
performance reference while components are replaced incrementally. The host
closure covers Linux/AArch64 and macOS/AArch64; the source inventory contains
both AArch64 and x86-64 guest translators. Production selection is currently
AArch64 guest-only, so presence in this tree does not by itself make the x86
target selectable.

- Upstream repository: `../engine`
- Source revision: `7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`
- Closure manifest: `RUNTIME_SOURCES.manifest`
- Manifest SHA-256: `5b275249a5718a139c46609beaf418b2fb58a542efca31addb85647aac31d8d9`
- Compiled-unit manifest: `COMPILED_TUS.tsv`
- Compiled-unit manifest SHA-256: `787499ee9c72fe5523e83bf430c719fcb78cace4876dc0397b7ef641b58dcd34`
- License: MIT; see `LICENSE`

The closure intentionally excludes graphics, GPU, CUDA, OpenGL, Vulkan,
Wayland, Windows, product activation, and fake-host implementations. Files are
kept under their upstream relative paths so updates can be audited mechanically.
Do not edit `../engine`; changes to this retained copy belong to Husklet.
