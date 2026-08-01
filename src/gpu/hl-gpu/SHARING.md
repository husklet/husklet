# Cross-connection buffer sharing — design

Design only. Not implemented, not authorised for build. The consumer that motivates it is CUDA↔GL
buffer interop (`surface/hl-cuda/INTEROP.md`, tier 1), but the mechanism is `hl-gpu`'s because the
limitation is `hl-gpu`'s: resources are per-connection and nothing in the protocol lets one connection
name another's.

Read the last section first if you are reviewing an implementation against this. **What this does not
make safe** is the part most likely to be over-read.

## The problem, precisely

`runtime::model::resources::SessionResources` maps protocol ids to executor-native objects, one table per
connection. `GlobalLedger` is shared but holds only residency totals — it confers accounting, not
addressability. The guest GL driver and the guest CUDA driver are separate connections over
`$HL_GPU_EXEC`, so a `BufferId` minted by one is meaningless in the other.

Both objects are already host-side, in one executor process. Nothing needs to cross the guest boundary,
so this is aliasing inside a process: no memfd, no IOSurface, no copy.

## Mechanism

One process-global **export table**, shared across sessions the way `GlobalLedger` already is, holding:

```text
ExportId -> { resource: Arc<native>, bytes: u64, owner: SessionId,
              importers: Set<SessionId>, state: Unmapped | MappedBy(SessionId),
              owner_released: bool }
```

Two protocol commands:

- `ExportBuffer { buffer: BufferId } -> ExportId` — the owning session offers one of its buffers.
- `ImportBuffer { export: ExportId } -> BufferId` — the importing session mints a **local** id in its own
  table that aliases the same native object.

The importer's id is its own. Ids are minted per-session and would collide otherwise, so passing the
exporter's raw `BufferId` across is not an option and is the first thing a reviewer should check has not
crept in.

## Identity

- **`ExportId` is process-global, monotonic, and never reused.** This is load-bearing, not hygiene: it is
  the only thing that makes a stale handle distinguishable from a live one. If ids were recycled, an
  import naming a dead export would silently succeed against an unrelated resource — a wrong answer
  where the design owes an error.
- **One export entry per resource.** Re-exporting an already-exported buffer returns the existing
  `ExportId` rather than minting a second. Two entries for one resource means two refcounts and one of
  them will be wrong.
- **Duplicate import in one session is refused.** This matches what real CUDA does with a
  double-registered GL buffer, and it matches the duplicate-create rule the resource table already
  enforces uniformly.
- **The export entry carries the authoritative byte length.** The importer sees that length and cannot
  widen it. `cuGraphicsResourceGetMappedPointer` returns a pointer *and* a size, and a size the two sides
  disagree about is an out-of-bounds kernel that no bounds check catches.

## Lifetime

The rule is one sentence: **the native resource lives until the last live reference drops, and the
residency charge follows the last live reference.**

The exporting session owns the object. Importers hold references. `owner_released` records that the owner
has destroyed its id while importers remain.

Charge following the reference is what stops the deferred release being a leak nobody can see. When the
owner destroys its buffer with importers outstanding, the owner's ledger is credited and the retained
bytes are charged to the importing session(s). The memory is then bounded by the budget of whoever is
actually keeping it alive, and it shows up in the accounting that already exists rather than in a
category nobody reads.

## Failure edges

These are the design. A version of this that only describes registration and mapping will be
reimplemented at the first crash.

### 1. Owner frees while an importer still references

**Rule: the destroy SUCCEEDS, the storage is retained, the charge moves to the importer.**

Refusing the destroy was considered and rejected: deleting a GL buffer is legal application behaviour and
an application that gets an error there has no recourse. Silently retaining was also rejected — that is
an invisible leak. Retain-with-charge-transfer keeps both the application and the accounting honest.

The owner's `BufferId` is destroyed immediately and becomes use-after-free for the owner on its next use,
exactly as it would without sharing. The importer is unaffected until it unregisters.

**Must be tested:** the owner destroys, the importer's subsequent read returns the data written before the
destroy; the retained bytes appear against the importer in the ledger; the owner's next use of its own id
is refused as use-after-free.

### 2. A handle from a connection that has gone away

Session teardown releases the owner's ids but must not free an object with importers. `owner_released` is
set as in edge 1.

An `ImportBuffer` naming an `ExportId` that no longer exists must fail with a typed error. The failure
mode to design out is returning a default or a freshly created resource — "could not reach the subject"
must not be indistinguishable from "here is your buffer". Non-reused ids are what make this decidable.

Symmetrically: if the **importing** session goes away, its reference drops. If it was the last one and the
owner had released, the object is freed then.

