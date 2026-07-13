# Host paths and generated artifacts audit — wave Q (2026-07)

Documentation-only audit of image-store resolution, shell/Rust setup duplication, checked-in guest
executables, and website source/output media.

## Canonical image-store resolver

Product code already defines the correct contract in `dd-cli/src/paths.rs`: user state root is
`$HOME/.dd`, and the image store is `$HOME/.dd/images`; launchd and daemon launch explicitly pass that as
`DD_IMAGES`. Tests should share the contract without depending on the CLI crate.

Define one small resolver in `dd-tests` test support with this precedence:

1. explicit `DD_IMAGES`;
2. `$HOME/.dd/images` when it exists/when product-state tests are intended;
3. a repository-relative legacy fixture store only when explicitly present (derive repository root from
   `CARGO_MANIFEST_DIR`, never `/Users/x/...`);
4. an actionable error naming `DD_IMAGES`, rather than silently selecting a nonexistent directory.

Expose the resolved path to shell scripts via one lightweight command/helper or require Make to export it
once. Do not duplicate fallback expressions in every script. The daemon itself should continue consuming
`DD_IMAGES`; importing test resolver logic into production would invert ownership.

Exact stale branches/comments removable after centralization: nine `${DD_IMAGES:-/Users/x/dd/poc/images}`
assignments, two Rust `PathBuf::from("/Users/x/dd/poc/images")` fallbacks, coverage's absolute alpine glob,
comments describing `/Users/...` shared mounts, and repeated script headers claiming `poc/images` is the
default. Keep unique temp socket/state/volume directories and environment exports: those provide test
isolation, not duplicate resolution.

`pty-conformance.sh` already defaults to `$HOME/.dd/images` and is the closest shell precedent. The daemon's
standalone `./images` default is a separate direct-binary development behavior; changing it is a product
migration, not test cleanup.

## Shell/Rust setup duplication

Docker scenario scripts repeat daemon socket/state/volume setup, export construction, readiness polling,
and cleanup. The Rust scenario daemon owns equivalent lifecycle logic but the shell suites cover distinct
Docker/Compose surfaces. Deduplicate setup, not assertions: a shared sourced shell library can own path
resolution, unique state creation, daemon launch/readiness, and trap cleanup. Keep each suite's command
sequence local so failure output and startup speed do not regress.

Avoid replacing shell setup with a new Rust subprocess per suite; that adds build/start overhead. A sourced
POSIX helper preserves current speed. Long-term Rust migration is justified only when a scenario's exact
assertions and failure diagnostics are ported.

## Executable/source provenance

Git history currently shows no stale same-stem pair among the audited fixtures:

- all five x86 C/binary pairs last changed together in `8e742294`;
- Darwin `hello.c`/`hello` changed together in `62c6c3ba`;
- all twenty tracked GUI-matrix C/binary pairs changed together in `f26743cd`.

This is evidence of synchronized commits, not reproducible identity. The x86 `build.sh` documents exact
compiler flavors and prints hashes but does not persist expected hashes. The GUI Makefile records flags and
header dependencies, but no compiler version or source/header digest. Shared headers can change without a
same-stem source timestamp/history signal, so pairwise commit dates alone cannot prove freshness.

Add a checked-in manifest per fixture family containing output name, SHA-256, source plus shared-header
SHA-256, target triple, compiler/version, and exact flags. A fast Rust test should hash existing files and
compare the manifest; it must not rebuild during normal tests. The explicit maintainer rebuild job updates
binaries and manifest atomically. This preserves zero-build test startup and catches header-only drift.

Do not delete the binaries: x86 fixtures intentionally pin static/non-PIE/static-PIE/Go ELF flavors, and
GUI probes need guest Wayland/EGL libraries unavailable on generic hosts. Untracked/new GUI sources should
not be assumed covered merely because the Makefile names them; coverage gates should require source,
build-list, run-list, and (when prebuilt execution is supported) manifest agreement.

## Website/media source and outputs

The site tracks three GIF/poster pairs (`dd-run`, `dd-inside`, `dd-docker`), `demo.tape`,
`gen_demo.py`, `SCREENCAST.md`, app screenshot, and four logo/icon raster sizes. HTML consumes both GIF and
poster intentionally: posters avoid eager animation/download and JavaScript swaps in GIFs on play. These
are performance assets, not redundant copies; removing posters worsens page load and layout behavior.

However, generation ownership is split: `demo.tape`/`gen_demo.py` are potential sources, while
`SCREENCAST.md` describes manual capture, and no manifest maps a source/capture to the six outputs. Add a
media manifest with dimensions, byte size, SHA-256, generation command/tool version, and source commit.
Keep generated assets checked in so GitHub Pages remains a static, zero-build deploy.

Logo/icon variants are consumed at different declared sizes (`favicon`, 64px favicon, apple touch icon,
normal and 2x logo). Retain them for browser/device performance. If regenerated, derive all from one
declared master and validate dimensions; do not generate at page request/build time.

Safe text cleanup: update `SCREENCAST.md`'s obsolete `dd app` command to the canonical rebrand command,
remove any generation step that no longer produces a tracked asset once the manifest identifies the real
pipeline, and delete stale `make bench` site reproduction text after benchmark removal. No media binary is
currently proven orphaned: all six GIF/poster files, screenshot, and logo/icon variants have HTML consumers.

## Ordered plan

1. Add a shared test image-store resolver and sourced shell lifecycle helper; remove absolute fallbacks.
2. Add fixture manifests and fast hash verification without runtime recompilation.
3. Add a static-site media manifest; keep posters and generated outputs checked in.
4. Remove only stale path/generation/rebrand comments after their canonical owners exist.
5. Preserve product, test, and site startup performance throughout.
