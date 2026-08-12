# Exec compatibility oracle audit

> **Historical ownership:** Rust-engine ownership statements below are retained
> migration evidence. Exec cases now exercise the selected C production engine.

## Retained implementation studied

The retained engine was read only. `../engine/src/linux_abi/syscall/proc.c`
was followed through `exec_forward_env`, `exec_close_cloexec`, canonical
`execveat` case 281, and the shared `execve` case 221. This includes dirfd and
`AT_EMPTY_PATH` resolution, flag and pointer admission, script rewriting,
ELF error classification, argument/environment capture, the commit point,
CLOEXEC sweep, SysV detach, mapping/JIT reset, signal-disposition reset, sibling
thread retirement, loader publication, and old allocation teardown.

The audit also covered `../engine/src/linux_abi/syscall/fs.c::fd_reset_emul`
for descriptor close bookkeeping; `../engine/src/linux_abi/fork.c::loaded_span`,
`fsrv_restore_prep`, `fsrv_restore_done`, `fsrv_restore_pristine`, and
`hl_forkserver_runner` for ELF state reconstruction and execution-cache epoch
handling; and `../engine/src/linux_abi/number.c::x86_number` and
`hl_linux_syscall_number` for x86-64-to-canonical syscall mapping.

## Ownership and ordering

Before the commit point, the process retains its old executable image,
descriptors, mappings, signals, robust list, and threads. Path/ELF/script and
argument failures return the precise Linux errno without changing that state.
After validation, exec is irreversible: sibling tasks retire, CLOEXEC
descriptors close while non-CLOEXEC OFDs survive, caught handlers reset while
ignored signals remain ignored, SysV attachments and robust state clear,
mapping and translated-code state is replaced, and the new initial thread is
published. Descriptor enumeration and filesystem resolution are bounded; no
table lock is held across loader or host filesystem work. Exec has no partial
success result and cancellation is resolved before publication.

Guest-ISA differences are restricted to syscall numbering and ELF machine
selection. Host branches implement descriptor enumeration and executable
mapping, but guest paths, addresses, errno, and lifecycle remain Linux values.

## C-to-Rust capability matrix

| Retained capability | Rust owner | Case evidence |
|---|---|---|
| transactional executable staging and ELF/script admission | `hl-loader` and `hl-runtime::exec` | `exec-edges` |
| dirfd, empty-path, and final-component nofollow policy | `hl-runtime::process::execveat` plus filesystem path port | `execveat-edges` |
| task commit, sibling retirement, signal and robust reset | `hl-task::PreparedTaskExec` and runtime process composition | `exec-edges` |
| descriptor-local CLOEXEC sweep with surviving OFDs | `hl-descriptor::DescriptorTable::close_on_exec` and runtime epoll/descriptor exec adapters | `exec-edges` |
| mapping and execution-image replacement | `hl-memory`, `hl-execution`, and runtime exec port | both cases |
| exact errno without old-image mutation | typed loader/path errors converted at the Linux boundary | both cases |

The two cases preserve the complete source-owned local exec cohort on both
guest ISAs. They do not claim dynamic-interpreter breadth, set-id/capability
transitions, checkpoint interaction, or every argument-size limit.

## Migration evidence

The canonical source and golden bytes are exact copies of both former legacy
owners, `legacy/local/tests/compat/exec` and
`legacy/oracle/tests/compat/exec`; both legacy categories and their inventory
rows are removed. Direct typed engine runs passed both cases on ARM64 and AMD64
with `native: true` and `diagnostics: true`, producing `hl-native` and
`hl-native-detail` proof on each ISA.

The QEMU oracle passed `execveat-edges` on both ISAs. `exec-edges` exited 1 on
both QEMU providers because its robust-list and clear-tid observations differ
under user-mode QEMU. That host-oracle limitation does not mark the engine case
broken: the Rust engine passed the checked golden on both guest ISAs, consistent
with the retained engine corpus results.