**Must be tested:** import a stale `ExportId` and get a typed error, with a positive control importing a
live one through the same path in the same test — a refusal from a path that never works proves nothing.
Also: owner disconnects with a live import, importer keeps working; importer disconnects last, the object
is actually freed and the ledger returns to its baseline.

### 3. Two connections racing on the same buffer

Two different races, and conflating them is how this gets built wrong.

**3a — table race.** Concurrent `Export` / `Import` / `Destroy` on one entry. The export table is behind a
mutex and each operation is atomic under it. Ordinary, and the only trap is holding that lock across an
executor call; it must not be.

**3b — data race.** CUDA writing while GL reads. This is what map/unmap exists for, and the design must
enforce the state machine rather than trusting the application to follow it:

- `state` is `Unmapped` or `MappedBy(session)`.
- Map transitions `Unmapped -> MappedBy(caller)`; mapping an already-mapped resource is refused.
- **While `MappedBy(X)`, use of the underlying resource by any session other than X is refused.**

That last rule is the expensive one and the one most likely to be quietly dropped. It is only real if the
check sits at the **single point where a command resolves a resource id to its native object**. Scattered
across individual command handlers it will be complete on the day it is written and incomplete a month
later — this repository has already paid for a capability where three of four paths learned a new case
and the fourth turned refusals into failures.

**If the check cannot be placed at that single lookup point, this capability must not ship.** An exported
symbol is a promise to every application, and a mapping rule that is enforced on most paths is worse than
one enforced on none, because it fails as silently wrong data instead of as an error.

Ordering, as distinct from exclusion, rides on machinery that already exists: both sessions execute on
the same host executor, and `FenceId` / `Cmd::WaitFence` are already in the protocol. Map inserts a wait
on the other side's outstanding work; unmap signals. No new synchronisation primitive is required.

**Must be tested:** map, then have the *other* session touch the resource, and require a refusal — with a
positive control showing that same session touching it successfully once unmapped. Double-map refused.
Unmap by a session that does not hold the map refused. And an actual data dependency across the seam —
one side writes, the other reads after the fence, and the value is asserted — because a test that only
checks refusals never proves the working path exists.

### 4. Edges that are easy to forget

- **Import of a resource the caller already owns.** Refuse; it is a self-alias and every refcount rule
  above assumes distinct sessions.
- **Export of an already-destroyed id.** Refuse with the table's existing use-after-free error rather
  than a new one.
- **Unregister while mapped.** Refuse, or define it as an implicit unmap — but pick one and test it.
  Leaving it undefined is how a resource ends up permanently `MappedBy` a session that has gone.
- **Zero-length or absurd-length buffers.** The sibling creation paths already refuse both; the export
  path must not become the one that does not. This exact asymmetry was found and fixed in `hl-vulkan`.

## What this does NOT make safe

Stated so the implementation is not read as broader than it is.

- **It is buffers only.** No images, no textures, no CUDA arrays, no format reinterpretation, and none of
  the tiling or swizzle questions those raise. Tier 2 is not partially delivered by this and must not be
  described as such.
- **It does not share guest memory.** It aliases two host-side resources. No guest pointer participates,
  and nothing here helps a case where the data genuinely starts in the guest.
- **It is single-process.** Two workspaces, or two executor processes, share nothing. There is no
  cross-machine or cross-container story here at all.
- **It is not a security boundary.** An `ExportId` is a capability token: any session holding one gets
  access to the resource. The sessions involved are drivers inside one workspace and are mutually
  trusting. Do not reuse this to isolate anything.
- **It does not make concurrent access safe — it makes it refused.** Correctness across the seam still
  depends on the application mapping and unmapping. The design turns a data race into an error, which is
  a large improvement and is not the same as a guarantee.
- **It does not deliver Vulkan interop.** Tier 3 should be built on this rather than beside it, but
  `hl-vulkan` carries `VK_KHR_external_memory` in `capability.rs` only, so that work is two-sided and
  untouched by this.
- **It does not survive device loss or executor restart.** Export ids are process-lifetime.
- **It says nothing about performance.** Removing the round trip is the point, but no number here is
  measured. A first slice must show the copy is gone by measurement rather than by argument.

## Scope note for tier 3

An imported external semaphore is inert without `cuSignalExternalSemaphoresAsync` and
`cuWaitExternalSemaphoresAsync`, which `e2e/husklet/apps/cuda/probe.c` does not currently probe. The
honest count for a working external-memory tier is **23 entry points, not 21**. A map that undercounts
what "working" requires produces an underestimate at exactly the wrong moment.
