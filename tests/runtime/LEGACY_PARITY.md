# Legacy runtime parity audit

This report mechanically compares the retained centralized C inventory with
the 36 direct-child `test.yaml` runtime categories. It describes migration
coverage, not engine correctness. The comparison was made on 2026-08-04 from
the shared working tree; `../engine` was read only.

## Oracle and method

The retained control flow studied was
`../engine/tools/matrix_runner.c::load_manifest`, `isa_servable`, and the case
execution/result path; `../engine/tools/compat_runner.c::run_one`; and the
registration functions `hl_guest_binary`, `hl_guest_suite`, and
`hl_compat_suite` in `../engine/cmake/Phase3Compat.cmake`. The row authorities
were:

- `legacy/inventory.tsv`: 846 executable `(suite, case, ISA)` rows;
- `legacy/build-plan.tsv`: 850 build rows, of which 840 join the executable
  inventory and 10 are build-only registrations;
- the corresponding manifests, sources, and expected output under
  `../engine/tests/compat` and `../engine/tests/soak`;
- all 36 YAML manifests, expanded through inherited targets and build fields.

The mechanical join maps `core/abi` to `abi_core`, `core/syscall` to
`syscall_core`, and `syscall` to `syscalls`; the other retained suite names map
to their same-named categories. `x86_64` maps to YAML target `amd64`. Source
and golden equality means SHA-256 equality of bytes, not matching filenames.
Host-conditional `excluded-macos` and unconditional YAML `!unsupported` are
reported separately because they are not the same execution policy.

## Executable inventory result

| Retained disposition | YAML result | Rows |
|---|---|---:|
| active | active, same identity and ISA | 700 |
| active | `!broken`, same identity and ISA | 74 |
| active | no same suite/case/ISA identity | 32 |
| excluded only on macOS | typed `!host-excluded [macos]` | 40 |
| **total** | | **846** |

Of the 814 identity-matched rows, 722 retain byte-identical sources. Another
86 use deliberately changed source: 82 network rows (41 cases on both ISAs)
replace the retained `net_util.h` include with category-owned
`socket_util.h`; `epoll-reblock-fin` and `pidfd-signal` in `syscalls` are
changed adapters on both ISAs. The remaining six bootstrap rows have no
retained source in the build plan.

All 808 non-bootstrap, identity-matched rows retain byte-identical golden
output. The six bootstrap inventory goldens were deleted with the old seed
tree, so their provenance is not mechanically comparable even though the YAML
rows have checked-in replacements. No identity-matched row changes its
expected exit status.

## Lost or renamed identities

These 32 active inventory rows have no same suite/case/ISA identity:

| Retained family | Cases | Rows | Current evidence |
|---|---|---:|---|
| `core/syscall` | `mmapshared`, `epoll`, `epoll-highfd`, `epoll-edge`, `epoll-dup-lifetime`, `epoll-fork-inherit`, `eventfd`, `eventfd-sema`, `signalfd-multi`, `inotify`, `timerfd`, `madvise`, `fallocate`, `statx-agree`, `clockelapsed` | 30 | `mmapshared`, `inotify`, `timerfd`, and `clockelapsed` have byte-identical sources under `syscalls`; the other eleven are absent or source-divergent |
| `soak` | `threadpool-aarch64`, `threadpool-x86` | 2 | intentionally folded into target-specific rows of `runtime/threadpool` |

The four byte-identical `core/syscall` moves account for eight rows, but they
remain identity divergences until an explicit alias or migration identity is
represented by the typed inventory gate. Similar case names elsewhere are not
counted as parity without source and golden evidence.

## Status divergences

The 74 retained-active rows now marked `!broken` are coherent families rather
than isolated omissions:

- all 21 `isolation` cases on both ISAs: 42 rows;
- 15 `syscall` cases on both ISAs: 30 rows (`aio-pread`, `aio-persist`,
  `aio-bad-opcode`, `epoll-pwait2`, `epoll-reblock-fin`, `fanotify`,
  `high-fd-emul`, `pidfd-signal`, `prctl-ltp`, `process-vm`, `getrandom-len`,
  `pipe2-badflag`, `pwritev2-rwf`, `modern-procfd`, and `iov-bounds`);
- `posix/chmodchown` on both ISAs: 2 rows.

The 40 retained `excluded-macos` rows now use typed
`!host-excluded [macos]` status, so they execute on Linux and Windows while
remaining inactive on macOS:

- network: `lo-any-bridge`, `oob`, `passcred-scm`, `so-type`,
  `socket-matrix`, `udp-msg-trunc`, `udp-switch`, `unix-seqpacket`;
- POSIX: `ctty-session`, `pty-ctl`, `tty-notty`, `tty-suspend`;
- soak: `reallocchurn`;
- syscall: `fcntl-cmds`, `memfd-seals`, `output-buffer-fault`,
  `output-buffer-fault2`, `output-buffer-fault3`, `pipe-size-dup3`,
  `seccomp-probe`.

Each name represents two ISA rows. This closes the former host-policy
divergence without changing source, build, golden, or exit contracts.

## Build-only rows

Ten retained build-plan rows never entered `legacy/inventory.tsv`:
`network/{linger-reset,tcp-pollhup}` and
`posix/{mq-notify-thread,pthread-cancel-async,tty-bg-deviation}` on both ISAs.
All ten have YAML identities and exact goldens, but all ten are now
`!broken`. The six POSIX sources are byte-identical. The four network sources
carry only the category helper-include change described above.

## Highest-value migration families

1. Port the complete 21-case isolation setup domain. Marking every row broken
   preserves names but supplies no executable compatibility evidence.
2. Restore the 15-case syscall failure cohort as a domain, starting from the
   retained syscall implementation and its complete call graph rather than
   fixture-by-fixture patches.
3. Resolve the remaining `core/syscall` case identities. Represent the four
   proven moves explicitly, then migrate or deliberately supersede the
   remaining sources and goldens.
4. Add the mechanical comparison as a typed repository gate before deleting
   the centralized inventory. YAML parsing alone cannot detect any divergence
   listed in this report.
