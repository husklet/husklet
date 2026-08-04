# Runtime build-input oracle audit

This lane studied the retained C tree read-only before changing the Rust test
driver. The build graph is owned by
`../engine/cmake/GuestFixtures.cmake`:

- `_hl_guest_cc` selects the architecture-specific compiler command from the
  pinned environment without constructing a toolchain path.
- `hl_guest_binary` owns one output identity in the global
  `HL_GUEST_OUTPUTS` registry. It creates the output directory, invokes or
  copies the selected source, and declares both the primary source and every
  `G_DEPENDS` entry as build dependencies. Explicit declarations win over the
  later suite sweep. Compiler flags precede the source and libraries follow it,
  preserving link ordering.
- `hl_guest_suite`, `hl_guest_named`, and `hl_guest_pair` expand that primitive
  over architecture and source sets. They sort globbed sources and preserve
  the two retained output naming conventions.
- `hl_guest_finalize` publishes the complete output list and rejects an
  incomplete prebuilt corpus before execution.

The retained input fingerprint is in
`../engine/tools/guest_fixture_digest.cmake`.
`hl_guest_fixture_inputs` constructs a sorted, de-duplicated list of fixture
sources, headers, assembly/includes, committed binary inputs, and the exact
recipe files. `hl_guest_fixture_digest` hashes both each manifest-relative name
and its bytes. Golden output is deliberately excluded because it is not a
compiler input. The Windows prebuilt path in `GuestFixtures.cmake` compares the
digest before staging anything and registers every input with
`CMAKE_CONFIGURE_DEPENDS`, so an edit cannot silently reuse a stale corpus.

The execution/oracle side was checked in
`../engine/tools/matrix_runner.c`: `load_manifest` validates bounded relative
source and golden paths, `run_guest` owns bounded process execution and capture,
`run_one` compares exit status and byte-exact output and restores resources,
and `main` drives the manifest in declaration order. Suite ownership and
serialization are declared by `hl_compat_suite` in
`../engine/cmake/Phase3Compat.cmake`. Its `RESOURCE_LOCK` assignments protect
shared network, IPC, and scratch identities, while one case process owns its
temporary resources through teardown.

The retained graph has no application-specific lifetime state in these build
inputs. A source or dependency exists for the duration of the build graph; an
output exists until the build tree is removed. Architecture differences are
compiler selection, linkage, flags, dynamic-loader paths, and explicitly
excluded cases. Host differences are compilation versus digest-checked prebuilt
staging. Build failure is terminal for that output; execution timeouts, partial
capture, interruption, and resource cleanup are runner concerns rather than
build-input concerns.

## Rust mapping

`definition::input::ManifestPath` now owns portable `/`-separated identity.
Deserialization rejects absolute, empty, dot, parent, backslash, drive/stream,
control-character, Win32-invalid-character, trailing-dot/space, reserved-device,
and NUL spellings. It also rejects source/input identities that collide after
case folding, rather than accepting a graph on a case-sensitive checkout that
cannot be represented by a normal case-insensitive checkout. Loading resolves
every primary source and auxiliary input, requires a regular file inside the
canonical category root, rejects source aliases and duplicate canonical inputs,
and stores auxiliary identities in sorted order. The original manifest
spelling—not an absolute checkout path—is the durable identity.

`RuntimeCase::inputs` maps retained `G_DEPENDS`: auxiliary files affect the
case's resumability identity but are not inserted into the compiler argument
list. A linker script or header remains referenced by its explicit compiler
flag/include from the YAML recipe, matching retained `hl_guest_binary`, where
`DEPENDS` affects rebuild admission and `FLAGS`/`LIBS` affect the command.

`runtime::fingerprint` replaces the former unframed concatenation with
length-framed SHA-256 fields. It includes case/ISA metadata, the primary source,
every sorted auxiliary input with portable name and bytes, and the golden.
Names are hashed as well as bytes, so add/remove/rename/content changes all
invalidate resume state without depending on the checkout's absolute path.

One retained gap remains explicit: `Phase3Compat.cmake` passes
`elf_rodata_write.ld` in a linker flag but does not also declare it through
`G_DEPENDS`; its broad prebuilt-corpus digest still sees the file, while a local
incremental fixture edge may not. The Rust YAML must list such files in
`build.inputs` when manifests are migrated; this implementation lane does not
edit those manifests.
