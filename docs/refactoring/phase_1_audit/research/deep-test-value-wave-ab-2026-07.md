# Test-value audit — wave AB (2026-07)

Documentation-only audit of Rust/C tests for source-text checks, duplicate coverage and false-green skips.
No local `AGENTS.md` or `.dev/AGENTS.local.md` exists in this worktree.

## Remove source-text “correctness” tests

### `dd-gpu/tests/capability_handshake.rs`

Delete these two tests and the `root`/`read` helpers they exclusively use:

- `backend_capabilities_describe_negotiable_shader_format_and_command_support` reads
  `dd-gpu/src/backend.rs` and searches eight field-name substrings. Comments, unused fields, or unrelated
  identifiers satisfy it; renaming/refactoring correct code fails it.
- `wgpu_present_capability_has_a_working_present_operation` reads `dd-gpu-wgpu/src/backend.rs` and compares
  two exact source snippets. Equivalent formatting/helper extraction evades it, and a present method can
  still return a different false-success/error shape.

Behavioral replacements:

1. Construct every shipped backend and call `capabilities()`; round-trip its handshake and assert each
   advertised command/format/payload can enter the corresponding public operation or is rejected during
   negotiation before execution. The existing software round-trip/negotiate tests already cover most of
   this contract.
2. For wgpu presentation, instantiate the backend in the macOS GPU gate, inspect the runtime capability,
   create the advertised present target, render a known 2×2 pattern, call `present`, and read/capture the
   target. If the backend does not advertise presentation, assert negotiation rejects it. This tests the
   contradiction directly.
3. Compile-time field existence needs no test: code constructing/destructuring `Capabilities` by named
   fields is compiler evidence.

These source mirrors explicitly admit duplicating tracked source-inspection gates; remove both copies rather
than maintaining parallel substring checks.

## Keep build/ABI inventory tests, but parse authoritative data

`dd-tests/tests/gate_invariants.rs::test_every_gui_matrix_probe_is_gated_or_documented` is not a runtime
correctness test; it is build-inventory evidence and should remain. Its current implementation tokenizes the
entire Makefile, so a probe name in a comment or unrelated rule counts as gated. Replace whitespace search
with one of:

- `make -qp`/a dedicated `print-probes` target returning the expanded `PROBES` variable; or
- a generated checked-in probe manifest consumed by both Make and the Rust gate.

Then require four-way agreement: C source, build manifest, run matrix, and expected executable/hash manifest.
The exclusion table must require a nonempty owner/reason and should be empty in release gates. This remains
legitimate manifest/build coverage rather than pretending code behavior exists.

Likewise Vulkan/GL exported-command manifests are valid build/ABI census evidence when generated from
headers/registries and cross-checked against exported symbols. They do not prove implementation correctness;
pair each advertised command class with behavioral tests, but do not delete the census.

## Constant/string tests that should become observable archive tests

The following assert that hard-coded argument arrays contain flag strings, not that archive behavior works:

- `dd-images/src/image/archive/save.rs` assertions over `SAVE_FLAGS` (`--xattrs`, `--sparse`,
  `--format=posix`);
- `dd-images/src/image/archive/import.rs` assertions over `EXTRACT_FLAGS` (`--numeric-owner`,
  `--same-owner`, `-p`, `--xattrs`).

Replace them with one archive round-trip fixture containing executable/set mode bits, a sparse file, xattr
(when supported), nanosecond mtime, numeric uid/gid (in an isolated permitted environment), symlink,
hardlink, and whiteout. Assert `stat`/filesystem observations after save+load/import. Platform-unavailable
xattrs/ownership should be explicit capability cases, not silently counted as coverage. Existing archive
content and whiteout tests provide a base; merge flag-presence tests into those behavioral fixtures.

String checks on generated WGSL/MSL can be legitimate compiler-output golden tests when the string is the
observable translation product, but broad `contains("fn ")` smoke checks are weak. Prefer parsing/compiling
the shader and executing a minimal buffer/pixel workload. Keep exact translation goldens where textual
stability is itself the contract.

## False-green skips

### Engine tests

These named tests return success when required engines are absent:

- `overlay_correctness_aarch64`, `overlay_correctness_x86_64`;
- `pcache_lifecycle_aarch64`, `pcache_policy_aarch64`, `pcache_policy_x86_64`;
- `forkserver_equivalence_aarch64`, `forkserver_equivalence_x86_64` through `engine_lane`;
- `nonpie_dladdr_rtld_next_aarch64` at macOS/toolchain/engine/native-oracle branches.

The suite-level matrix guard does not cover these standalone integration tests. Split discovery from behavior:
unit-test helpers everywhere, mark environment-specific integration tests with an explicit runner category,
and make the CI/Make target for that category preflight required engines/toolchains then fail if missing.
Local optional runs may report a machine-readable ignored/unsupported status, but a plain passed test must
never mean “returned before assertion”. For `nonpie_dladdr`, an unexpected native oracle is a failure, not a
reason to skip.

### GL parity tests

`translator_matches_gl_shim_c_over_the_shader_corpus` skips the entire test if C build fails and skips
individual shader pairs when the reference translator fails. It only asserts `checked > 0`, so one passing
pair can hide many untested failures. Require the reference tool in the parity job and assert
`checked == discovered_complete_pairs`; missing fragment/reference failure must fail with the pair name.

Seven `dd-shim-gl/tests/pixel_parity.rs` tests (`full_frame_clear...`, `textured_triangle...`,
`multi_draw...`, `wl_egl_window...`, `fbo...`, `mat3_uniform...`, `texstorage_and_mapbuffer...`) return
green on compiler, workload, or cdylib absence. Add a one-time prerequisite preflight and make the dedicated
parity target fatal. Keep pure `diff_engine_detects_divergence_and_agreement` as a unit test.

### Metal/wgpu tests

Metal texture-copy, Chrome IR, render-to-texture, backend-validation and golden suites commonly return
success on “no Metal device”; golden cases also skip missing captures/goldens individually. This is acceptable
only outside the macOS GPU gate. `make mac-crates`/release GPU CI must set a required-device/golden mode where
absence is fatal and assert a nonzero expected case count. Missing golden files should fail normal comparison;
generation belongs in an explicit `--update` workflow, never an automatic green skip.

## Duplicate and maintenance-only coverage

- Remove the two capability source mirrors; behavioral tests in the same file already supersede them.
- Remove benchmark-only gate tests with the benchmark island, as previously approved.
- Consolidate repeated engine/toolchain discovery across overlay, pcache, forkserver and nonpie tests into a
  shared Rust prerequisite module that returns typed unavailable reasons. This reduces maintenance without
  merging the distinct observable behaviors.
- Do not remove C guest fixtures merely because Rust launches them. C is the observable guest ABI workload;
  Rust should own orchestration and assertions.
- Do not replace compositor/pixel tests with checks that handler/function names exist. Assert protocol
  round-trips, pixels, input coordinates, lifecycle errors and serialized bytes.

## Ordered action

1. Delete four source-reader helpers/tests (two tests plus `root`/`read`) from capability handshake.
2. Strengthen GUI/ABI manifests as generated build evidence, clearly labeled non-behavioral.
3. Replace archive flag-string assertions with filesystem round trips.
4. Add required prerequisite modes and exact executed-case counts for engine, GL parity and Metal gates.
5. Preserve C ABI fixtures and behavioral Rust orchestration; eliminate green early returns in required CI.
