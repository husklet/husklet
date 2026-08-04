# Nested engine oracle audit

The retained implementation was studied read-only in `../engine`.

| C capability | Oracle owner and entry | State, ordering, and teardown | Rust test owner | Status |
|---|---|---|---|---|
| Validate a chain before launch | `tools/nested_engine_gate.c:main` | Checks every executable in outer-to-inner order; a missing cross-tree artifact returns 77 and explicitly says the gate did not run | `src/apps/testing/src/nested.rs` manifest validation and `unavailable` | Implemented; unsupported is a non-green result |
| Construct recursive execution | `cmake/Phase3Gates.cmake:hl_nested_case` | Engine ELF architecture must equal its parent's guest ISA; final element is the leaf guest | Typed `Layer { artifact, guest_isa, options }` flattened in manifest order | Implemented; artifact production remains external and truthful |
| Per-layer engine configuration | C production engines consume their own command line; no ambient option is forwarded through a parent guest | Each layer owns its argv and lifetime; native diagnostics require native execution | `EngineOptions::append` immediately after its owning layer | Implemented |
| Exact result gate | `tools/nested_engine_gate.c:main`, `read_file` | Waits for the chain, requires ordinary exit and byte-exact stdout; stderr remains a diagnostic channel | `execute` | Implemented; native-mode rows additionally require propagated `hl-native-detail` evidence |
| Bounded supervision | `tools/process.c:hl_process_run`, `child_exec`, `read_output` | Fork/exec ownership, EINTR-safe reads and wait; the C capture grows without a bound and has no timeout/process-group teardown | `capture`, `drain`, `terminate_group` | Rust intentionally strengthens the oracle with byte bounds, timeout, TERM/KILL, and reap |
| Foreign static nested artifacts | `cmake/Phase2Production.cmake:hl_linux_production`, `cmake/Phase3Gates.cmake:nested-foreign-engines` | Second static cross-toolchain; deliberately absent from default build | Manifest `ArtifactSource::ForeignBuild` plus required build instruction | Schema implemented; artifact build is an honest remaining gap |

Architecture branches are encoded by the manifest rather than host-conditionals in
the runner. The retained matrix covers same-ISA, cross-ISA, three-layer inverse,
and `aarch64 -> x86_64 -> aarch64`; this initial checked manifest names the final
acceptance chain without pretending the necessary static foreign binaries exist.
