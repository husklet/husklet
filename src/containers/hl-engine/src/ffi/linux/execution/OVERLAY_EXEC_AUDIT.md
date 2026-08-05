# Overlay executable source audit

## Retained C oracle

The read-only oracle was `/Users/x/dd/engine`. The complete overlay resolver in
`src/linux_abi/container/vfs/overlay.c` was inspected, especially
`overlay_lookup_raw`, `overlay_resolve`, and `xresolve_overlay`. Its executable
call sites were then traced through `src/linux_abi/container/vfs.c`
(`xresolve_exec`, `find_in_path`, and the image-read path) and
`src/linux_abi/x86.c` (the `overlay_resolve` image-read branch).

The C process-global overlay state owns an ordered writable root followed by
`g_lower[]` records for the lifetime of the guest process. Every executable and
ELF-interpreter lookup follows symlinks through that same ordered view. It opens
the selected host path read-only before replacing the image; lookup failure is
reported before any exec teardown. Fork inherits the overlay records and exec
does not discard them. The resolver bounds symlink traversal, keeps absolute
targets inside the guest root, preserves upper/whiteout precedence, and returns
the first visible lower. This ownership is common to AArch64 and x86-64; only
the subsequent architecture-specific image loading differs. The observed host
branches use confined descriptor/path lookup on Linux and macOS rather than
changing overlay precedence.

## Rust divergence and mapping

`FileSource::layered` already implemented ordered upper/lower image reads, and
the initial image loader used it. The transactional exec owner `Sources`,
however, retained only the writable root. `SourceFactory::open` therefore
constructed `FileSource::rooted` for every guest `execve`, so an executable or
interpreter present only in the image lower became `ENOENT` after path
resolution had already found it. `exec_image::Coordinator::fork` repeated the
loss by reconstructing `Sources` from that root alone.

`Sources` now owns the complete ordered lower list and selects
`FileSource::layered` for every transactional exec. `Coordinator` owns and
clones that complete source factory across fork. Immutable lower records are
reference-counted, so exec and fork do not copy the lower list. `NativePath` remains the owner
of metadata and executable-access resolution; `Sources` remains the loader's
bounded byte-read capability. No new lock or host call is added to path lookup,
and source clones retain immutable path records plus the existing projected
authority reference.

The focused regression constructs an empty upper with both the main executable
and interpreter in a lower, opens a fresh exec source through `SourceFactory`,
and requires both image roles to read from that lower. The daemon controls then
exercise real external `grep` and `cat` execs. The `grep` command now exits zero;
its separate empty journal-output failure predates this exec-source regression.
