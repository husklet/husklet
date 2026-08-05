# Indirect-branch table reset audit

This bounded Linux optimization follows the retained engine's lazy reset of
large indirect-branch tables. It does not claim end-to-end parity.

## Retained oracle

The read-only implementation studied was
`../engine/src/translator/cache.c`, especially `cache_create`,
`cache_flush`, `cache_fork`, and the Linux `madvise(..., MADV_DONTNEED)` reset
path. The translator owns the table for the cache lifetime. Translation and
mapping mutation are serialized by the JIT lock; fork repair runs while peers
cannot execute translated code. A reset invalidates the complete table before
execution resumes. Linux supplies zero-filled pages on later access. Other
hosts retain their explicit clear path.

## Husklet mapping

`hl_native_executor` owns its 65,536-entry IBTC from construction through
destruction. `ibtc_clear` is called during construction, identity reset, arena
rollover, authority changes, and fork repair while exclusive mutation
admission excludes translated execution. Individual invalidation remains an
atomic publication operation and is unchanged.

The table is allocated at a 64 KiB boundary and occupies exactly 1 MiB, so the
Linux discard range is page aligned and does not include allocator metadata.
Successful discard has the same next-read zero contract as `memset` without
faulting every table page into the process. Failure and non-Linux hosts fall
back to `memset`. No guest-visible ordering, errno, cache identity, or table
lifetime changes.

This removes fixed construction/reset memory traffic. The dominant steady-
state gap remains generated projected-memory guards and write publication; it
must be addressed separately with exact compatibility evidence.
