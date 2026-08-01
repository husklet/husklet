# Cross-connection sharing — handoff to slice 3

Written 2026-08-01 by the agent who built slices 1 and 2, for whoever builds slice 3. Slice 3 is the one
that closes the path; everything below it is preconditions.

## The two artifacts, and which parts of them are real

The boundary between design and implementation moved twice today, so read this table before either file.

| document | status |
|---|---|
| `src/gpu/hl-gpu/SHARING.md` | **Mostly design.** The mechanism, identity, lifetime and failure-edge sections are now IMPLEMENTED (slices 1–2). "What this does NOT make safe" is still binding. The "constraint on Vulkan sharing" section is a permanent product constraint, not a to-do. The tier-3 scope note is design. |
| `src/surface/hl-cuda/INTEROP.md` | **All design and measurement.** Nothing in its tier tables is implemented. Its coverage numbers and the 0-of-21 interop finding are measured facts as of HEAD `8e8e69600`. |

## What landed, and what each slice established

| commit | slice | established |
|---|---|---|
| `3660d73be` | 1 | The export registry: `runtime/model/sharing.rs`. `ExportId` identity, refcounted lifetime, charge transfer, the map state machine, `check_access`. 16 tests in `tests/sharing.rs`, each verified by reverting the rule it guards. |
| `65cef9b7b` | 2 | **The gate.** `check_access` wired into `ResourceTable::get`/`get_mut`, the single point every command resolves an id to a native object. 4 tests in `tests/sharing_gate.rs`. `ResourceTable::iter` deleted. Cost measured: +0.03–0.08 ns/resolve, flat in table size. |
| `811909f1f` | — | `MappedElsewhere` mapped in all three drivers after it broke the shared tree. |
| `361fa7d2f` | — | `ACK_MAPPED_ELSEWHERE` on the wire, with the older-guest path measured. |

Slices 1 and 2 are self-contained: nothing yet creates an export, because no protocol command does.

## What to do next, in order

1. **`Cmd::ExportBuffer` / `Cmd::ImportBuffer`, and their wire encoding.** `Cmd` is a shared type in
   exactly the way `GpuError` and `RefusalKind` were — see the trap below.
2. **Executor wiring.** On import, insert the aliased native into the importing session's table and call
   `ResourceTable::set_guard` with an `Access` built from the export entry's shared state cell. The
   registry already returns the `Arc<dyn Any + Send + Sync>`; the executors currently store
   `Box<dyn Any>`, and reconciling those two is the first real design decision of slice 3.
3. **Session identity.** `sharing::SessionId` exists but nothing assigns one. The runtime needs a real
   per-connection id, and `Access::new` needs it too. Do this before 2, not after.
4. **`Exports` ownership.** It is `Clone`-shares-one-registry, like `GlobalLedger`. It has to be
   constructed once in the composition root and handed to every session, not created per session — a
   per-session registry would compile, pass slice 1's tests, and share nothing.
5. **Map/unmap as protocol commands**, with the fence ordering `SHARING.md` describes. `FenceId` and
   `Cmd::WaitFence` already exist; no new synchronisation primitive is needed.
6. **Only then** the ten CUDA entry points (`INTEROP.md` tier 1). Do not start here.

## Traps that are not obvious from the code

- **`cargo check -p hl-gpu` is not enough and will mislead you.** Four crates consume these shared types.
  I added a `GpuError` variant, checked one crate, and left the tree unbuildable for every agent needing
  `hl-cuda`, `hl-gl` or `hl-vulkan`. The rule is in `AGENTS.md` now: `cargo check --workspace
  --all-targets` before committing anything touching a shared type here. `Cmd` is one.
- **`cargo test -p hl-cuda` fails outside the nix shell** with `linker aarch64-linux-gnu-gcc not found`;
  `hl-cuda`'s build script cross-builds the guest shims. Use `nix develop . --command cargo …`.
- **`hl-gpu`'s `transport_deadlines` has two pre-existing failures** —
  `applied_frame_without_ack_is_terminal_and_never_replayed` and
  `connect_deadline_bounds_a_saturated_unix_backlog`. Verified against a clean baseline; not yours, and
  not caused by any of this work. Do not spend time on them thinking you broke something.
- **`header.rs` has a guard that will fail your build if you add a `RefusalKind`,** by design: a
  hand-written list paired with a compiler-enumerated match and a length assertion. It is not an
  obstacle, it is the thing that stops a new variant silently escaping the byte-collision check. Name the
  variant in all three places.
