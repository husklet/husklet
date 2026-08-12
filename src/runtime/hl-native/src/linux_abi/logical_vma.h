#ifndef HL_LINUX_ABI_LOGICAL_VMA_H
#define HL_LINUX_ABI_LOGICAL_VMA_H

#include <pthread.h>
#include <stddef.h>
#include <stdatomic.h>
#include <stdint.h>

/*
 * Linux exposes 4 KiB VMAs to x86 guests even when the host can only create
 * mappings at a coarser granularity.  This ledger describes the guest-visible
 * intervals separately from their host representation.  Ordinary mappings do
 * not need an entry; only mappings whose address or file offset cannot be
 * represented directly by the host are recorded here.
 */
typedef struct hl_logical_vma_backing hl_logical_vma_backing;
typedef struct hl_logical_vma_plan hl_logical_vma_plan;

typedef struct hl_logical_vma_view {
    uint64_t guest_first;
    uint64_t guest_last;
    uint64_t host_delta;
    uint64_t backing;
    uint64_t backing_offset;
    uint32_t protection;
    uint32_t flags;
} hl_logical_vma_view;

/*
 * Public immutable descriptor for generated fast paths. Views are sorted and
 * non-overlapping. host = guest + host_delta (modulo uintptr width).
 */
typedef struct hl_logical_vma_snapshot {
    struct hl_logical_vma_snapshot *next;
    size_t count;
    hl_logical_vma_view views[];
} hl_logical_vma_snapshot;

typedef struct hl_logical_vma_entry {
    uint64_t guest_first;
    uint64_t guest_last;
    uint64_t backing_offset;
    uint32_t protection;
    uint32_t flags;
    hl_logical_vma_backing *backing;
} hl_logical_vma_entry;

typedef struct hl_logical_vma_ledger {
    pthread_mutex_t lock;
    hl_logical_vma_entry *entries;
    size_t count;
    size_t capacity;
    _Atomic(hl_logical_vma_snapshot *) current;
    hl_logical_vma_snapshot *retired;
    /* Monotonic mapping generation, bumped after every snapshot publication.
       Readers that cache a derived fact revalidate against it instead of
       against `current`, whose value a freed-then-reallocated snapshot can
       reuse (ABA). Never wraps in any realistic run. */
    _Atomic uint64_t generation;
} hl_logical_vma_ledger;

enum {
    HL_LOGICAL_VMA_READ = 1u,
    HL_LOGICAL_VMA_WRITE = 2u,
    HL_LOGICAL_VMA_EXEC = 4u,
    HL_LOGICAL_VMA_SHARED = 1u,
    HL_LOGICAL_VMA_DIRECT = 2u,
};

typedef struct hl_logical_vma_resolution {
    void *host;
    size_t contiguous;
    uint32_t protection;
    uint32_t flags;
} hl_logical_vma_resolution;

typedef struct hl_logical_vma_pin {
    void *host;
    size_t contiguous;
    uint32_t protection;
    uint32_t flags;
    void *token;
} hl_logical_vma_pin;

/*
 * Stable, pointer-free description used by checkpoint/restore.  backing_offset
 * is absolute within the vnode (not relative to the engine's hidden host
 * window), so independently created aliases retain their canonical topology.
 */
typedef struct hl_logical_vma_descriptor {
    uint64_t guest_first;
    uint64_t length;
    uint64_t device;
    uint64_t inode;
    uint64_t backing_offset;
    uint32_t protection;
    uint32_t flags;
} hl_logical_vma_descriptor;

int hl_logical_vma_init(hl_logical_vma_ledger *);
void hl_logical_vma_destroy(hl_logical_vma_ledger *);

/*
 * Create a canonical hidden MAP_SHARED view for a guest VMA.  guest_first,
 * length, and file_offset use Linux's logical 4 KiB granularity.  host_page is
 * the actual host mmap granularity and must be a power of two.
 */
int hl_logical_vma_map_shared(hl_logical_vma_ledger *, uint64_t guest_first, uint64_t length, uint32_t protection,
                              int fd, uint64_t file_offset, size_t host_page);

/*
 * Failure-atomic mapping transaction. Prepare allocates and maps every hidden
 * resource while leaving the live ledger untouched and holding its mutation
 * lock. Commit is allocation-free/infallible; abort discards the staged view.
 */
int hl_logical_vma_prepare_shared(hl_logical_vma_ledger *, uint64_t guest_first, uint64_t length, uint32_t protection,
                                  int fd, uint64_t file_offset, size_t host_page, hl_logical_vma_plan **);
int hl_logical_vma_prepare_protect(hl_logical_vma_ledger *, uint64_t guest_first, uint64_t length, uint32_t protection,
                                   hl_logical_vma_plan **);
void hl_logical_vma_commit_shared(hl_logical_vma_plan *);
void hl_logical_vma_abort_shared(hl_logical_vma_plan *);

/* Remove exactly the guest-visible interval, splitting surviving VMAs. */
int hl_logical_vma_unmap(hl_logical_vma_ledger *, uint64_t guest_first, uint64_t length);
int hl_logical_vma_protect(hl_logical_vma_ledger *, uint64_t guest_first, uint64_t length, uint32_t protection);

