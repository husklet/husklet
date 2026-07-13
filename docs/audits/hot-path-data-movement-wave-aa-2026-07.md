# Hot-path data-movement audit (wave AA, 2026-07)

This is a static call-frequency and ownership audit of compositor/render, GPU replay, JIT engine and
runtime loops. No timing profile was available, so the report labels observed frequency separately from
expected cost. “Hot” means directly inside a frame, command-stream, input, polling, syscall-translation or
log-pump loop; it does not mean a token happened to match `clone` or `format!`.

## High-confidence cuts

| Priority | Observed call-frequency evidence | Avoidable work and compatible change | Evidence required |
|---|---|---|---|
| P0 | Smithay `snapshot_surface` runs for the root and every composited child/popup on each present. For the overwhelmingly common normal transform, `apply_buffer_transform` calls `src.to_vec()`. | This copies each complete cached BGRA surface even when no transform is requested, before CPU blending or presentation. Return `Cow<[u8]>`/a borrowed pixel view for normal, allocate only for the seven transformed cases, and let `SurfaceBuffer` carry shared immutable storage or borrow through composition. Preserve buffer-release ownership separately. | Allocation-count instrumentation around a Rust rendered scene must show zero full-frame transform allocation for normal surfaces; golden pixels and buffer-lifecycle tests must remain identical. |
| P0 | One Smithay commit/present can call `tree_dirty`, `present_tree`, `clear_tree_dirty`, `complete_tree_buffer_uses` and `pace_tree`; each independently walks descendants and independently calls `collect_popups_for_root`, which scans all live popup surfaces and sorts matching depth. | Build one short-lived `PresentedTree` snapshot per root update containing ordered surfaces, popup offsets and ids. Reuse it for dirty check, composition, pacing, cleanup and buffer completion. Do not cache across commits unless lifecycle invalidation is proven. | Add test-only visit counters to a deep tree and show one topology walk per update. Existing rendered z-order, callback, feedback, destruction and popup-chain tests define behavior. |
| P1 | Legacy Cocoa's poll loop allocates `ready_fds: Vec<RawFd>` on every wake, then performs `clients.iter().position` for every ready fd. | Reuse a scratch vector and maintain `RawFd -> client index`, updating it on `swap_remove`, or process stable indices in reverse. This removes one allocation/wake and the dense O(ready × clients) scan without changing event order. | Rust loop tests with multiple simultaneous ready/disconnecting clients, fd reuse and accepts. Count allocations/wake and comparisons; protocol traces must preserve service order where observable. |
| P1 | Smithay tree routines repeatedly create empty `Vec<WlSurface>` and popup vectors per commit even for the common single-surface/no-popup window. | Give `PresentedTree` inline/small storage or retain reusable scratch capacity in `DdState`. A plain persistent `Vec` is sufficient; do not add a dependency solely for small-vector storage. | Allocation counts for single root and multi-child scenes; lifecycle tests must prove no resource is retained past destruction. |
| P1 | Legacy `Server` clones `children`, data-device id lists, region ops and MIME lists before dispatch/traversal to satisfy mutable borrows. Some occur on each parent commit or selection event. | Replace whole-list clones with scoped immutable extraction of ids or `mem::take`/restore only where reentrancy is impossible. For tree presentation, use one reusable id worklist. Keep owned MIME bytes across asynchronous transfer. | Protocol tests for child mutation/destruction during parent commit and multiple data devices. Allocation counters must identify the clone as recurring before changing it. |
| P2 | Runtime `wait_with_output` retains every `LogChunk` and then copies all chunk payloads into a second `Vec` at process exit. This is per output byte, but only the final concatenation is once/process. | If callers require only combined bytes, append into a single bounded buffer in sequence order during pumping. If live chunk metadata is externally consumed, retain the current representation or store ranges into one backing buffer. | Concurrent stdout/stderr ordering, truncation/backpressure and exit-race tests. Measure peak retained bytes, not throughput benchmarks. |

The first item is the only source-proven full-frame copy: its size is exactly the cached tight BGRA length
(`width × height × 4`) for every snapshot under a normal transform. Tree traversal multiplicity and poll
complexity are also source-proven; their wall-time importance depends on surface/client counts and must be
confirmed with counters rather than a permanent benchmark suite.

## GPU replay: retain deliberate work

`replay_stream` validates the entire stream and then decodes/applies it. The double walk is intentional:
it guarantees malformed late commands cannot partially mutate backend state. Removing validation or
fusing it with execution is incompatible. The bulk `WriteBuffer` path already length-checks and borrows
the payload in both passes, avoiding the dominant megabyte-scale copy.

Potential neutral refinements are limited:

- Reuse capacity for the returned presentation-token vector when a caller processes many frames, or add a
  callback/sink API alongside the existing `Vec` API. Do not replace the public result solely to avoid the
  usual zero/one-token allocation without call-site evidence.
- Error `format!` calls are inside `map_err` and therefore execute only on malformed streams. They are not
  success-path work and should remain diagnostic.
- Non-`WriteBuffer` `Cmd::decode` ownership is part of validation/execution semantics. Convert another
  payload to borrowing only after a trace shows it is large and frequent and lifetimes remain contained to
  the input frame.

## Renderer/JIT/runtime items that are cold or speculative

- Metal retained-object `clone()` operations generally increment Objective-C reference counts; they do not
  copy textures, pipelines or devices. Pipeline/library/sampler caches avoid compilation and creation on
  the draw path. Removing retains without an ownership proof risks use-after-free for negligible gain.
- PNG BGRA→RGBA buffers, dump-path formatting and shader-source dump names are debug/error paths. They are
  intentionally expensive only when requested and are not production frame cuts.
- Presenter title formatting happens at window creation/title change, not per pixel. Clipboard MIME/string
  conversions happen on clipboard transactions, not frames.
- JIT persistent-cache `malloc`/`memcpy` calls occur while loading/saving cache images. Configuration string
  construction and config-fd allocations are launch-time. They can affect startup and peak memory, but the
  source does not establish them as translated-block execution hot paths.
- JIT ELF lane-sized `memcpy` calls implement unaligned/width-safe guest memory semantics. Replacing them
  with typed loads may change fault/alignment behavior and is not a compatibility-neutral optimization.
- Jail/fscache `snprintf` uses stack buffers, not heap allocation. Path construction may be syscall-hot, but
  removing it needs syscall-frequency evidence and cache-correctness analysis; token counting is insufficient.
- Spawn command `format!`, environment clones and device-mount strings are once per container launch.

## Safe sequencing and acceptance

1. Add temporary allocation/visit counters around normal-transform snapshotting and tree construction, then
   land borrowed/shared BGRA storage with Rust pixel and lifecycle tests.
2. Introduce the ephemeral `PresentedTree`; compare exact surface order, popup offsets, callback counts,
   presentation feedback and release events before/after.
3. Instrument poll wakes/client comparisons and only then replace the ready-fd scan. Test fd reuse and
   disconnect mutation explicitly.
4. Change log storage only after documenting ordering and retention requirements of every caller.
5. Collect sampling profiles/counters for JIT syscall translation before proposing engine hot-path changes.

Temporary counters, allocation traces and sampling profiles are appropriate evidence. Do not add a benchmark
suite, source-string assertions or tests that merely check an implementation token exists. Closure requires
identical rendered bytes/protocol behavior plus fewer counted allocations, copies, visits or retained bytes.
