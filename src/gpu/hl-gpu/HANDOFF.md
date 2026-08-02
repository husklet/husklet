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

---

# Slice 3 in progress — what has landed, and two decisions that need the manager

Appended by the agent building slice 3. The sections above are slices 1–2 and are unchanged.

## Landed

| commit | established |
|---|---|
| this section's parent commit | `SessionId::next` (process-global, monotonic, never reused); `Session::id`; `Session::exports: Option<Exports>` defaulting to `None`; teardown in both directions on `Drop` and `release_all`; **`Exports::access`**, the bridge that hands a party an `Access` bound to the entry's own state cell. 9 tests in `tests/sharing.rs`'s new sibling `tests/sharing_session.rs`, with a 7-row mutation matrix in the file. |

That is handoff steps 3 and 4. `Entry::state` became the `Arc<AtomicU64>` itself rather than gaining one
beside a `MapState` field, so there is exactly one representation of a claim; `MapState` is derived on
read. Two representations would have drifted in the direction nobody tests — the registry's own tests read
the field and stay green while every guard reads a stale cell.

Worth carrying forward from the mutation run: **a guard bound to the WRONG session is invisible to every
refusal assertion**, because a wrongly-bound guard refuses more rather than less. Only the positive
control caught it.

## Decision 1: `ExportBuffer`/`ImportBuffer` cannot be `Cmd` variants. They belong on the readback channel.

Step 1 above specifies them as `Cmd` variants. That does not work, and the reason is structural rather
than stylistic: **a `Cmd` batch is fire-and-forget with a one-byte ack, and an export mints a host-owned
identity the guest has to learn.** `ImportBuffer` has the same problem in the other direction — SHARING.md
requires the importer to receive the export's authoritative byte length and be unable to widen it, which
is a value returned, not a value sent.

The only way to keep them as `Cmd`s is to let the GUEST name the `ExportId`. That gives away the single
property the whole design rests on: non-reuse is what makes a stale handle distinguishable from a live
one, and SHARING.md makes the host owe that. A guest whose counter repeats would get a silently wrong
resource where the design owes an error.

The channel that already fits exists: `transport/model/readback.rs`. It is a versioned, additive
request/response over the same connection, disjoint from submit by a `surface_id` sentinel, with an
extensible `kind` byte (`BUFFER`, `FENCE`, `FENCE_WAIT`) and a length-prefixed reply. Export is
`kind = EXPORT_BUFFER`, `id = buffer`, reply = the `ExportId`. Import is `kind = IMPORT_BUFFER`,
`id = the local buffer id the guest mints`, `offset = the ExportId`, reply = the authoritative length.
It carries its own `READBACK_VERSION`, so this costs no `WIRE_VERSION` bump and cannot mis-frame a submit.

Consequence worth noting: **`Cmd` is not touched at all**, so the shared-type hazard the handoff warns
about does not arise for this part. Map/unmap (step 5) are a different case and may still want to be
`Cmd`s, because they are ordering points inside a batch and return nothing.

## Decision 2: the `Box<dyn Any>` / `Arc<dyn Any + Send + Sync>` reconciliation splits by executor

The handoff calls this the first real design decision. Three facts were measured, not assumed:

- **Connections are genuinely concurrent.** `apps/husklet/src/runtime/gpu_service.rs::serve_connection`
  does `thread::spawn` per accepted connection. (`hl_gpu::transport::server::serve` is sequential and
  says so in its own doc comment, which is misleading if read as the product's threading model — the
  composition root does not use it.) So a shared native must be safely mutable under real concurrency.
  The map protocol does not supply that: while `Unmapped`, BOTH sessions are permitted, by design —
  SHARING.md says the mechanism turns a race into a refusal only while mapped.
- **All connections share one wgpu device.** `Executors::Wgpu(Device)` is cloned per connection and
  `device.executor()` builds a fresh `WgpuExecutor` over the *same* `Device`.
- **`wgpu::Buffer` is `Clone + Send + Sync`** (wgpu 24.0.5) — verified by compiling a trait assertion, not
  from memory. It is internally refcounted to one allocation.

So on the **product path aliasing is a `Clone`** and is already sound: two sessions holding clones of one
`wgpu::Buffer` on one device is exactly the alias tier 1 wants, and `WgpuBuffer { buffer, size }` needs no
change beyond an `export`/`import` pair.

The **CPU reference executor is the expensive one**, and it is the oracle rather than the product path.
`cpu::model::buffer::Buffer` is `{ data: Vec<u8>, usage: u32 }` with 37 sites treating `data` as a slice
(`len`, index, `copy_from_slice`, `iter_mut`, `&b.data`). Aliasing it needs either `Arc<Mutex<Vec<u8>>>`
— which touches all 37 and creates a self-copy deadlock in `service/copy.rs`, where a source and a
destination are held at once — or `UnsafeCell`, whose soundness argument the map protocol cannot supply
for the reason above.

**Recommendation, for the manager rather than taken unilaterally:** put `export_buffer`/`import_buffer` on
the `GpuExecutor` port with defaults returning `Unsupported`, implement them on `WgpuExecutor` only, and
leave the CPU executor honestly declining. An executor that cannot alias then says so instead of
returning a copy that looks like an alias — a copy that succeeds would make the copy-elimination
measurement meaningless in exactly the way this slice exists to avoid. The cost is that the closed path
is not testable in-process against the CPU oracle, so the end-to-end proof has to be
`e2e/husklet/apps/cuda-present` on the host.

## Not done, and not measured

Steps 1, 2 and 5, and therefore the copy-elimination distribution — there is still no closed path, so
there is still no copy to have eliminated. It remains slice 3's to take, as a distribution.

