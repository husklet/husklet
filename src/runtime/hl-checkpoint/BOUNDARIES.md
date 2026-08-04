# Checkpoint boundary

## Retained C oracle audit

The retained implementation studied for this production wiring lane is
`../engine/src/linux_abi/checkpoint.c` (`ckpt_poll`, `ckpt_control_init`,
`ckpt_coordinate_and_exit`, `ckpt_dump_self`, `ckpt_dump_self_locked`,
`ckpt_restore_preflight`, `ckpt_restore_proc_run`, `ckpt_fork_children`, and
`ckpt_restore_tree`), `../engine/src/linux_abi/ckpt_sink.h`,
`../engine/src/linux_abi/ckpt_sink_stream.h`,
`../engine/src/linux_abi/ckpt_source.h`, and
`../engine/src/core/checkpoint_channel.c` (`hl_ckpt_channel_acquire`,
`hl_ckpt_channel_call`, trigger creation, and broker acceptance).

The C engine owns one private broker stream per host process, reacquires it
after `fork`, and shares only the generation trigger. The guest init is the
tree coordinator. It interrupts peers, waits for each process-owned group,
captures its own CPU, address space, descriptors, signals, and identity last,
then publishes `MANIFEST`. Object finish and group commit are visibility
boundaries; any partial write, peer failure, timeout, or transport close aborts
the object/group and prevents manifest publication. Restore validates the
manifest, architecture, process topology, digest, and every recoverable resource
before mutating live state. It prepares shared objects before rebuilding the
tree in parent order and resumes only after memory, descriptors, CPUs, signals,
process groups, and terminal state are installed. POSIX hosts use `SCM_RIGHTS`,
per-process Unix streams, a shared mapped trigger, safepoint interruption, and
real host forks; Windows rejects this transport as a whole. Guest ISA branches
select the CPU image architecture and restoration implementation.

The Rust ownership mapping is deliberately not a C image-format port:

| C capability | Rust owner | Status in this lane |
|---|---|---|
| transactional object/group store and final manifest | `hl-checkpoint` whole-image writer plus `hl-container::CheckpointImage` | implemented as one bounded Rust image object committed by a Rust-only manifest |
| freeze, ordered domain capture, rollback, thaw | `hl-runtime::RuntimeCheckpointCoordinator` | implemented |
| task-tree capture and refork restore | task/execution/fork participants | multi-process production transport remains unsupported |
| memory, descriptor, provider, event, network, IPC, execution state | sibling runtime participants | wired for the production single-process assembly; unsupported resources reject during participant capture/validation |
| pre-mutation topology/resource admission | `RuntimeAssembly::preflight_checkpoint` | single-process/single-thread and no-fork assembly admission implemented |
| C object names and `HLCKPT07`/`HLMAN007` format | retained C only | intentionally incompatible; no cross-format claim |

The container adapter never interprets runtime sections. It stores the canonical
Rust whole-image bytes, commits a small `HLRUST01` length manifest last, and
requires that manifest before restore. The runtime image reader independently
verifies its own section bounds and digest.