- **The `Access` state cell is `Arc<AtomicU64>` holding `holder + 1`, not `holder`.** Zero means
  unmapped, so session 0 would be invisible without the offset.

## Decisions you will be tempted to undo, and why not

Each of these looks like an easy simplification. Each cost something to reach.

**`MappedElsewhere` must not collapse into the invalid-argument arm.** It is the only refusal on this
wire that a caller recovers from by *waiting* rather than by sending something different — the identical
call from the same caller succeeds once the holder unmaps. A caller that cannot tell it from a malformed
request will "fix" a correct program. Folding it in with the `Invalid` arms would remove four lines and
delete that distinction everywhere at once.

**`ExportId`s are never reused.** This looks like hygiene and is not: it is the only thing that makes a
stale handle distinguishable from a live one. Recycle them and an import naming a dead export silently
succeeds against an unrelated resource — a *wrong answer* where the design owes an error. If you find
yourself wanting a compact id space, you want a separate index, not reuse.

**The gate sits in `ResourceTable::get`/`get_mut`, not in command handlers.** Handlers are the obvious
place and are wrong. A rule spread across handlers is complete the day it is written and incomplete a
month later; this repository has already shipped a format ungate where three of four paths learned a new
case and the fourth turned refusals into failures. The current placement is total by *construction*:
`live` is private, so nothing outside `id.rs` can reach a slot except through that type's public API, and
exactly two of its methods hand out a native reference. That is a closed enumeration over one file rather
than an audit, and it means a command added next year cannot bypass the check because there is no other
way to reach a resource. **`ResourceTable::iter` was deleted for this reason** — it had zero callers and
yielded an unguarded native, so it was a hole waiting for its first caller. Do not reintroduce it; if you
need teardown iteration, return ids and resolve them, or add a method that is explicitly guard-exempt and
says why.

**The Vulkan constraint is permanent, not transitional.** I claimed the wire code would dissolve it and
that was wrong — the wire carries the reason *class*, and each driver maps that class to the result the
same error produces locally. Vulkan's local result is `VK_ERROR_UNKNOWN` because `VkResult` has no code
for transient contention, so the wire can only make remote agree with local, never make local honest.
Vulkan-side sharing must therefore *prevent* contention through the map protocol rather than report it.
Do not design a Vulkan path that expects a caller to observe a refusal and retry, and do not treat the
constraint as waiting on a feature.

**The owner destroying under a live import succeeds and retains, with the charge moving to the importer.**
Both alternatives were considered. Refusing the destroy breaks legal application behaviour — deleting a
buffer is something applications do, and an error there leaves them no recourse. Silent retention is an
invisible leak. Charge transfer bounds the retained memory by the budget of whoever is keeping it alive
and puts it in accounting that already exists.

## What is not measured

- **The copy-elimination measurement.** `SHARING.md` requires a first slice to show the round trip is
  gone *by measurement, not argument*. It could not be taken yet — nothing is wired end to end, so there
  is no copy to have eliminated, and a number for something that does not exist would be the emptiest
  kind of green. **It belongs to slice 3, the slice that closes the path.** Report it as a
  **distribution**, not a mean or a median: a mean and a median agreeing to 0.05 ms has already hidden a
  bimodal distribution on this fleet, and my own first attempt at the gate's cost reported the guarded
  path as *faster* — a negative delta that was not a speedup but proof the instrument had no resolution.
  `e2e/husklet/apps/cuda-present` is the natural place to take it: it already runs the round-trip route
  end to end with an independent reference, so the interop route can be compared against it directly.
- **Anything above one importer per resource.** The tests cover one and eight concurrent sessions; no
  case exercises a long-lived many-importer graph.
- **Guard behaviour under a transaction rollback.** `ResourceTable` journals inserts and removes for
  atomic rollback. A guard attached inside a batch that then NACKs is not covered by any test, and I did
  not work out what should happen. Decide it deliberately.
- **Everything in `INTEROP.md` tier 1.** No CUDA entry point exists.

## One more thing

The count for a working external-memory tier is **23 entry points, not 21** — `probe.c` does not probe
`cuSignalExternalSemaphoresAsync` or `cuWaitExternalSemaphoresAsync`, and an imported semaphore is inert
without them. That is right for "what is missing that we measure" and wrong for "what building it costs",
and it errs in the direction that hurts.
