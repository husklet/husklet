# Nested engine oracle audit

The retained implementation was studied read-only in `../engine`.

| C capability | Oracle owner and entry | State, ordering, and teardown | Rust test owner | Status |
|---|---|---|---|---|
| Validate a chain before launch | `tools/nested_engine_gate.c:main` | Checks every executable in outer-to-inner order; a missing cross-tree artifact returns 77 and explicitly says the gate did not run | `src/apps/testing/src/nested.rs` manifest validation and `unavailable` | Implemented; unsupported is a non-green result |
| Construct recursive execution | `cmake/Phase3Gates.cmake:hl_nested_case` | Engine ELF architecture must equal its parent's guest ISA; final element is the leaf guest | Typed `Layer { artifact, guest_isa, options }` flattened in manifest order | Implemented; artifact production remains external and truthful |
| Per-layer engine configuration | C production engines consume their own command line; no ambient option is forwarded through a parent guest | Each layer owns its argv and lifetime; native diagnostics require native execution | `EngineOptions::append` immediately after its owning layer | Implemented |
| Exact result gate | `tools/nested_engine_gate.c:main`, `read_file` | Waits for the chain, requires ordinary exit and byte-exact stdout; stderr remains a diagnostic channel | `execute` | Implemented; native-mode rows additionally require propagated `hl-native-detail` evidence |
| Bounded supervision | `tools/process.c:hl_process_run`, `child_exec`, `read_output` | Fork/exec ownership, EINTR-safe reads and wait; the C capture grows without a bound and has no timeout/process-group teardown | `capture`, `drain`, `terminate_group` | Rust intentionally strengthens the oracle with byte bounds, timeout, TERM/KILL, and reap |
| Foreign static nested artifacts | `cmake/Phase2Production.cmake:hl_linux_production`, `cmake/Phase3Gates.cmake:nested-foreign-engines` | Second static cross-toolchain; deliberately absent from default build | Typed Cargo recipe plus `testing nested prepare`; the recipe and complete Rust source tree determine the cache key, while a SHA-256 receipt verifies reused bytes | Implemented; target toolchains remain an explicit host prerequisite |

Architecture branches are encoded by the manifest rather than host-conditionals in
the runner. The retained matrix covers same-ISA, cross-ISA, three-layer inverse,
and `aarch64 -> x86_64 -> aarch64`. `testing nested prepare` builds the declared
Linux targets with locked, offline Cargo, installs verified artifacts atomically,
and reuses only a content-bound cache entry. `testing nested run` prepares first,
so an unavailable compiler or target is a build failure rather than a false test
result.

The artifact recipe uses the pinned GNU Rust targets with `+crt-static` applied
only to the final `hl-engine` binary through `cargo rustc`; applying that feature
workspace-wide prevents proc-macro construction, while the MUSL libc bindings do
not expose Linux `statx` and socket ABI types used by the host adapter. The flake
provides a target-specific linker for each GNU triple. Commit `07dcff6be` removed
the former `runtime/core` fixture; the nested leaf is now the active
`runtime/abi-core/hello` artifact, whose own manifest declares ARM64, exit 42, and
the byte-exact `hi\n` output used by this gate.
