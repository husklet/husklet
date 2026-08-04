# Loader boundary

`hl-loader` validates ELF64 program headers and constructs an initial Linux
process image. Its policy is determined by ELF type, segment attributes,
architecture, requested placement, and bounded resource limits. Application or
language-runtime identity is never an input.

## Retained-C mapping

| Generic Rust behavior | Retained C oracle | Mapping |
|---|---|---|
| Prefer an `ET_EXEC` link address without replacement, then fall back only on collision | `src/linux_abi/thread.c`: `nonpie_place_at_link_address`; `src/linux_abi/elf.c` and `x86.c`: `load_elf` | `ExecutablePlacement::PreferLink` and `Loader::reserve_main` |
| Keep Linux-visible `ET_EXEC` entry, program-header, and pointer values in link coordinates | `src/linux_abi/elf.c`: low `entry`/`phdr`; `src/linux_abi/thread.c`: non-PIE coordinate contract | `Loader::guest_bias` returns zero for `ET_EXEC`; stack and handoff retain link values |
| Project only an in-range guest address to displaced private storage | `src/linux_abi/thread.c`: `nonpie_fold`/`nonpie_unfold` | `ImageProjection::{storage_address,guest_address}` |
| Copy `PT_LOAD`, zero BSS, then apply final protections transactionally | `src/linux_abi/elf.c` and `x86.c`: load-segment population and protection | `Loader::stage_image` plus `ImageProtectionPlan` |

## Deliberately rejected C policy

The following retained-C branches are compatibility debt, not Linux ELF
semantics, and are not migrated:

- `src/linux_abi/goimage.h`: Go build-info detection and SIGURG policy;
- `src/linux_abi/x86.c`: `go_find_moduledata` and `go_rebase_nonpie`;
- `src/linux_abi/x86.c`: `g_nonpie_types_lo/hi` and
  `g_nonpie_blob_code`/V8 immediate rewriting;
- the `.data`/`.data.rel.ro` qword scan that guesses which words are pointers.

Those branches mutate canonical guest values or classify particular programs.
Any workload that still depends on them exposes a missing generic projection at
the execution, syscall, signal, fault, or memory boundary. The fix belongs at
that boundary and must be expressed in Linux/ELF terms.

## Ownership

- Loader owns ELF validation, placement plans, image population, initial stack,
  TLS templates, dynamic-loader handoff, protections, and image projection.
- Memory owns storage mappings and protection enforcement.
- Execution and syscall adapters consume `ImageProjection` when accessing the
  storage of a displaced executable.
- Task/signal owns delivery semantics and may not request loader classification
  of a guest runtime.

Regression fixtures may contain strings or section names associated with real
applications, but tests must assert that they are semantically irrelevant.
