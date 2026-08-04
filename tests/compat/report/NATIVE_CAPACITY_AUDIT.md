# Native-capacity assertion reconciliation

Source authority: `../engine/tests/unit/test_native_capacity.c` at the current
working tree. The file contains exactly 103 `HL_CHECK` invocations. Assertion
numbers below are ordinal positions in source order; line numbers refer to that
C file.

## Current accounting

| Status | Assertions | Meaning |
|---|---:|---|
| mapped | 0 | Exercised through the same production host-service domain and operation |
| partial | 35 | A production Rust domain proves the shared invariant, but not the C assertion's opaque host-service operation |
| missing | 68 | No accepted production-equivalent assertion |
| total | 103 | Complete `HL_CHECK` inventory |

The mapped count is deliberately zero. The new provider reservation slice must
not be presented as native file/mapping/watch/event parity: it owns projected
provider identities and remote-close obligations, whereas the retained test calls
the concrete native `hl_host_services` tables.

The 35 partial assertions are:

- provider-file reserve/create ordering analogues: assertions 5 and 34 (C lines
  60 and 148);
- production raw-arena growth analogue: assertion 33 (C line 142). The
  `dynamic_capacity` source test keeps 4,098 `VirtualMemory` arenas live in one
  `HostResourceContext`, then drops them and observes the live count return to
  zero. This proves the retained growth axis without inventing a 4,098 limit,
  but it does not expose a C-compatible numeric mapping handle;
- wrong-kind rejection for provider-representable kinds: assertions 18, 21-27,
  30-31, and 80 (lines 108, 119-128, 133-134, and 256); network and shared-memory
  wrong-kind assertions 28-29 remain missing because they are not provider handle
  kinds;
- generation/reuse rejection analogues: assertions 40-41, 46-47, 55-56, 61-62,
  66-67, 72-73, 76, 83-84, 89-90, 95-96, 100, and 102 (lines 175-176, 189-190,
  206-207, 218-219, 228-229, 239-240, 244, 263-264, 276-277, 287-288, 295, and
  298).

All other assertion ordinals are missing. In particular, the current slices do
not cover native host construction/capability advertisement, C-ABI dynamic
registry growth, resource creation through that ABI, current-handle operations,
cross-fork survival,
timer/watch collections, reverse teardown, or native descriptor/mapping cleanup.

## Mapping handle reconciliation

Mappings are the highest-leverage next slice even though files have more ordinary
lifecycle assertions. The retained test creates 4,098 simultaneously live mapping
handles (one cross-mapping plus 4,097 loop mappings), validates wrong-kind
protection, preserves the last mapping across `fork`, releases in reverse order,
requires a replacement generation, rejects stale protect/release, and validates
the replacement. Its directly relevant assertions are 6, 23, 33, 44-49, and
101-102 (C lines 62, 122, 142, 183, 188-194, and 297-298).

Exact retained implementation entry points:

- Linux `src/host/linux/host.c`: `hl_linux_encode_handle`,
  `hl_linux_lookup_locked`, `hl_linux_allocate_handle`,
  `hl_linux_retire_mapping_locked`, `hl_linux_entry_holds_locked`,
  `hl_linux_memory_reserve`, `hl_linux_memory_protect`,
  `hl_linux_memory_release`, `hl_linux_memory_discard`,
  `hl_linux_memory_map_file`, and `hl_linux_memory_map_anonymous`;
- macOS `src/host/macos/host.c`: `hl_macos_handle`,
  `hl_macos_handle_index`, `hl_macos_lookup`,
  `hl_macos_register`, `hl_macos_mapping_fill`,
  `hl_macos_retire_mapping_locked`, `hl_macos_mapping_holds_locked`,
  `hl_macos_reserve`, `hl_macos_protect`, `hl_macos_release`,
  `hl_macos_discard`, `hl_macos_map_file`, and
  `hl_macos_map_anonymous`.

Rust ownership has two distinct layers:

- `hl-memory::MappingCoordinator` and its ledger own guest-visible VMA ranges,
  split/merge behavior, holes, backing offsets, and protection. This is address-space
  semantics and must not grow a second parallel VMA table.
- The retained capacity fixture exercises the native host-service typed handle
  table because C consumers need opaque numeric identities. That table owns
  dynamically grown slots, kind discrimination, nonzero generations,
  reserve/create/publish rollback, fork survival, stale-handle rejection, and
  reverse teardown for raw host resources.

`MAPPING_COUNT=4097` plus `cross_mapping` is a growth test, not an authoritative
4,098-resource cap. A fixed `mapping_capacity=4098` would invert the retained
contract. Rust's production arena consumer does not cross an opaque handle
boundary: it keeps a typed `Arc<VirtualMemory>`, and the arena keeps one
`HostResourceLease` whose boxed raw reservation performs the only `munmap`.
A second numeric registry here would duplicate ownership and manufacture
stale/wrong-kind states that Rust cannot express. A dynamically growing
generational table is required only if Rust later exposes the C
`hl_host_services` ABI, or another genuinely opaque heterogeneous handle
boundary. It is neither an arena limit nor a VMA identity.

`../engine/src/translator/arena.c` is not that registry. Its
`hl_arena_reserve`, `hl_arena_bind`, `hl_arena_repair`, and `hl_arena_release`
functions only consume the host memory service for translator code storage, bind
writable/executable aliases into emit state, repair them after fork, and release the
opaque host handle. The dynamically grown identity storage remains in the Linux and
macOS host backends. Rust app `VirtualMemory` similarly owns raw arena projection,
and both its local mapping ledger and `hl-memory::MappingCoordinator` own range
semantics rather than host-service handle identity.

