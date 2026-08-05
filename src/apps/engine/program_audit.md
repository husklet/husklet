# Engine program compatibility audit

The application integration tests exercise the retained launch wire, executable
handoff, and process-exit contract. The corresponding retained C implementation
was inspected in:

- `../engine/src/core/cli.c`, entry `hl_cli_route_parse`;
- `../engine/src/core/target/run.c`, entry `hl_native_engine_run`;
- `../engine/src/translator/guest/x86_64/translate.c`, `emit_sigill`;
- `../engine/src/translator/guest/x86_64/interp.c`, the undefined-opcode paths
  through `interp_guest_trap`;
- `../engine/src/linux_abi/signal.c`, `sig_coredumps`, the signal-death relay,
  and `deliver_guest_fatal_fault`.

## Retained behavior and Rust ownership

| Capability | Retained C behavior | Rust owner | State |
|---|---|---|---|
| Config route | A leading `--configfile PATH` selects the configuration wire before server/client routing. | `hl_engine::cli::Route` and `program::PlanSource` | Implemented |
| Launch wire | The version-one header and its string pool travel as one bounded image; malformed size, offsets, or termination are rejected before launch. | `hl_engine::launcher::wire` and `config::LaunchConfig` | Implemented; header is 192 bytes after `name_binds_offset` was added. |
| Rooted interpreter | The root filesystem scopes absolute guest paths while the executable supplied by the launcher remains a host input. The interpreter is opened beneath the guest root. | `hl_engine::program`, `ffi::linux::execution::routing`, and `hl_loader` | Implemented |
| Exit code | A normal guest exit is returned unchanged. | `Program::exit_status` | Implemented |
| Signal death | A fatal default guest signal returns `128 + signo`; retained code separately preserves signal identity for a guest parent. | scheduler signal delivery, task exit state, and `Program::exit_status` | Implemented |
| Undefined x86 instruction | Architecturally undefined opcodes are guest `SIGILL`/`ILL_ILLOPN`, not an engine decode failure. An unhandled signal exits 132. | x86 decoder, scheduler fault-to-signal routing, and signal delivery | Implemented |

The launch image is owned by the invoking process until parsing completes. Rust
then owns validated strings and arguments. Runtime process, mapping, signal, and
interpreter state are scoped to the engine instance and torn down when its wait
path completes. The retained route and exit paths acquire no cross-engine global
lock. Partial configuration reads or corrupt offsets fail before any guest state
is created. The ISA-specific branch relevant here is x86-64 undefined-instruction
classification; host selection does not change the Linux-visible `SIGILL`
contract.

This audit found two stale tests rather than product defects: their handcrafted
wire still used the former 184-byte header, and their undefined opcode expected
an engine fault even though both the retained C engine and the Rust scheduler
correctly classify it as a guest signal.

## Executable ownership

The migrated `../engine_rust/src/app/hl-engine` tree placed its library and all
three production executables in one crate. Husklet deliberately separates the
reusable composition crate at `src/containers/hl-engine` from the packaged
process adapters at `src/apps/engine`. That split is sound: daemon and container
callers reuse the composition API without depending on executable policy.

The initial split copied the AArch64 and x86-64 worker mains independently. Both
owned the same environment capture, authority-descriptor conversion, logging,
event, program invocation, and exit-status adapter, with only the executable
identity and an existing x86 opt-in error print differing. The shared worker now
lives in the application crate library; each binary supplies only its fixed
guest identity. `hl_engine::program::Program` remains the sole owner of launch,
runtime, and Linux exit semantics, and the x86 diagnostic difference is retained
exactly. No retained C runtime behavior is changed by this application-only
deduplication.

## Initial image authority and rooted lookup

The initial-image ownership boundary was re-audited against:

- `../engine/include/hl/engine.h`, `hl_engine_executable` and
  `hl_engine_config::executable`;
- `../engine/src/core/lifecycle.c`, `hl_production_entry` and
  `hl_run_linux_guest` call sites;
- `../engine/src/core/target/{aarch64,x86_64}.c`, `load_program`;
- `../engine/src/linux_abi/elf.c`, `elf_interp`;
- `../engine/src/linux_abi/container/vfs.c`, `xresolve_exec`; and
- `../engine/src/linux_abi/container/vfs/overlay.c`, `xresolve_overlay`.

The retained launcher owns a separately authorized initial-main handle/image.
That authority is consumed only for the initial main image and is never reused
for its `PT_INTERP`, later `exec`, or another process. Guest-path lookup remains
root confined, follows absolute symlinks relative to the root, searches overlay
layers in order, and reports lookup failure without falling through to the host.
The per-launch image and source state outlive loader inspection and are released
with the engine process; no table lock is held across a host open or image read.
Both guest ISAs use the same ownership split. Host-specific launch transport may
carry authorized bytes instead of a live handle, but does not change the split.

Rust maps the authorized main to `RuntimePlan::executable_host` and
`FileSource`'s non-root-primary mode. A main already located beneath the root is
rebased to a guest path and opened with `openat2(RESOLVE_IN_ROOT)` so absolute
symlinks remain confined; an external authorized main is opened directly.
`PT_INTERP` is always opened beneath the root (and its ordered lowers), while
the rooted source used by later `exec` never accepts host fallback. This restores
the retained capability boundary without executable-name or fixture-specific
policy.
