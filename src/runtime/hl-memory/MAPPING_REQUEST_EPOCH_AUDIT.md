# Mapping-request epoch audit

This audit is pinned to Husklet `cf15cdd33` and the read-only retained engine
at `/Users/x/dd/engine`. It covers the memory-owned prerequisite for extending
an x86 REP activation across a public run boundary; it does not claim that the
scheduler, timer/accounting, or native callback prerequisites are complete.

## Retained implementation studied

- `src/linux_abi/thread.c`: `gbus_mapping_transition_lock`,
  `gbus_mapping_stw_begin`, `gbus_mapping_stw_end`,
  `gbus_mapping_transition_unlock`, `gbus_mapping_prepare`,
  `gbus_mapping_prepare_release`, `hl_linux_bus_transition_begin`, and
  `hl_linux_bus_transition_end`.
- `src/linux_abi/syscall/mem.c`: the complete `service_memory` cases for
  `munmap` (215), `mremap` (216), `mmap` (222), and `mprotect` (226), including
  every prepare/abort/commit/unlock path and the Darwin coarse-host-page and
  non-PIE logical-mapping branches.
- `src/linux_abi/syscall/signal.c`: the non-PIE alternate-stack direct-mapping
  transaction in `service_signal`.
- `src/translator/guest/x86_64/translate.c`: executable-range replacement
  invalidation reached from the memory syscall paths.

The retained transition identity is process-global logical mapping state.
`g_bus_transition` owns serialization and its begin/end callbacks stop and
release translated peers around affected logical ledgers. The caller owns the
lock lifetime from prepare through host mutation and logical publication;
every failure releases prepared state and the transition lock. Fork repair is
installed through `g_bus_atfork_once`. The state is torn down/reset with the
process image. The required order is transition lock, translation flush,
mapping stop-the-world, then logical ledger. Host-specific branches change
physical mapping mechanics (especially Darwin's larger host granule), but not
that ordering. Invalid arguments return before mutation; partial host effects
are reconciled before unlock. Signal delivery does not cancel a transaction
mid-publication.

Husklet's owner is one `hl_memory::MappingCoordinator` per address space.
`transaction: Mutex<()>` spans host projection or staging through ledger and
executable publication. `CheckpointActivity` owns admission/exit lifetime.
The new saturating `mapping_requests` epoch has the same coordinator lifetime,
is published before a mutator waits for `transaction`, and is only observed
with acquire ordering. Saturation fails closed permanently. A
`ProjectionLease` captures the epoch only after it owns the transaction; its
`RequestContinuation` retains the atomic owner without retaining host pointers.
Dropping the lease releases all write reservations, host projections, mapping
transaction authority, and checkpoint admission.

## Capability matrix

| Capability | Retained owner | Rust owner | Result |
| --- | --- | --- | --- |
| Serialize map/unmap/protect | `mem.c` + `g_bus_transition` | `Coordinator::{map,unmap,protect}` | implemented; request publishes before lock |
| Atomic mapping batch | logical prepare/commit paths | `Coordinator::apply` | implemented; one request per batch |
| Remap move/copy/shrink/grow | `service_memory` case 216 | `Coordinator::{remap,remap_charged}` | implemented; request precedes transaction |
| External backing repair | retained file/logical registries | `Coordinator::backing_changed` | implemented; all aliases share one request and transaction |
| Host/external writes | syscall copy and executable invalidation paths | `write_vectors`, `external_write`, `commit_write`, `commit_write_spans` | implemented; request covers queued publication |
| Read-only host operation | retained stable mapping read | `read_vectors`, `external_read` | unchanged; does not spuriously invalidate |
| Direct projection admission | translated peer between STW boundaries | `project_contiguous`/`project_direct` | implemented; capture occurs after lock admission |
| Checkpoint transaction ingress | retained snapshot stop-the-world | `with_frozen_snapshot`, `freeze_checkpoint` | implemented; checkpoint epoch denies while admissions drain, mapping epoch publishes before its transaction wait |
| Exit/termination | process-image teardown | `prepare_exit` + `CheckpointActivity` | implemented by checkpoint continuation; no second transaction wait exists |
| Saturation/ABA safety | retained generation publication | `RequestContinuation` | implemented; `u64::MAX` never grants |
| Scheduler, signal, cancellation, timer, and accounting gates | retained scheduler/task state | other Rust domains | intentionally still missing; this token grants no execution by itself |

Focused tests prove a queued unmap invalidates before it can acquire the live
projection transaction, the transaction remains blocked until lease release,
and saturation denies permanently. The full `hl-memory` library suite covers
the remaining mutation entry points, failure rollback, checkpoint, exit,
projection, executable alias, external prefix, remap, and batch behavior.
