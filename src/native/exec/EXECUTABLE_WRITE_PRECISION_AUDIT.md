# Executable-write projection audit

## Retained C oracle

The retained engine was studied read-only at `/Users/x/dd/engine`. The relevant
entry points are:

- `src/core/target/x86_64.c`: `jit86_store_alias_range`,
  `jit86_store_alias_changed`, and `jit86_smc_commit` own write-to-executable
  alias discovery and translation retirement. `g_filemap_lock` protects shared
  file-map identity while overlapping backing offsets are projected to guest
  aliases. Per-CPU `store_ranges` drive writeback; per-CPU `smc_ranges` drive
  invalidation. Overflow retires all translations but deliberately never turns
  into whole-view writeback. Commit runs inside the mapping stop-the-world
  interval and clears both journals before execution resumes.
- `src/translator/guest/x86_64/translate.c`:
  `jit86_drop_range_translations` retires mappings only for the executable guest
  address interval changed by unmap/remap/protection transitions. The mapping
  and indirect-branch caches are cleared only after an actual overlap.
- `src/translator/cache.c`: `map_invalidate_source_ranges` owns range-qualified
  translated-source invalidation. Cache generation rotation and fork repair are
  separate lifetime mechanisms; neither changes guest-visible addresses.
- `src/translator/guest/x86_64/abi.h`: `G_SMC_UNMAP` and `G_SMC_COPYOUT` connect
  Linux mapping/copyout operations to those x86 owners. A copyout through a
  writable alias is projected by backing identity rather than by the writable
  view's permissions.

The retained implementation has no application-specific branch in this path.
It separates mapping identity, writable destination bytes, executable aliases,
and cache lifetime. Architecture specificity is confined to the x86 translator;
shared-file alias enumeration and Linux mapping ordering remain generic. Failed
or overflowing address arithmetic does not publish a partial alias interval.

## Rust ownership and gap

`hl-memory::ProjectionLease` owns mapping/checkpoint admission, host projection
lifetime, writable reservations, exact dirty journals, shared-backing
reconciliation, exclusive-reservation invalidation, and executable alias
publication. `Coordinator::executable_write_ranges` maps a committed dirty
backing interval onto every executable shared alias. Mapping changes rotate the
ledger generation; checkpoint/fork restore constructs a new coordinator and
does not preserve host projection pointers.

The native x86 projection cache is the source-side adapter. Its
`hl_x86_projection_resolve` previously coalesced host-contiguous views whenever
both satisfied the requested access, even if their full guest permissions
differed. `hl_x86_projection_written` owns only the cached window and therefore
ORed the first view's permissions into `executable_written`. An RW store through
the adjacent non-executable view could consequently inherit an RX/RWX bit and
force an epoch exit before `ProjectionLease::publish_written_ranges` performed
the already-precise alias projection.

The repair makes one cached x86 window permission-homogeneous. A cross-boundary
operation falls through the existing operand-resolution/interpreter path; no
partial native store is admitted. Exact dirty publication, alias projection,
mprotect generation changes, shared reconciliation, fault handling, fork, and
checkpoint ownership are unchanged.

| Capability | Retained C | Rust after repair |
|---|---|---|
| Actual written bytes | per-CPU `store_ranges` | bounded native dirty journal |
| Executable alias discovery | logical VMA/file backing overlap | `executable_write_ranges` backing overlap |
| Translation retirement | source-range invalidation | executable page tokens/ranges |
| Permission transitions | mapping STW plus range drop | ledger generation plus projection exclusion |
| Shared writeback | store journal, never SMC overflow | reservation commit plus exact reconciliation |
| Fork/checkpoint | cache-generation repair and STW image | fresh coordinator/projection lifetime |
| Mixed-permission host-contiguous views | distinct logical VMAs | distinct native cached windows |
