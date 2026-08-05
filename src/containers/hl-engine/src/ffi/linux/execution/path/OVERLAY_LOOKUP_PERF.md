# Overlay lookup performance audit

No timing result is recorded here. The benchmark was prepared source-first and
must run only from the released, exact committed tree.

## Retained C oracle

The complete lookup and cache path was read in the retired engine:

- `linux_abi/container/vfs/overlay.c`: `layer_follow`, `dir_is_opaque`,
  `overlay_dir_verdict`, `overlay_lookup_raw`, `overlay_lookup`, and
  `overlay_resolve`;
- `linux_abi/container/route.c`: `atpath`, including resolution-cache lookup
  and storage;
- `linux_abi/fdcache.c`: `mc_hash`, `hl_fdcache_resolution_bump`,
  `hl_fdcache_upper_negative_lookup/store`,
  `hl_fdcache_upper_verdict_lookup/store`, dentry lookup/store,
  resolution lookup/store, open lookup/store, and `hl_fdcache_reset`;
- `linux_abi/syscall/dispatch.c`: namespace-mutation epoch bumps.

The C lookup searches upper then lowers, probes whiteouts before falling
through, and applies a recursively memoized parent verdict so a hidden or
opaque ancestor cannot leak lower descendants. It follows symlinks across the
whole union with a 40-hop bound. Its direct-mapped caches store fixed-size path
strings, not descriptors. They are guarded by a container-shared namespace
epoch plus a process-local fork/chroot generation. The upper-negative cache
removes repeated upper/whiteout/opaque probes for lower-only directories; the
directory-verdict cache amortizes ancestor visibility; dentry, resolution, and
open caches collapse repeated canonical climbs and component walks. Volume
paths are deliberately excluded because their backing is host-mutable.

## Rust hot-path inventory

The ordinary Rust path encodes its owned descriptor directly in `NodeHandle`.
It performs no registry lookup or allocation after the descriptor is opened.

The layered path stores every handle in a mutex-protected `BTreeMap`. Each path
component currently performs registry lookup, whiteout probes, candidate
`fstatat`/`openat`, allocates a candidate vector and guest path, inserts another
map entry, and later removes it. The registry mutex remains held while the
lookup closure performs native filesystem calls. Duplicating `ParentLease`
duplicates the selected descriptor, every retained lower parent, and the upper
root. Unlike the retained C implementation, this path has no upper-negative,
directory-verdict, dentry, or full-resolution memo.

These are hypotheses until the committed benchmark runs. The likely first
optimization boundary is to stop holding the registry lock across native
probes and replace logarithmic, allocation-heavy transient handle storage with
a generation-checked slot table. Cache work must follow measured evidence and
retain the shared-epoch and mount-exclusion correctness rules from the C oracle.

## Benchmark contract

`overlay_bench.rs` uses the same `Resolver`, `Host`, tagged registry, and
`ParentLease::duplicate_parent` production path for every sample. A single-root
host is the raw-descriptor control. Layered samples cover an upper hit, lower
hit, upper whiteout miss, and twelve-component lower hit. The deep sample uses
an equally deep ordinary control so traversal depth is not confused with
overlay overhead. Each sample verifies its verdict, warms 256 iterations, and
measures 25,000 operations. It is ignored by normal tests and refuses a debug
build.

Release command:

```text
cargo test --release -p hl-engine overlay_resolution_microbench -- --ignored --nocapture
```

## Linux component-pin A/B (2026-08-04)

The first bounded optimization removes the `fstatat` performed before every
Linux `O_PATH | O_NOFOLLOW` pin. Linux does not select pin flags from the inode
kind: `openat` can pin first and the existing `fstat` supplies the kind. Missing
opens retain the same `NotFound` result. macOS retains its type-first sequence.

Alternating warning-strict release runs on the same host and target directory:

| sample | baseline ns/op | candidate ns/op | change |
|---|---:|---:|---:|
| ordinary control | 1,560.6 | 1,526.7 | -2.2% |
| layered upper hit | 2,683.5 | 2,581.1 | -3.8% |
| layered lower hit | 3,419.8 | 3,219.4 | -5.9% |
| whiteout miss | 1,810.8 | 1,719.5 | -5.0% |
| deep lower hit | 33,217.5 | 31,281.9 | -5.8% |

The machine was shared, so the syscall removal is the stronger mechanism
evidence and the timings are bounded A/B evidence rather than a stable-host
certificate. The remaining deep-path gap is dominated by the missing C-style
epoch-gated dentry/full-resolution memo, registry locking, candidate
allocation, and descriptor duplication.

## Cache safety gate

A positive descriptor cache was prototyped and deliberately not retained.
`ParentLease::publish` is currently called only by successful regular-file
copy-up. The following namespace mutations do not yet advance the resolver
epoch: successful mkdir, mknod, unlink, rmdir, rename, link, symlink,
`open(O_CREAT)`, upper-parent materialization, and descriptor/inode link
publication. C covers these through the dispatch mutation bump and explicit
copy-up/materialization bumps in `overlay.c` and `fdcache.c`.

Until those paths publish after every visible commit—including partial-failure
paths—a cache could resurrect a renamed or deleted pathname. The eventual
cache must retain owned descriptors, be bounded, exclude host-mutable mount
routes, validate the Acquire-loaded epoch again after borrowing a cached pin,
and discard a fill if the epoch changes during resolution. Fork, chroot, root,
and mount changes additionally require a process-local namespace generation,
matching the retained C `fgen` model. These invalidation tests are prerequisites
to implementation, not follow-up hardening.

### Namespace-mutation oracle audit

The namespace publication lane studied retained
`../engine/src/linux_abi/syscall/fs.c`, entry dispatch cases `mknodat`,
`mkdirat`, `unlinkat`, `symlinkat`, `linkat`, and `renameat`/`renameat2`, plus
their overlay whiteout/copy-up calls. The C dispatcher owns no durable mutation
transaction: each syscall pins its parent descriptors, performs the host
namespace operation, then evicts pathname/access/readlink or inode metadata
state after success. Overlay unlink and rename can make a whiteout visible after
an earlier host operation, so invalidation is tied to each visible namespace
commit rather than the final syscall return alone. Rust ownership maps to
`PendingMutation` (operation lifetime and syscall ordering), `ParentLease`
(pinned selected/upper parent and shared epoch), and `overlay_publish`
(copy-up, parent materialization, and whiteout publication). Linux supplies
`renameat2` and descriptor-based `linkat`; macOS uses `renameat` and does not
support the pinned-inode hard-link path. The Rust mutation publication now
covers mkdir, mknod, unlink/rmdir, rename, link, and symlink immediately after
host success, including a later tmpfs-accounting error. `open(O_CREAT)`, upper
parent materialization, and descriptor/inode publication remain separate gaps.
