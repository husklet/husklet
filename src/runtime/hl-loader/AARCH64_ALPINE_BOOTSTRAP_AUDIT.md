# AArch64 Alpine bootstrap ELF/exec audit

Audit tree: `8172069ef3f54606611a33a353d8237d51b4b6e7`. Retained oracle files read in full:
`../engine/src/linux_abi/elf.c` (`elf_interp`, `load_elf`, `build_stack`, mapping/protection and fault guards) and
`../engine/src/linux_abi/syscall/proc.c` (exec argument/environment copying, `exec_forward_env`, `sys_execve`,
thread retirement, descriptor close-on-exec, signal reset, mapping teardown, and image re-entry). The oracle is
process-global C state: image/stack/mapping registries live for an image and are replaced at exec; exec serializes
through the process/thread lifecycle, validates guest strings before destructive teardown, closes CLOEXEC
descriptors, retires sibling threads, resets caught signals, destroys old mappings, loads main plus `PT_INTERP`,
constructs argv/env/auxv, and transfers control. Failures before commit return Linux errno; failures after teardown
terminate the image. AArch64 requires `EM_AARCH64`, 4 KiB guest-page ELF coordinates, AArch64 auxv/platform values,
and architecture stack ordering. Host branches concern fixed ET_EXEC placement, host-page protection granularity,
and signal-context repair; none changes guest-visible ELF addresses.

| Oracle capability | Rust owner | Status |
|---|---|---|
| bounded image read, ELF identity/class/endian/machine validation | `hl-loader::ElfInspector` | implemented |
| complete `PT_LOAD` span, file copy, BSS zeroing, overflow/bounds checks | `hl-loader::{ImagePlan,Loader}` | implemented |
| main ET_EXEC fixed placement and PIE/interpreter bias | `hl-loader::{LoadPolicy,Loader,DynamicLoaderHandoff}` | implemented |
| `PT_INTERP` resolution and second image load | `hl-loader::Loader::prepare_interpreter` | implemented |
| segment permissions with guest/host-page projection | `hl-loader::{ImageProtectionPlan,GuestProtectionPlan}` | implemented |
| atomic mapping publication/rollback | `hl-loader::MappingTransaction` + engine loader adapter | stronger than oracle |
| guarded stack, argv/env strings and Linux auxv including `AT_EXECFN` | `hl-loader::{StackPlanner,StackLayout}` | implemented |
| architecture TLS and dynamic-loader handoff | `hl-loader::{tls,handoff}` | implemented |
| guest pathname/shebang/argv/env bounded capture | `hl-runtime::exec::source` + `hl-linux::ExecPlan` | implemented |
| exec cross-domain prepare/publish/rollback ordering | `hl-runtime::exec::{SafeRuntimeExec,Runtime}` | implemented |
| CLOEXEC, sibling retirement, signal reset, IPC teardown | runtime exec participants | implemented by participant contract; production wiring requires evidence |
| post-load register/TLS installation and first instruction | `hl-engine` execution composition | implemented structurally; failing production launch shows an unresolved gap |

Fail-first production evidence used the flake-pinned
`alpine-minirootfs-3.24.1-aarch64.tar.gz`, SHA-256
`f55a90f69052c5bd6f92cb09a8f47065970830b194c917a006fb94028e721259`, through
`Containers` and the non-fake Rust engine. Command:
`nix develop -c cargo test -p hl-container --test run_options process_run_options -- --ignored --nocapture`.
The first `runflags/env-e` execution completes as `Fault { status: -1, detail: 0 }` with empty output in about
0.17 seconds. The failure occurs before `hl-container::engine::Process::wait` obtains an `EngineExit` (temporary
instrumentation there emitted nothing), so the public status is the container supervisor's error fallback, not an
ELF execution fault diagnostic. This bound did not establish which construction/start error was discarded; changing
loader semantics would therefore be speculative. The next lane should expose the stored supervisor failure (or retain
the engine construction/start error in container state), then repair the identified owning invariant.
