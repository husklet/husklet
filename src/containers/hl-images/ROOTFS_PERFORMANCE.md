# Rootfs and image performance checkpoint

## 2026-08-04 exact-tree evidence

The measured Husklet tree was `eb14c27367d4e38af339bd5567de799fe9a73b04`.
The warning-strict release artifacts were:

| artifact | SHA-256 |
|---|---|
| `testing` | `67d199a047c7dc412798af72845cdd4270cf974d325d324f7934ed05c8207e1d` |
| Rust `hl-engine` | `4891d0bd9ab0b781c8d6625e98af05ad80cd88ee84c296cb4f3107d72056a82e` |
| ARM64 guest | `a8f403013de972313b6c4f3450a82f5e7222690cf05610636155b0c56526d5f9` |
| retained C runner | `0633ed0f914f666f2127ca1b86a4def69eea65c59f7942f8afd66d2b7a6ebc62` |

The retained source oracle was read at `../engine` revision
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. The audit covered
`tools/matrix_runner.c::{stage_rootfs,open_case_workspace,run_case,remove_rootfs}`,
`linux_abi/container/vfs.c::{secure_resolve_probe,jail_match}`, and
`linux_abi/container/vfs/overlay.c::{overlay_lookup_raw,overlay_resolve}`.
The runner owns one disposable staged root per case. The engine pins the root and
bind parents for one process lineage. Overlay reads probe the upper then lowers,
cache only epoch-valid rootfs results, exclude host-mutable volume routes, and
invalidate resolution state on copy-up or namespace change. C does not implement
the durable OCI snapshot, lease, publication, or garbage-collection contract.

The Rust audit covered `TestImage::{resolve_identity,materialize}`,
`Images::rootfs`, `Roots::{fork,fork_overlay,open}`, and
`Snapshots::{prepare,Draft::commit_with}` through `Tree::{copy_to,sync}`. A
normal writable root currently reflinks or copies the complete immutable tree,
preserves hard-link identity and permissions, recursively synchronizes every
entry, atomically publishes the snapshot, and creates durable leases. The
operation lock spans this transaction. `fork_overlay` avoids the copy, but the
Rust execution engine does not yet implement overlay lookup and copy-up, so that
path is not a valid production substitute.

One uncontended ARM64 `syscall` benchmark row reported these separated setup
costs:

| phase | time |
|---|---:|
| first image identity resolution | 6,280,429 us |
| execution root materialization | 313,657 us |
| execution root release | 42,505 us |
| container service construction | 4,966 us |
| cold create | 1,715 us |
| cold start | 6,114 us |

Warm create was 2,816 us median and warm start was 4,115 us median over three
samples. The first identity resolution includes cold registry/image-store work
and is not a steady-state open measurement.

A CPU-17, seven-repeat direct matrix separated provider startup from guest work.
For a small syscall phase, median wall time was native 13,707 us, retained C
13,540 us, and Rust 11,255 us. This proves that bare Rust provider startup is not
the 313 ms rootfs cost; the durable root fork is outside this direct runner.

The pathname/file cohort cannot yet support a performance ratio. Native and C
both returned checksum 400, at 447 us and 382 us median respectively. Rust
returned checksum **0** at 62 us. Treating that shorter time as an optimization
would reward skipped filesystem work. This is the same correctness boundary
that prevents using the empty-overlay fast path to replace the durable fork.

No production optimization is justified from this checkpoint. The next coherent
domain is overlay lookup, copy-up, whiteout/opaque handling, exact bind exclusion,
epoch invalidation, and teardown. After that domain passes rootfs-write accounting
and the pathname cohort, benchmark `fork_overlay` against the current 313 ms
materialization baseline. Weakening recursive durability or sharing one writable
root between cases is not an acceptable benchmark shortcut.

## Ordinary mount activation after overlay support

The retained C oracle remains revision
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. This follow-up read
`tools/matrix_runner.c::{stage_rootfs,open_case_workspace,run_guest,remove_rootfs}`
and
`src/linux_abi/container/vfs/overlay.c::{overlay_lookup_raw,overlay_resolve,overlay_copyup,overlay_copyup_tree}`.
The matrix runner creates and tears down one small private root per case. The C
engine keeps bind mounts outside the overlay lookup, resolves upper before ordered
lowers, and copies only a mutated lower inode or renamed subtree into the upper.
It does not clone an immutable OCI root merely because a bind or volume route is
present. Root, lower, bind, and copied-up inode ownership lasts for the process
lineage; the namespace epoch invalidates cached paths after copy-up.

Rust container creation at `8e1b220bd0fc0420ec8c03e8db15d8a1f2bee96b`
now maps that division directly. `Containers::create_image` retains the empty
private overlay upper for ordinary bind, tmpfs, named-volume, and anonymous-volume
mounts. Only a mount with the explicit population contract falls back to a full
materialized root, because population currently needs a host-visible merged source
tree. Runtime mount validation and overlay bind exclusion remain unchanged.

A warning-strict release probe used one immutable synthetic image containing 2,000
4 KiB files in 40 directories and alternated full durable forks with empty overlay
forks for 11 samples. The probe was measurement-only and is not retained in the
test corpus. Raw create times in microseconds were:

```text
materialized: 40729 25688 29527 28402 29882 26209 28580 27065 22658 27892 28559
overlay:         172   255   158   228   194   270   165   208   180   182   279
```

The medians were 28,402 us and 194 us respectively: the valid overlay path was
146 times faster and removed 28,208 us from this synthetic mount-activation
boundary. The earlier pinned Alpine measurement remains the representative large
image bound: a full execution-root materialization cost 313,657 us. Native and C
do not expose a comparable durable OCI snapshot/lease transaction, so presenting
either as an A/B for that storage contract would be false equivalence.

Exact committed-tree verification of `8e1b220bd0fc0420ec8c03e8db15d8a1f2bee96b`
ran `cargo test -p hl-container containers::image::tests::` under the pinned Nix
shell with `RUSTFLAGS=-D warnings`: 2 passed, 0 failed. The focused tests prove
ordinary mounts select overlay retention and population still selects materialized
fallback; the full daemon/container suite remains the integration gate.
