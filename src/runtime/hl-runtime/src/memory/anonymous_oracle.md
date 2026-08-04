# Anonymous memory accounting oracle

Retained implementation inspected in `../engine`:

- `src/linux_abi/syscall/mem.c`: `svc_mem`, cases 214 (`brk`), 215
  (`munmap`), 216 (`mremap`), and 222 (`mmap`).
- `src/linux_abi/container/state.c`: `g_mem_max`, `g_mem_charged`,
  `acct_container_reset`, `acct_after_fork`, `acct_publish_mem`,
  `acct_proc_leave`, and `acct_mem_total`.
- `src/linux_abi/syscall/proc.c`: `fork_child_hooks`, `exit_group`, and the
  exec mapping reset/replacement path.
- `src/linux_abi/checkpoint.c`: mapping and brk snapshot/restore paths.
- `src/linux_abi/container/vfs.c`: `memory.max`, `memory.current`, and
  `/proc/meminfo` projections.
- `src/core/target/aarch64.c` and `src/core/target/x86_64.c`: `HL_MEM_MAX`
  capture and initial heap construction.

The retained mmap path reserves the byte-exact guest length before an
anonymous mapping, excludes `MAP_NORESERVE`, returns `ENOMEM` at the limit,
and refunds a failed host mapping. Its wider lifecycle is negative evidence:
`MAP_FIXED` does not refund replaced coverage, `munmap` refunds the requested
length across holes and file mappings, every `mremap` mode bypasses accounting,
exec and checkpoint omit charge reconstruction, and enforcement is
process-local while `memory.current` is container-aggregate.

The Rust owner therefore keeps byte-exact reservation provenance in each
`hl-memory::Region`, committed in the same mapping-ledger generation as map,
fixed replacement, unmap, and remap. Checkpoint wire data carries that
provenance. `hl-runtime::AnonymousMemoryLease` owns only the aggregate
contribution of one address space to the container account, avoiding a second
range ledger and lock-order inversion.

Rust fork policy intentionally differs from the retained host-fork baseline.
`AddressSpace::fork_bounded` allocates a distinct arena and copies private
bytes, so the child reserves its complete copied charge before fork
publication. A failed reservation rejects the fork; dropping either address
space releases only that address space's contribution. This matches the Rust
mechanism rather than pretending its deep copy is retained-C COW.

## Capability matrix

| Capability | Rust owner | Status |
|---|---|---|
| Exact anonymous `mmap`, replacement, and `munmap` charge provenance | `hl-memory::Region` and mapping transactions | Implemented; both guest ISAs covered |
| In-place and moved `mremap` accounting | `hl-runtime::RuntimeMemorySyscalls` | Implemented; growth, shrink, fixed move, `DONTUNMAP`, limit, and rollback tests pass |
| Shared `brk`/`mmap` account and exec retirement | `hl-runtime::BrkRegion` and `AnonymousMemoryLease` | Implemented; byte-exact transition and release tests pass |
| Checkpoint wire preservation of charge provenance | `hl-memory::MemoryCheckpointImage` and `hl-runtime::PortableMemoryCodec` | Implemented as checkpoint/image wire version 2; version 1 is rejected |
| Checkpoint replacement of the live syscall coordinator, lease, and break state | `MemoryCheckpointParticipant` plus the engine address-space transaction | Missing: the host swaps the arena/coordinator, but `RuntimeMemorySyscalls` retains its old coordinator and `BrkRegion`; the break snapshot is not in the checkpoint payload |
| Reserve child aggregate charge before private-byte fork copy | engine `AddressSpace::fork_bounded` and runtime fork composition | Missing: copied mapping provenance is installed first and `BrkRegion::fork` reserves the aggregate account later |

The two missing lifecycle rows are load-bearing. A mapping-ledger round trip is
not sufficient checkpoint evidence until the syscall owner is transactionally
rebound, and a fork that eventually accounts the child is not evidence that a
limit rejection occurs before the potentially large private-memory copy.