The production injection boundary is now present. `GuestExecutor` owns one
`HostResourceContext`; initial
`VirtualMemory`, exec/fork arena recreation, and checkpoint restore all retain that
same context. `VirtualMemory::reserve_in` obtains a reservation before raw `mmap`
and publishes a lease that owns the raw guarded reservation only after creation
succeeds. Failed creation drops the empty reservation, while arena teardown drops
the published raw owner exactly once. The simultaneous 4,098-arena source test
moves only assertion 33 to partial; kinded/generational C ABI assertions remain
unmapped.

## Reservation and transaction capability map

| Lifecycle | Retained C owner | Rust production owner | Reconciliation |
|---|---|---|---|
| reserve/create | Linux creates raw VM then publishes a generated handle, compensating with `munmap` if publication fails; macOS and Windows reserve a handle placeholder and retire it on VM failure | `VirtualMemory::reserve_in` takes a context reservation, creates the guarded raw mapping, then publishes one RAII lease; failure drops the unpublished reservation | Equivalent create-before-publish rollback; different identity boundary |
| guest map/protect | A mapping handle owns native ranges and holes; fixed replacement retires overlapped handles | One raw arena remains reserved; `MappingCoordinator` stages VMA ledger changes and the adapter stages host changes, commits host first, then publishes the ledger | Equivalent transaction ordering; Rust deliberately separates arena ownership from VMA identity |
| fork | POSIX host fork copies the backend registry and mappings; child repair resets unsafe synchronization state and repairs code aliases | `AddressSpace::fork_bounded` creates a separate arena in the same context, snapshots/copies mappings, and publishes the child only after the complete task fork commits | Equivalent unpublished-child cleanup through `Arc` drop; not a host-fork table-copy implementation |
| exec | Linux ABI drops the prior guest mapping registry while retained host services release its mappings | `Spaces::create` builds a fresh arena; `PreparedProcessImage::publish` swaps the complete image, rollback restores the retained previous `Arc`, and finish releases it | Rust has explicit reversible ownership across the multi-participant exec transaction |
| partial unmap | Backend unmaps the requested subrange and records holes; full coverage consumes the handle | Coordinator publishes split/removed VMA ranges only after host commit; the host adapter returns the subrange to `PROT_NONE` while preserving the containing raw arena | Same guest result, intentionally different raw-VM lifetime |
| release/exit | Release unmaps held ranges and retires the generated handle; host destroy scans dynamic tables in reverse dependency order | Final arena `Arc` drop drops `HostResourceLease`, which drops the raw reservation before decrementing the instance live count; staged mapping exit also removes logical mappings | Exact single raw-owner teardown for arenas; heterogeneous reverse-order host-service teardown remains a separate future ABI concern |
| checkpoint restore | C resets and reloads mappings into the process host context | Restore constructs and populates a fresh arena; `SpaceTransaction::commit` replaces the current image, rollback reinstalls the retained old lease, and an unpublished replacement drops normally | Reversible arena ownership is present; no charge/account state is restored |
| capacity and identity | Initial 4,096 storage grows dynamically; numeric handles require kind and generation validation | No slot table or fixed cap; typed `Arc` reachability is identity and stale/wrong-kind use is structurally unavailable | The 4,098 test is a growth proof, not a configured capacity |

Windows follows the same retained C publication invariant with a unified dynamic
kinded table and per-mapping region records, although fixed overlap replacement is
documented as non-atomic and it has no POSIX host-fork path. Translator
`arena.c` remains only a memory-service consumer: reserve/bind/repair/release do
not own capacity storage.

One Rust exit caveat remains explicit: `PreparedMappingExit::finish` discards the
result of committing its staged guest unmaps. Terminal `Arc` teardown still drops
the whole raw arena, so this cannot leak that reservation, but a host commit
failure is not reported and logical teardown diagnostics are lost.

## Host reservation capacity is not guest memory accounting

The C engine's guest limit is a separate Linux-ABI policy. `container/state.c`
maintains `g_mem_max`, a process-local atomic charge, and a 1,024-slot shared
container accounting table. `syscall/mem.c` charges anonymous, non-`MAP_NORESERVE`
`mmap` and positive `brk` growth and refunds their immediate failures. That code
must not be copied as a complete oracle: `munmap` subtracts the requested length
without charged-range provenance; fixed replacement cannot compute the charge it
replaced; `mremap` does not adjust charge; and exec/checkpoint restore do not
reset or restore the charge. C fork baselines inherited charge so it is not
counted again in the child slot. Host physical-memory sampling and procfs/cgroup
projection are independent again.

Rust currently has no enforcing equivalent. `HostResourceContext::live` counts
raw resource owners, not bytes. `SystemAuthority` holds a static
`ResourceSnapshot`; composition copies `HL_MEM_MAX` into total/free reporting but
mapping, `brk`, `munmap`, `mremap`, exec, exit, fork, and checkpoint do not reserve
or refund it. `MmapPlan` also accepts but does not retain `MAP_NORESERVE`, so a
correct charging decision cannot yet survive planning.

A coherent port needs an instance-scoped byte budget plus per-address-space
charged-range provenance. The mapping transaction must reserve the exact positive
before/after delta before host commit, publish charge and VMA state together, and
refund on every rollback; fixed replacement, split unmap, remap, exec, exit, fork,
and checkpoint must all carry that provenance. Fork policy needs an explicit
decision because C baselines inherited copy-on-write memory while Rust currently
copies private contents into a new arena. Adding only an atomic byte counter would
double-charge or leak across these transitions and is therefore not a valid
partial implementation.
