# Husklet atomic rename waves

Phase 3 begins only after phase-2 test ownership is stable. Each wave ends green and has a narrow rollback
point. Do not combine all brand changes into an unreviewable repository-wide substitution.

## R0 — freeze manifests and decisions

Generate current package/artifact, environment producer/consumer, FFI/export, endpoint, persisted-format,
user-copy and exclusion manifests. Decide binary/JIT artifact names and every item in `decisions.md`.
Record old state/archive fixtures and a packaged `dd.app` smoke baseline.

## R1 — internal Rust package graph

Rename crate directories, Cargo packages/libs, dependency keys/path/package aliases, workspace members,
Rust imports and crate-owned test commands. Keep executable names, environment variables, endpoints and
persisted formats unchanged during this wave so failures are build-graph-only.

Gate: `cargo metadata`, formatting, every headless crate test, all-target builds and mac crate build/tests.
Rollback is the single R1 commit.

## R2 — C/Rust internal and FFI symbols

Rename project-private `dd_/ddjit_` symbols and include guards. Treat launch FFI struct/function/magic/header
as one subcommit with generated ABI manifest before/after. Standard API symbols stay unchanged.

Gate: C unity builds for all engines, Rust FFI layout tests, exported-symbol comparison, direct spawn of
all engine lanes and shim dlopen/loader clients.

## R3 — executable and packaged artifact names

Rename Cargo bins, JIT outputs, Vulkan ICD library/SONAME, bundle resource names and every resolver. Make
packaging fail if any required artifact is absent; do not accept old-name fallback as completion.

Gate: clean release build, artifact manifest exact match, app bundle codesign inspection, CLI/daemon/display
launch from bundle and Vulkan loader discovery.

## R4 — environment and cross-process endpoints

Rename supported environment variables by lockstep group, then socket/service names. Update CLI/GUI setters,
daemon/JIT/display/shim readers, engine forwarding, tests and documentation together. Remove obsolete
controls rather than rebrand them.

Gate: intentionally set every public override to a nondefault value and prove the intended consumer
observed it; additionally poison old defaults so half-renames cannot pass. Exercise daemon socket, all JIT
controls, Wayland, GPU execution, Mach IOSurface bridge and guest shims.

## R5 — state, xattrs, archives and caches

Introduce the decided migration/fresh-cutover behavior before switching writers. Move state root and logs;
version or explicitly reject old sidecars/archives/xattrs; invalidate caches whose identity changed.

Gate: clean install, old-root detection, explicit migration if supported, restart/state recovery, old/new
archive fixtures, image discovery, workspace config, alias/tag behavior, cache miss/rebuild and uninstall
with/without purge.

## R6 — app/service/release/user surface

Switch reverse-DNS IDs, launchd, app bundle/dmg names, updater, Docker context/system strings, website,
media and release workflow. Publish external images/assets before changing defaults.

Gate: install → launch → container → GUI → update → uninstall journey; launchd has no old job; Docker
context points to the Husklet socket; website link/media validation; release assets contain only selected
names.

## R7 — residue and compatibility audit

Classify every remaining `dd`, `DD`, `ddjit` and `com.dd` match. Allowed residues are machine-readable and
limited to compatibility readers/fixtures, historical text, upstream/reference content and intentionally
standard external names. Unclassified project-owned residue fails the phase.

Run all crate-owned tests, all engine lanes, daemon quick real-image scenarios, shim ABI clients,
compositor/Metal/IOSurface gates and packaged application smoke. Only then remove temporary migration
instrumentation and declare the rebrand complete.

## Rollback rule

Rollback reverses a whole wave, never individual producer/consumer files. Persisted-data waves require a
forward recovery plan because a writer may already have emitted new state; Git reversal alone is not a
data rollback.