/*
 * Resolve one guest access into the canonical backing window.  The returned
 * pointer remains valid until hl_logical_vma_reclaim_quiescent().  Lookup is
 * lock-free and allocation-free so generated memory paths can inline the same
 * immutable snapshot traversal.  contiguous stops at the logical VMA boundary.
 */
int hl_logical_vma_resolve(hl_logical_vma_ledger *, uint64_t guest_address, size_t length, uint32_t required_protection,
                           hl_logical_vma_resolution *);

/*
 * Release replaced snapshots and backing windows.  The caller must guarantee
 * that no reader can still hold a pointer returned before the most recent
 * mapping transaction (the engine's stop-the-world mapping transition does).
 */
void hl_logical_vma_reclaim_quiescent(hl_logical_vma_ledger *);

size_t hl_logical_vma_count(hl_logical_vma_ledger *);

/* Process-global ledger used by the production Linux personality. */
int hl_logical_vma_global_map_shared(uint64_t guest_first, uint64_t length, uint32_t protection, int fd,
                                     uint64_t file_offset, size_t host_page);
/* Publish guest-page protection over existing engine-owned storage. */
int hl_logical_vma_global_map_direct(uint64_t guest_first, uint64_t length, uint32_t protection, uint64_t host_first);
/* Restore-only construction keeps canonical storage writable while publishing
 * the exact guest protection recorded in the image. */
int hl_logical_vma_global_restore_shared(uint64_t guest_first, uint64_t length, uint32_t protection, int fd,
                                         uint64_t file_offset, size_t host_page);
int hl_logical_vma_global_prepare_shared(uint64_t guest_first, uint64_t length, uint32_t protection, int fd,
                                         uint64_t file_offset, size_t host_page, hl_logical_vma_plan **);
int hl_logical_vma_global_prepare_direct(uint64_t guest_first, uint64_t length, uint32_t protection,
                                         uint64_t host_first, hl_logical_vma_plan **);
int hl_logical_vma_global_prepare_protect(uint64_t guest_first, uint64_t length, uint32_t protection,
                                          hl_logical_vma_plan **);
int hl_logical_vma_global_unmap(uint64_t guest_first, uint64_t length);
int hl_logical_vma_global_protect(uint64_t guest_first, uint64_t length, uint32_t protection);
int hl_logical_vma_global_overlap(uint64_t guest_first, uint64_t length);
void hl_logical_vma_global_reset_quiescent(void);
void hl_logical_vma_global_reclaim_quiescent(void);

/* Translator instruction-fetch seam. See hl_logical_vma_resolve(). */
int hl_logical_vma_resolve_exec(uint64_t guest, size_t length, const void **host, size_t *contiguous);
/*
 * Same tri-state as hl_logical_vma_resolve_exec, but reports the *maximal*
 * guest interval [first,last) over which the answer holds -- the view for a
 * hit, the gap between neighbouring views for a miss -- so a caller can cache
 * one resolution and reuse it for every address inside it.  *generation is the
 * mapping generation the answer was derived from, read before the snapshot so
 * a concurrent publication can only make a cached answer look stale.
 */
int hl_logical_vma_resolve_exec_span(uint64_t guest, uint64_t *generation, uint64_t *first, uint64_t *last,
                                     uint64_t *delta);
/* Address of that generation counter; stable for the process lifetime. */
const _Atomic uint64_t *hl_logical_vma_global_exec_generation(void);
int hl_logical_vma_resolve_data(uint64_t guest, size_t length, uint32_t required, void **host, size_t *contiguous);
typedef void (*hl_logical_vma_alias_visitor)(uint64_t guest_first, uint64_t guest_last, void *opaque);
int hl_logical_vma_pin_data(uint64_t guest, size_t length, uint32_t required, hl_logical_vma_pin *pin);
void hl_logical_vma_unpin(hl_logical_vma_pin *pin);
void hl_logical_vma_visit_exec_aliases_pin(const hl_logical_vma_pin *pin, size_t offset, size_t length,
                                           hl_logical_vma_alias_visitor visitor, void *opaque);
int hl_logical_vma_global_active(void);
size_t hl_logical_vma_global_export(hl_logical_vma_descriptor *, size_t);
int hl_logical_vma_global_describe(uint64_t guest, hl_logical_vma_descriptor *);
int hl_logical_vma_global_copy_out(uint64_t guest, void *destination, size_t length);
int hl_logical_vma_global_copy_in(uint64_t guest, const void *source, size_t length);
const _Atomic(hl_logical_vma_snapshot *) *hl_logical_vma_global_snapshot_source(void);
int hl_logical_vma_visit_exec_aliases(uint64_t guest_first, uint64_t guest_last, hl_logical_vma_alias_visitor visitor,
                                      void *opaque);
int hl_logical_vma_has_exec_alias_file(uint64_t device, uint64_t inode, uint64_t file_offset, size_t length);

/* Deterministic allocation-failure seam for the transaction regression. */
void hl_logical_vma_test_fail_next_prepare(void);

#endif
