# Guest-memory lookup audit

## Retained C oracle

The read-only oracle was `/Users/x/dd/engine`. The complete mapping and lookup
domain studied was `src/linux_abi/logical_vma.c` and `.h`, together with the
consumer seam in `src/translator/guest_memory.c` and `.h`, the architecture
bindings in `src/core/target/aarch64.c` and `x86_64.c`, and pin/copy ordering in
`src/linux_abi/syscall/guest_copy.c`.

`hl_logical_vma_ledger` owns sorted mutable entries under one mutex and publishes
an immutable sorted `hl_logical_vma_snapshot` with an acquire/release pointer
exchange. `hl_logical_vma_resolve` and `hl_logical_vma_resolve_exec_span` binary
search that snapshot without taking the writer mutex. Mapping, unmapping and
protection stage complete entry sets, publish the snapshot, then release-increment
the separate atomic generation. Old snapshots retain backing references and are
reclaimed only at a declared quiescent point. Pins instead take the ledger mutex,
retain the selected backing, and release it in `hl_logical_vma_unpin`; this makes
the returned storage lifetime explicit across a concurrent mapping transition.

The translator resolves the generation counter once when binding its operation
table. Instruction fetch performs an acquire generation read before reusing a
span. Executable resolution checks execute permission and reports the maximal
stable guest interval and host delta. Data copies preserve prefix and fault
ordering by pinning each span, copying only the proven contiguous prefix, and
unpinning it. Writable aliases notify executable aliases after the copy. Linux
and macOS retain a close-on-exec duplicate file descriptor for shared backing;
Windows retains a cloned host handle. Direct mappings borrow their storage,
whereas shared mappings own and unmap canonical storage. Reset and destroy retire
all views and release every backing.

## Rust ownership mapping

`hl_memory::MemoryLedger` owns the canonical sorted `RegionSet`, transition
generation and mutation lock. `MappingCoordinator` owns mapping admission,
transaction ordering, host projection, pinning, write rollback and executable
publication. `ProjectionLease` is the Rust equivalent of the retained backing
pin: mapping transitions cannot begin while it retains storage and authority.
`RegionSet::resolve` preserves the C binary-search and contiguous-prefix rules.

Before this change, even a generation-only validation acquired the whole
`RwLock<LedgerState>`. That diverged from the C oracle's independent atomic
generation and added reader-lock traffic to exclusive reservations, atomic
batches, projection checks and native execution token validation. The generation
is now also published through an `AtomicU64`: mutations still update canonical
state while holding the writer lock, then release-publish the matching generation;
readers acquire-load it. Mapping data and permission lookup remains behind the
existing ledger lock. This deliberately does not implement coarse projection or
weaken exact write publication/coherence.

## Evidence

Both sides used commit `037f67c69` containing the same ignored diagnostic, a
release build, 20,000,000 generation reads per sample, and nine post-warm samples.
The locked baseline median was 93,428,833 ns (range 91,630,917--96,920,250 ns).
The atomic candidate median was 12,743,250 ns (range 10,782,167--23,038,333 ns),
or 7.33 times faster. The candidate won all nine samples; its single noisy maximum
remained below the baseline minimum.

`RUSTFLAGS='-D warnings' cargo test --locked --offline -p hl-memory` passed 93
tests with zero failures and one ignored performance diagnostic. The suite covers
concurrent mapping readers, projection/mutation exclusion, stale-generation
rejection, exact executable-alias publication, rollback, atomics and checkpoint
restore.