---

# Session close — 2026-08-01

## Slice 3 is NOT finished. Read this before treating the sharing work as done.

| step | state |
|---|---|
| 1. `ExportBuffer`/`ImportBuffer` protocol surface | **not started.** See the decision above — it is not a `Cmd` pair. |
| 2. Executor wiring | **not started.** |
| 3. Session identity | **landed**, `605fb0498`. |
| 4. `Exports` ownership | **landed**, `605fb0498`. `Option<Exports>`, `None` by default, fails closed. |
| 5. Map/unmap as protocol commands | **not started.** |
| the copy-elimination distribution | **never measured, and could not have been.** |

Nothing is wired end to end, so nothing creates an export in production: `set_guard` has zero callers
outside tests and `Exports` is never constructed outside tests. That was verified by enumeration when the
sharing work was (correctly) cleared of causing a Chrome regression, and it is still true. **The gate is
unreachable in a shipped bundle.** Slices 1–3 are preconditions with tests, not a working capability.

The copy-elimination number has still never been taken, and the reason has not changed: there is no
closed path, so there is no copy to have eliminated. When it is taken it must be a **distribution** — a
mean and a median agreeing to 0.05 ms has already hidden a bimodal shape on this fleet.

`605fb0498` also added the join slices 1 and 2 lacked: `Exports::access` hands a party an `Access` bound
to the entry's own state cell, so a claim taken through the registry is visible on the resolution path.
`Entry::state` IS that cell rather than sitting beside a `MapState` field, because two representations of
one fact drift in the direction nobody tests. Nine tests, seven-row mutation matrix in
`tests/sharing_session.rs`, observed attribution rather than predicted — two rows differed from the
prediction, and a guard bound to the WRONG session was caught only by the positive control, because a
wrongly-bound guard refuses *more* rather than less.

## The `glVertexAttribIPointer` defect — `58d5c7b9f`

Recorded here because it is the session's most consequential fix and the reasoning is not visible from
the diff. It is in `hl-gl`, not this crate, but it was found from an `hl-gpu` NACK and the trail starts
here.

Chrome could not render a frame. `glVertexAttribIPointer` called `record::vertex_attrib_pointer` — the
recorder for the FLOAT entry point — which took no `integer` parameter and hard-coded the flag `false`.
An integer array therefore arrived at lowering indistinguishable from
`glVertexAttribPointer(GL_UNSIGNED_INT, normalized = FALSE)`, a combination GL genuinely does convert to
float. It was converted, declared `Float32x2`, and the shader's `uvec2` input required `Uint32x2`. wgpu
refused the pipeline. Chrome is asking for ordinary GLES 3.0 §2.8; the fault was entirely ours.

**The full wgpu message was in `domain.log` all along, four lines below the line everyone was grepping.**
wgpu builds its description as `format!("Validation Error\n\nCaused by:\n{detail}")`, and
`grep verdict=nack` returns the first line only. Three people read that log and saw a bare
"Validation Error". If a wgpu error ever looks contentless, read the following lines before believing it.

The blast radius came from `hl-gpu`: the refused pipeline was inside batch 12, Chrome's entire startup
working set — 1121 commands, 36.5 MB. `Transaction::drop` rolled the whole batch back, discarding every
`CreateSampler`/`CreateTexture`/`CreateShader` that had already succeeded, and every later reference came
back `unknown/freed`. One bad pipeline, a dead tab.

Measured, both sides bound. Before: installed binary `76c74e4f34db8008`, gl tree `907def965a4263b1`,
workspace `vk-chrome1` — 9 NACKs (1 pipeline validation, 8 `unknown/freed`), no window. After: private
bundle `81eec4cb77a50802`, gl tree `4977530831b41812`, fresh workspace `chrome-e2e-clean5`, domain started
after the build — **0 NACKs**, a native toplevel window (`number=10951 visible=true on_screen=true`), and
19 frames shown.

One trap worth carrying: `grep -c verdict=ack` also returned 0, which is the shape of a mute instrument.
It is not. **`verdict=ack` is `Level::Debug` and is compiled out of a release bundle; `verdict=nack` is
`hl_error!` and survives.** A live `[gpu] E` line in the same log proved the tag and level were reaching
the file, which is what makes the zero a measurement rather than a silence. Caveat on that run: the
es2gears control did not execute (`not_measured`), so there is no in-session control — which weakens a
negative reading, not this positive one.

## Open, unstarted: one validation failure permanently desynchronises the id space

The rollback above is correct in isolation and catastrophic in effect. `runtime::submit` guarantees that
a rejected batch leaves the id lifecycle exactly as it was, which is what lets a connection retry — but
**the guest has no way to learn which of its creates were undone.** The one-byte ack carries a refusal
class and nothing else, the guest advances its object caches optimistically at prepare time, and
`retires_share_group` correctly declines to kill the group for an ordinary refusal. So both sides behave
reasonably and end up permanently disagreeing about which ids exist.

This is not fixed and not started. Two shapes were discussed: a wire change that names the evaporated
ids, or something narrower on the guest side that rolls its own caches back on a NACK.

Related and adjacent, specified by another agent: `src/gpu/hl-gpu-wgpu/SUBMIT_PROPAGATION.md`, which
covers per-operation refusal propagation in `submit_cb_inner`. Read it alongside this — a batch that
reports which operation failed is a precondition for a guest ever being told what to undo.

Also still open from earlier in the slice: guard behaviour under a transaction rollback. A guard attached
inside a batch that then NACKs is covered by no test and the intended behaviour was never decided.
