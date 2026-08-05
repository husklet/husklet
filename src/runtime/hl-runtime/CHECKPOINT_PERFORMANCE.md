# Checkpoint composition performance audit

The retained C engine under `../engine` was inspected read-only.

## Retained implementation and ownership

- `src/linux_abi/checkpoint.c`: generation-triggered safepoint capture,
  process-tree coordination, per-process metadata/CPU/page/descriptor capture,
  sparse memory encoding, manifest-last publication, restore ordering,
  descriptor/OFD reconstruction, signal state, epoll graphs, robust/futex
  state, and rollback cleanup.
- `src/core/checkpoint_channel.c`: bounded request/reply stream ownership and
  exact partial-I/O progression for object/group publication.
- `src/linux_abi/container/snapshot.c`: monotonic guest snapshot-address
  reservation and overflow/alignment bounds.
- `src/linux_abi/sentry_snapshot.h`: sentry-side checkpoint boundary used by
  the process coordinator.

The C coordinator freezes at guest safepoints, captures each process's owned
state once, writes the manifest only after every group commits, and restores
parents before children. Memory is sparse and pointer-free; CPU host-transient
fields and translation caches are not persisted. Descriptor records retain
descriptor flags, shared OFD identity/offset, and type-specific bounded state.
Architecture branches select CPU layout and event wire layout. Host branches
reconstruct kqueue/handles and path-backed resources. Partial stream operations
retry or fail transactionally; malformed lengths, missing groups, unsupported
resources, and pre-publication failures cannot expose a partial checkpoint.

Rust `RuntimeCheckpointCoordinator` owns ordered freeze, capture, whole-image
digest publication, thaw, validate, stage, commit, rollback, resume, and finish.
Task, descriptor, memory, provider, event, network, IPC, and execution domains
own their bounded pointer-free sections. Descriptor restore preserves numeric
generations, OFD identity, aliases, offsets, flags, transfer roots, and object
codec ownership.

## Finding and change

The coordinator deliberately validates every section before staging any domain.
The descriptor participant decoded and fully validated its bounded image during
that phase, discarded it, then decoded and validated the same bytes again in
`stage`; `DescriptorTable::restore_checkpoint` performs its own invariant check
before publication as well. Large descriptor images therefore paid for an
avoidable full decode, object-byte copy, and ordered-set validation.

The descriptor participant now retains at most one decoded image, keyed by the
canonical whole-image SHA-256 digest. `stage_bound` consumes it only for the
same digest; standalone staging and any mismatch decode normally. The cache is
bounded by the existing section limit and is replaced by the next validation.
Ordering, pointer-free representation, validation-before-mutation, rollback,
descriptor/OFD identity, resource bounds, and stage/commit semantics are
unchanged.

## Exact A/B evidence

Base and candidate derive from `e0358aa57` in isolated worktrees. Fixture setup
and image construction are excluded. Each timed iteration performs descriptor
validate, bound stage, and rollback. Seven alternating pairs:

```text
2 descriptors, base ns:      1097 969 965 977 1076 1137 1082
2 descriptors, candidate ns: 1409 1083 955 859 1002 1001 857
median:                       1076 -> 1001 ns (-7.0%)

4096 descriptors, base ns:      4383447 3955708 4072260 4147562 3764604 3714109 3682432
4096 descriptors, candidate ns: 3276494 2842104 2886442 2984375 3097734 2642937 2621468
median:                          3955708 -> 2886442 ns (-27.0%, 1.37x)
```

The small-image result is effectively neutral; the many-resource checkpoint
shows the eliminated decode/validation pass. Native/C wall time is not directly
comparable to this composition-only diagnostic because the retained format and
resource model differ; both retain validate-before-publication ordering.
