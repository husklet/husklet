# Network checkpoint authority oracle

Retained implementation inspected in `../engine`:

- `src/linux_abi/checkpoint.c`: `ckpt_poll`, `ckpt_dump_self`,
  `ckpt_dump_self_locked`, `ckpt_scan_fds`, `ckpt_capture_socket_state`,
  `ckpt_coordinate_and_exit`, `ckpt_prepare_restore_sockets`, and
  `ckpt_prepare_restore_socket_states`.
- `src/translator/cache.c`: `stw_checkpoint_arm`, `stw_checkpoint_wait`,
  `stw_checkpoint_cpus`, and `stw_checkpoint_end`.
- `src/core/checkpoint_channel.c`: `hl_ckpt_channel_acquire` and the
  per-process broker/channel ownership inherited across `fork`.
- `include/hl/activation.h`: process-handle wait, destroy, and process-domain
  snapshot contracts.

The retained engine owns checkpoint initiation through a shared generation
trigger. Each live process reaches a translator safepoint, arms a stop-the-world
barrier, snapshots its registered CPU records, and scans its guest-visible
descriptor table while peers remain parked. Descriptor and socket state is
captured only for objects found by that scan. An empty descriptor/network set
does not require a separate network transaction. The process group is staged
and committed atomically; any scan, socket, or write failure aborts the group.
The container-init coordinator interrupts descendants, waits for every staged
process group, dumps itself last, and publishes the manifest last. Teardown is
process-based: successful participants exit, parent-side wait caches completion,
and destroying a live activation handle terminates and reaps it.

The retained synchronization is POSIX-host-specific. The trigger is shared
`mmap` state, the broker uses Unix sockets plus `SCM_RIGHTS`, and checkpoint
thread entry reconstructs state from POSIX signal/ucontext safepoints. The
Windows channel entry points explicitly reject the feature. Guest ISA only
changes the CPU image and safepoint machinery; descriptor/socket ordering is
shared.

## Capability matrix

| Retained capability | Rust owner | Status |
|---|---|---|
| Freeze all runtime participants before snapshot | `RuntimeCheckpointCoordinator` and participant `freeze`/`thaw` | Implemented |
| Snapshot network state only when the catalog contains it | `hl-network::NetworkCatalog::checkpoint_image` | Implemented |
| Capture an empty or internally reconstructable network without an external socket-retention broker | engine `network::Native` checkpoint host | Implemented by making the external authority optional when no retained authority lease is present |
| Preserve live listening sockets through an external owner | `AuthorityWorker` network transaction and `ObjectBindings` | Implemented; capture still fails closed when a listener needs retention but no authority exists |
| Restore created/bound sockets without a retained external descriptor | `Native::recreate` | Implemented; an authority-less restore rejects images containing retained authority leases |
| Capture a live process concurrently with execution | Historical Rust lifecycle/composition oracle | Historical gap; the retired `RustRuntimeMachine::start` was synchronous and returned only after execution reached a terminal result |
| Whole-process-tree checkpoint coordination | Production C lifecycle/composition | Missing |

The `network_checkpoint_role` regression was not an exit race: the synchronous
start had already cached the guest exit before capture. Capture failed in the
network participant's prepare phase because the default executor has no
`AuthorityWorker`, even though its network catalog had no externally retained
socket. The fix preserves the authority requirement exactly where identity
cannot be reconstructed: retained listeners and images carrying authority
leases.
