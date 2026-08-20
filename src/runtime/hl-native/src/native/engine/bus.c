#include "bus.h"

void hl_guest_bus_init(hl_guest_bus *b, const hl_guest_bus_ops *o, void *c) {
    b->query = 0;
    b->ops = o;
    b->context = c;
    atomic_init(&b->state, 0);
    atomic_init(&b->latched, 0);
}

#define BUS_STATE(generation, active) (((generation) << 1) | (uint64_t)((active) != 0))
#define BUS_GENERATION(state) ((state) >> 1)
#define BUS_ACTIVE(state) ((int)((state) & UINT64_C(1)))

/* The ledger arms for the duration of a mapping transaction and disarms again
   when the transaction leaves no past-EOF range behind, so this must follow it
   back down.  A one-way latch keeps every later translation carrying memory
   guards for a ledger that is empty, which is most of a dynamically linked
   guest's life: ld.so covers its inter-segment holes with PROT_NONE, and those
   ranges park rather than arm.

   Two properties make falling sound.

   Guarded code stays correct while disarmed.  A guard is a runtime query -- it
   loads OFF_BUS_FORCE and skips when the ledger is empty -- so blocks compiled
   while armed need no invalidation on 1 -> 0.  Only newly translated blocks
   change shape, which is the whole of the win.

   Unguarded code cannot survive a re-arm.  On 0 -> 1 the activate callback is
   stop-the-world: it parks every translated peer at a dispatcher boundary and
   rotates the code arena, discarding every block translated while disarmed,
   and it is serialized against translation by the dispatcher lock.  This
   publishes the arm before calling it, so a translator either observes the arm
   and emits guards, or holds the dispatcher lock and installs into the arena
   the flush then retires.  A block translated unguarded is therefore
   unreachable once the ledger is armed, and a genuinely past-EOF access inside
   it cannot escape its SIGBUS.

   Two things the latch was also covering, both now held explicitly.

   Notification order.  `generation` and `enabled` were separate words, so a
   disarm that lost the race on the generation could still land its `enabled`
   store on top of a newer arm.  That was harmless only because the disarm did
   nothing.  They are one word now, advanced by compare-exchange, so the newest
   generation wins and an arm wins a same-generation tie.

   Activation failure.  The stop-the-world flush can fail -- arena reservation
   is an allocation.  A disarm that cannot be undone by a later flush would
   leave unguarded blocks live under an armed ledger, which is a missed SIGBUS
   and strictly worse than the guards it saves.  So a bus that has ever failed
   to invalidate, or that has no invalidation callback at all, latches armed for
   the rest of the process -- exactly the old behaviour, reached only when the
   mechanism the fast path depends on is unavailable. */
void hl_guest_bus_changed(hl_guest_bus *b, uint64_t g, int active) {
    active = active != 0;
    if (!active && atomic_load_explicit(&b->latched, memory_order_acquire)) return;
    uint64_t next = BUS_STATE(g, active);
    uint64_t seen = atomic_load_explicit(&b->state, memory_order_acquire);
    for (;;) {
        if (BUS_GENERATION(seen) > g) return;                      // a newer notification already published
        if (seen == next) return;                                  // already published
        if (BUS_GENERATION(seen) == g && BUS_ACTIVE(seen)) return; // an arm wins a same-generation tie
        if (atomic_compare_exchange_weak_explicit(&b->state, &seen, next, memory_order_acq_rel, memory_order_acquire))
            break;
    }
    if (!active || BUS_ACTIVE(seen)) return; // no 0 -> 1 edge, so nothing to invalidate
    if (b->ops && b->ops->activate && b->ops->activate(b->context)) return;
    atomic_store_explicit(&b->latched, 1, memory_order_release);
}

/* The persistence boundary.  A run that PRODUCES a persistent translated-code
   image, or one that CONSUMES one, arms the ledger and latches it before the
   guest entry point is reached, for the whole life of the process.

   Two properties are being bought, and both are about the 0 -> 1 edge rather
   than about guards.

   Every persisted block carries guards.  A saved arena outlives the process
   that wrote it, so the ledger state it was translated under is not knowable
   by the process that restores it.  Arming the producer makes "guarded" an
   invariant of the file format instead of a property of one run's mapping
   history.

   No later activation edge exists to rotate on.  `hl_guest_bus_changed` only
   invokes `activate` on a 0 -> 1 transition, so a bus that is already armed
   absorbs every later arm without a stop-the-world flush, and the latch
   absorbs every disarm.  Without this, `ld.so`'s first past-EOF library
   mapping arms the bus AFTER `pcache_load` has restored, and the resulting
   rotation discards the entire restored arena -- a warm cache that costs its
   load time and returns nothing.

   This is not a weakening.  It is the same state the bus already enters, for
   the rest of the process, whenever an activation flush fails or no
   invalidation callback is installed; the safety argument above this function
   calls it "exactly the old behaviour".  Arming early can only add guards to
   blocks that would otherwise have had none, and a guard is a runtime query
   against the live ledger, so a guarded block is correct under either ledger
   state.  It costs the disarmed-translation fast path and the codegen
   specializations that decline to run while armed; that is the trade the
   caller opts into by asking for a persistent cache.

   Order matters: latch first, so a disarm racing in from a mapping teardown is
   suppressed rather than landing between the two stores. */
void hl_guest_bus_arm_latched(hl_guest_bus *b) {
    atomic_store_explicit(&b->latched, 1, memory_order_release);
    hl_guest_bus_changed(b, BUS_GENERATION(atomic_load_explicit(&b->state, memory_order_acquire)), 1);
}

void hl_guest_bus_bind(hl_guest_bus *b, hl_guest_bus_query q, int a, uint64_t g) {
    b->query = q;
    hl_guest_bus_changed(b, g, a);
}

int hl_guest_bus_active(const hl_guest_bus *b) {
    return BUS_ACTIVE(atomic_load_explicit(&b->state, memory_order_acquire));
}

uint64_t hl_guest_bus_fault(const hl_guest_bus *b, uint64_t a, uint64_t s) {
    return b->query ? b->query(a, s) : 0;
}

void hl_guest_bus_begin(hl_guest_bus *b) {
    if (b->ops && b->ops->begin) b->ops->begin(b->context);
}

void hl_guest_bus_end(hl_guest_bus *b) {
    if (b->ops && b->ops->end) b->ops->end(b->context);
}
