#ifndef HL_TRANSLATOR_GUEST_MEMORY_H
#define HL_TRANSLATOR_GUEST_MEMORY_H

#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>

typedef enum {
    HL_GUEST_MEMORY_READ = 1,
    HL_GUEST_MEMORY_WRITE = 2,
} hl_guest_memory_access;

typedef struct {
    void *host;
    size_t contiguous;
    void *token;
} hl_guest_memory_pin;

/*
 * Guest-address indirection seam.
 *
 * A guest address is not always a host address: the Linux ABI's logical-VMA
 * ledger (linux_abi/logical_vma.c) can move it, and a biased non-PIE image
 * moves it again (core/target/x86_64.c's window).  The translator has to honor
 * both without depending on the engine or Linux ABI, so the engine binds the
 * accessors here.
 *
 * Unbound is the honest standalone default and every entry is optional: a
 * translator with no engine under it sees one flat address space where a guest
 * address IS a host address.  That is what the decoder's own unit tests assume.
 */
typedef struct hl_guest_memory_ops {
    /* Executable bytes.  >0: *host and *contiguous describe an indirected mapping.
       0: ordinary, the guest address is the host address.  <0: fault. */
    int (*resolve_exec)(uint64_t guest, size_t length, const void **host, size_t *contiguous);
    /* Data, same tri-state.  0 copies nothing -- the caller owns the ordinary
       case, because only the caller knows which access validator applies. */
    int (*read)(uint64_t guest, void *destination, size_t length);
    int (*write)(uint64_t guest, const void *source, size_t length);
    /* Nonzero once any indirected mapping exists, i.e. the slow element-wise
       path is mandatory. */
    int (*indirect)(void);
    /* Non-PIE bias fold: low link address -> the real high mapping. */
    uint64_t (*host_pointer)(uint64_t guest);
    /* resolve_exec widened to the maximal guest interval [*first,*last) over
       which its answer holds, plus the mapping generation it came from.  Lets
       the fetch path resolve once per mapping instead of once per instruction.
       Optional: without it every fetch resolves, as before. */
    int (*exec_span)(uint64_t guest, uint64_t *generation, uint64_t *first, uint64_t *last, uint64_t *delta);
    /* Address of that generation counter.  A cached span is revalidated against
       it on every use, so no mapping transition has to notify anyone. */
    const _Atomic uint64_t *(*exec_generation)(void);
    /* Stable data span.  The return value has the same tri-state as read/write;
       unpin releases any lifetime token acquired by pin. */
    int (*pin)(uint64_t guest, size_t length, hl_guest_memory_access access, hl_guest_memory_pin *pin);
    void (*unpin)(hl_guest_memory_pin *pin);
} hl_guest_memory_ops;

void hl_guest_memory_bind(const hl_guest_memory_ops *ops);

int hl_guest_memory_resolve_exec(uint64_t guest, size_t length, const void **host, size_t *contiguous);
/* Tri-state of resolve_exec over [*first,*last).  `length` only bounds the
   span an unbound-seam fallback may claim. */
int hl_guest_memory_resolve_exec_span(uint64_t guest, size_t length, uint64_t *generation, uint64_t *first,
                                      uint64_t *last, uint64_t *delta);
/* exec_generation() of the bound ops, resolved once by hl_guest_memory_bind().
   A variable rather than an accessor because the fetch path re-reads it for
   every guest instruction; NULL means no span memo. */
extern const _Atomic uint64_t *hl_guest_memory_generation;
int hl_guest_memory_read(uint64_t guest, void *destination, size_t length);
int hl_guest_memory_write(uint64_t guest, const void *source, size_t length);
int hl_guest_memory_pin_data(uint64_t guest, size_t length, hl_guest_memory_access access, hl_guest_memory_pin *pin);
void hl_guest_memory_unpin_data(hl_guest_memory_pin *pin);
int hl_guest_memory_indirect(void);
uint64_t hl_guest_memory_host_pointer(uint64_t guest);

#endif
