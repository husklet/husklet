static void gbus_lock(void) {
    while (atomic_flag_test_and_set_explicit(&g_bus_lock, memory_order_acquire))
        sched_yield();
}

static void gbus_unlock(void) {
    atomic_flag_clear_explicit(&g_bus_lock, memory_order_release);
}

static void gbus_filter_rebuild_locked(void) {
    uint64_t lo = UINT64_MAX, hi = 0;
    if (g_bus_fail_closed || g_bus_prepares != 0) {
        lo = 0;
        hi = UINT64_MAX;
    } else {
        for (int index = 0; index < g_ngbus; ++index) {
            if (g_gbus[index].lo < lo) lo = g_gbus[index].lo;
            if (g_gbus[index].hi > hi) hi = g_gbus[index].hi;
        }
    }
    atomic_store_explicit(&g_bus_filter_lo, lo, memory_order_relaxed);
    atomic_store_explicit(&g_bus_filter_hi, hi, memory_order_release);
}

static unsigned gbus_page_hash(uint64_t page) {
    return (unsigned)page & (BUS_FILTER_BITS - 1u);
}

static void gbus_page_mark_locked(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    uint64_t first = lo >> 12;
    uint64_t last = (hi - 1) >> 12;
    /* Mark the preceding page too: a single instruction may begin there and
       cross into the first BUS page.  Guards then need only hash their start. */
    if (first != 0) first--;
    if (last - first >= BUS_FILTER_BITS) {
        for (unsigned i = 0; i < BUS_FILTER_WORDS; ++i)
            atomic_store_explicit(&g_bus_page_filter[i], UINT64_MAX, memory_order_release);
        return;
    }
    for (uint64_t page = first;; ++page) {
        unsigned bit = gbus_page_hash(page);
        atomic_fetch_or_explicit(&g_bus_page_filter[bit >> 6], UINT64_C(1) << (bit & 63u), memory_order_release);
        if (page == last) break;
    }
}

static void gbus_page_reset_locked(void) {
    for (unsigned i = 0; i < BUS_FILTER_WORDS; ++i)
        atomic_store_explicit(&g_bus_page_filter[i], 0, memory_order_release);
}

static void gbus_page_rebuild_locked(void) {
    gbus_page_reset_locked();
    if (g_bus_fail_closed) {
        for (unsigned i = 0; i < BUS_FILTER_WORDS; ++i)
            atomic_store_explicit(&g_bus_page_filter[i], UINT64_MAX, memory_order_release);
        return;
    }
    for (int index = 0; index < g_ngbus; ++index)
        gbus_page_mark_locked(g_gbus[index].lo, g_gbus[index].hi);
}

static void gbus_atfork_prepare(void) {
    pthread_mutex_lock(&g_bus_transition);
    gbus_lock();
}

static void gbus_atfork_parent(void) {
    gbus_unlock();
    pthread_mutex_unlock(&g_bus_transition);
}

static void gbus_atfork_child(void) {
    gbus_unlock();
    pthread_mutex_unlock(&g_bus_transition);
}

static void gbus_atfork_install(void) {
    (void)pthread_atfork(gbus_atfork_prepare, gbus_atfork_parent, gbus_atfork_child);
}

static void gbus_notify(uint64_t generation, int active) {
    gbus_lock();
    hl_linux_bus_change_fn callback = g_bus_callback;
    void *opaque = g_bus_callback_opaque;
    gbus_unlock();
    if (callback != NULL) callback(opaque, generation, active);
}

static void gbus_prepare(void) {
    (void)pthread_once(&g_bus_atfork_once, gbus_atfork_install);
    pthread_mutex_lock(&g_bus_transition);
    gbus_lock();
    int was_active = g_ngbus != 0 || g_bus_fail_closed || g_bus_prepares != 0;
    uint64_t generation = !was_active ? atomic_fetch_add_explicit(&g_bus_generation, 1, memory_order_release) + 1
                                      : atomic_load_explicit(&g_bus_generation, memory_order_relaxed);
    gbus_unlock();
    /* On first activation guarded code does not exist yet.  Complete the
       synchronous STW while the old empty ledger remains queryable; forcing
       queries to wait on a prepare here deadlocks a peer inside its guard
       before that peer can acknowledge the STW.  The caller has not changed
       the host mapping yet, so the old empty answer remains correct. */
    if (!was_active) gbus_notify(generation, 1);
    if (g_bus_transition_begin != NULL) g_bus_transition_begin(g_bus_transition_opaque);
    gbus_lock();
    /* From this point through host mapping publication and ledger commit,
       already-guarded code must use the precise transition path. */
    atomic_store_explicit(&g_bus_filter_force, 3, memory_order_release);
    if (g_bus_prepares != UINT32_MAX)
        g_bus_prepares++;
    else
        g_bus_fail_closed = 1;
    gbus_unlock();
    /* Keep the transition lock through host publication and commit/release. This serializes
       concurrent mapping transactions and prevents fork from inheriting an orphan prepare token. */
}

static void gbus_prepare_release(void) {
    gbus_lock();
    if (g_bus_prepares != 0) g_bus_prepares--;
    /* force remains set until publication below, so no translated guard can
       observe the temporary zeroes.  Rebuilding on every completed mapping
       transaction prevents a long-lived range plus distinct-page churn from
       monotonically saturating the fast rejection filter. */
    gbus_page_rebuild_locked();
    gbus_filter_rebuild_locked();
    int active = g_ngbus != 0 || g_bus_fail_closed || g_bus_prepares != 0;
    uint64_t generation = !active ? atomic_fetch_add_explicit(&g_bus_generation, 1, memory_order_release) + 1
                                  : atomic_load_explicit(&g_bus_generation, memory_order_relaxed);
    gbus_unlock();
    atomic_store_explicit(&g_bus_filter_force, active ? 1 : 0, memory_order_release);
    if (!active) gbus_notify(generation, 0);
    if (g_bus_transition_end != NULL) g_bus_transition_end(g_bus_transition_opaque);
    pthread_mutex_unlock(&g_bus_transition);
}

/* A host MAP_FIXED replacement must not run concurrently with a translated
   peer accessing the replaced range.  This is only a mapping transaction: it
   deliberately does not activate BUS instrumentation or change the ledger. */
static void gbus_mapping_transition_lock(void) {
    (void)pthread_once(&g_bus_atfork_once, gbus_atfork_install);
    pthread_mutex_lock(&g_bus_transition);
}

static void gbus_mapping_stw_begin(void) {
    if (g_bus_transition_begin != NULL) g_bus_transition_begin(g_bus_transition_opaque);
}

static void gbus_mapping_stw_end(void) {
    if (g_bus_transition_end != NULL) g_bus_transition_end(g_bus_transition_opaque);
}

static void gbus_mapping_transition_unlock(void) {
    pthread_mutex_unlock(&g_bus_transition);
}

static void gbus_mapping_prepare(void) {
    gbus_mapping_transition_lock();
    gbus_mapping_stw_begin();
}

static void gbus_mapping_prepare_release(void) {
    gbus_mapping_stw_end();
    gbus_mapping_transition_unlock();
}

int hl_linux_bus_transition_begin(hl_linux_bus_transition *transition) {
    if (transition == NULL || transition->held != 0) return -1;
    gbus_prepare();
    transition->generation = atomic_load_explicit(&g_bus_generation, memory_order_acquire);
    transition->held = 1;
    return 0;
}

int hl_linux_bus_transition_add(hl_linux_bus_transition *transition, uint64_t lo, uint64_t hi) {
    if (transition == NULL || transition->held == 0) return -1;
    return gbus_add(lo, hi);
}

void hl_linux_bus_transition_clear(hl_linux_bus_transition *transition, uint64_t lo, uint64_t hi) {
    if (transition != NULL && transition->held != 0) gbus_clear(lo, hi);
}

void hl_linux_bus_transition_end(hl_linux_bus_transition *transition) {
    if (transition == NULL || transition->held == 0) return;
    transition->held = 0;
    gbus_prepare_release();
    transition->generation = atomic_load_explicit(&g_bus_generation, memory_order_acquire);
}

void hl_linux_bus_set_change_callback(hl_linux_bus_change_fn callback, void *opaque) {
    gbus_lock();
    g_bus_callback_opaque = opaque;
    g_bus_callback = callback;
    uint64_t generation = atomic_load_explicit(&g_bus_generation, memory_order_acquire);
    int active = g_ngbus != 0 || g_bus_fail_closed || g_bus_prepares != 0;
    gbus_unlock();
    if (callback != NULL) callback(opaque, generation, active);
}

void hl_linux_bus_set_transition_callbacks(hl_linux_bus_transition_fn begin, hl_linux_bus_transition_fn end,
                                           void *opaque) {
    pthread_mutex_lock(&g_bus_transition);
    g_bus_transition_begin = begin;
    g_bus_transition_end = end;
    g_bus_transition_opaque = opaque;
    pthread_mutex_unlock(&g_bus_transition);
}

static int gbus_add(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return 0;
    (void)pthread_once(&g_bus_atfork_once, gbus_atfork_install);
    gbus_lock();
    (void)gbus_clear_locked(lo, hi);
    (void)gbus_parked_clear_locked(lo, hi); // a fresh arm supersedes any parked coverage here
    int ok = g_ngbus < GNA_MAX;
    if (ok)
        g_gbus[g_ngbus++] = (struct guest_bus_range){lo, hi};
    else
        g_bus_fail_closed = 1;
    gbus_page_mark_locked(lo, hi);
    gbus_filter_rebuild_locked();
    uint64_t generation = atomic_fetch_add_explicit(&g_bus_generation, 1, memory_order_release) + 1;
    int active = g_ngbus != 0 || g_bus_fail_closed || g_bus_prepares != 0;
    atomic_store_explicit(&g_bus_filter_force, g_bus_prepares != 0 ? 3 : (active ? 1 : 0), memory_order_release);
    gbus_unlock();
    gbus_notify(generation, active);
    return ok ? 0 : -1;
}

static void gbus_clear(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    gbus_lock();
    int changed = gbus_clear_locked(lo, hi);
    /* The range is genuinely gone (unmapped, replaced, or grown back into the
       file), so parked coverage must go with it: a later mapping reusing this
       address must never resurrect the old past-EOF verdict. */
    (void)gbus_parked_clear_locked(lo, hi);
    if (changed && g_ngbus == 0 && !g_bus_fail_closed) gbus_page_reset_locked();
    if (changed) gbus_filter_rebuild_locked();
    uint64_t generation = changed ? atomic_fetch_add_explicit(&g_bus_generation, 1, memory_order_release) + 1
                                  : atomic_load_explicit(&g_bus_generation, memory_order_relaxed);
    int active = g_ngbus != 0 || g_bus_fail_closed || g_bus_prepares != 0;
    if (changed)
        atomic_store_explicit(&g_bus_filter_force, g_bus_prepares != 0 ? 3 : (active ? 1 : 0), memory_order_release);
    gbus_unlock();
    if (changed) gbus_notify(generation, active);
}

/* mprotect(PROT_NONE): the guest cannot reach these bytes at all, and Linux
   classifies a touch as a permission fault long before it consults the page
   cache, so no SIGBUS can be raised here.  Move the ledger's coverage of
   [lo,hi) aside instead of arming the translated guard for it.  Purely a
   relaxation of the live set, so it needs no prepare/STW -- exactly like
   gbus_clear. */
static void gbus_park(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    gbus_lock();
    int changed = 0;
    for (int index = 0; index < g_ngbus; ++index) {
        uint64_t base = g_gbus[index].lo, end = g_gbus[index].hi;
        if (lo >= end || hi <= base) continue;
        gbus_parked_append_locked(base > lo ? base : lo, end < hi ? end : hi);
        changed = 1;
    }
    if (changed) {
        (void)gbus_clear_locked(lo, hi);
        if (g_ngbus == 0 && !g_bus_fail_closed) gbus_page_reset_locked();
        gbus_filter_rebuild_locked();
    }
    uint64_t generation = changed ? atomic_fetch_add_explicit(&g_bus_generation, 1, memory_order_release) + 1
                                  : atomic_load_explicit(&g_bus_generation, memory_order_relaxed);
    int active = g_ngbus != 0 || g_bus_fail_closed || g_bus_prepares != 0;
    if (changed)
        atomic_store_explicit(&g_bus_filter_force, g_bus_prepares != 0 ? 3 : (active ? 1 : 0), memory_order_release);
    gbus_unlock();
    if (changed) gbus_notify(generation, active);
}

static int gbus_parked_overlap(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return 0;
    gbus_lock();
    int found = 0;
    for (int index = 0; index < g_ngbus_parked; ++index)
        if (lo < g_gbus_parked[index].hi && hi > g_gbus_parked[index].lo) {
            found = 1;
            break;
        }
    gbus_unlock();
    return found;
}

/* mprotect back to an accessible protection restores the SIGBUS contract for
   the still-past-EOF bytes: move the parked coverage of [lo,hi) back into the
   live ledger.  This ARMS the guard, so the caller wraps it in the same
   prepare/STW transaction a mapping that arms the ledger uses. */
static void gbus_unpark(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    gbus_lock();
    int changed = 0;
    for (int index = 0; index < g_ngbus_parked;) {
        uint64_t base = g_gbus_parked[index].lo, end = g_gbus_parked[index].hi;
        if (lo >= end || hi <= base) {
            index++;
            continue;
        }
        uint64_t first = base > lo ? base : lo, last = end < hi ? end : hi;
        /* Removing the intersection can split entry `index` and move entries
           behind it, so rescan from the start; each pass strictly shrinks the
           parked coverage of [lo,hi), so this terminates. */
        (void)gbus_parked_clear_locked(first, last);
        (void)gbus_clear_locked(first, last);
        if (g_ngbus < GNA_MAX)
            g_gbus[g_ngbus++] = (struct guest_bus_range){first, last};
        else
            g_bus_fail_closed = 1;
        gbus_page_mark_locked(first, last);
        changed = 1;
        index = 0;
    }
    if (changed) gbus_filter_rebuild_locked();
    uint64_t generation = changed ? atomic_fetch_add_explicit(&g_bus_generation, 1, memory_order_release) + 1
                                  : atomic_load_explicit(&g_bus_generation, memory_order_relaxed);
    int active = g_ngbus != 0 || g_bus_fail_closed || g_bus_prepares != 0;
    if (changed)
        atomic_store_explicit(&g_bus_filter_force, g_bus_prepares != 0 ? 3 : (active ? 1 : 0), memory_order_release);
    gbus_unlock();
    if (changed) gbus_notify(generation, active);
}

uint64_t hl_linux_bus_fault(uint64_t address, uint64_t length) {
    if (length == 0) return 0;
    if (address > UINT64_MAX - length) return address != 0 ? address : 1;
    uint64_t end = address + length;
    if (atomic_load_explicit(&g_bus_filter_force, memory_order_acquire) != 3) {
        uint64_t lo = atomic_load_explicit(&g_bus_filter_lo, memory_order_relaxed);
        uint64_t hi = atomic_load_explicit(&g_bus_filter_hi, memory_order_acquire);
        if (address >= hi || end <= lo) return 0;
    }
retry:
    gbus_lock();
    /* A prepare spans activation, host mapping publication, and precise-ledger
       commit.  Wait out that short transaction rather than treating every
       address as BUS: unrelated translated threads must not receive a
       synchronous SIGBUS merely because a mapper is between publication and
       ledger insertion. */
    if (g_bus_prepares != 0) {
        gbus_unlock();
        sched_yield();
        goto retry;
    }
    if (g_bus_fail_closed) {
        gbus_unlock();
        return address != 0 ? address : 1;
    }
    for (int index = 0; index < g_ngbus; ++index)
        if (address < g_gbus[index].hi && end > g_gbus[index].lo) {
            uint64_t fault = address > g_gbus[index].lo ? address : g_gbus[index].lo;
            gbus_unlock();
            return fault != 0 ? fault : 1;
        }
    gbus_unlock();
    return 0;
}

int hl_linux_bus_hit(uint64_t address, uint64_t length) {
    return hl_linux_bus_fault(address, length) != 0;
}

uint64_t hl_linux_bus_generation(void) {
    return atomic_load_explicit(&g_bus_generation, memory_order_acquire);
}

int hl_linux_bus_active(void) {
    gbus_lock();
    int active = g_ngbus != 0 || g_bus_fail_closed || g_bus_prepares != 0;
    gbus_unlock();
    return active;
}

static void gna_add(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    gna_writer_lock();
    atomic_fetch_add_explicit(&g_gna_generation, 1, memory_order_acq_rel);
    gna_clear_raw(lo, hi); // coalesce inside the same odd-generation transaction
    int count = __atomic_load_n(&g_ngna, __ATOMIC_RELAXED);
    if (count < GNA_MAX) {
        __atomic_store_n(&g_gna[count].lo, lo, __ATOMIC_RELAXED);
        __atomic_store_n(&g_gna[count].hi, hi, __ATOMIC_RELAXED);
        __atomic_store_n(&g_ngna, count + 1, __ATOMIC_RELEASE);
        uint64_t first = atomic_load_explicit(&g_gna_filter_first, memory_order_relaxed);
        uint64_t last = atomic_load_explicit(&g_gna_filter_last, memory_order_relaxed);
        if (lo < first) atomic_store_explicit(&g_gna_filter_first, lo, memory_order_relaxed);
        if (hi > last) atomic_store_explicit(&g_gna_filter_last, hi, memory_order_relaxed);
    }
    atomic_fetch_add_explicit(&g_gna_generation, 1, memory_order_release);
    gna_writer_unlock();
}

static void gna_filter(uint64_t *first, uint64_t *last) {
    uint64_t low = atomic_load_explicit(&g_gna_filter_first, memory_order_acquire);
    uint64_t high = atomic_load_explicit(&g_gna_filter_last, memory_order_acquire);
    if (low < *first) *first = low;
    if (high > *last) *last = high;
}

// Remove [lo,hi) from the set (access granted, or the range unmapped/re-mapped), splitting any interval
// that straddles the boundary so a partial grant (mprotect of a sub-range of a big PROT_NONE reservation)
// keeps the still-inaccessible remainder tracked.
static void gna_clear(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    gna_writer_lock();
    atomic_fetch_add_explicit(&g_gna_generation, 1, memory_order_acq_rel);
    gna_clear_raw(lo, hi);
    atomic_fetch_add_explicit(&g_gna_generation, 1, memory_order_release);
    gna_writer_unlock();
}

static void gna_clear_raw(uint64_t lo, uint64_t hi) {
    int count = __atomic_load_n(&g_ngna, __ATOMIC_RELAXED);
    for (int i = 0; i < count;) {
        uint64_t b = __atomic_load_n(&g_gna[i].lo, __ATOMIC_RELAXED);
        uint64_t e = __atomic_load_n(&g_gna[i].hi, __ATOMIC_RELAXED);
        if (lo >= e || hi <= b) {
            i++;
            continue;
        }
        int keep_head = b < lo, keep_tail = hi < e;
        if (!keep_head && !keep_tail) {
            --count;
            __atomic_store_n(&g_gna[i].lo, __atomic_load_n(&g_gna[count].lo, __ATOMIC_RELAXED), __ATOMIC_RELAXED);
            __atomic_store_n(&g_gna[i].hi, __atomic_load_n(&g_gna[count].hi, __ATOMIC_RELAXED), __ATOMIC_RELAXED);
            __atomic_store_n(&g_ngna, count, __ATOMIC_RELEASE);
            continue;
        }
        if (keep_head)
            __atomic_store_n(&g_gna[i].hi, lo, __ATOMIC_RELAXED); // trim to the surviving head [b,lo)
        else
            __atomic_store_n(&g_gna[i].lo, hi, __ATOMIC_RELAXED); // keep_tail only: [hi,e)
        if (keep_head && keep_tail && count < GNA_MAX) {          // middle grant -> tail becomes a 2nd entry
            __atomic_store_n(&g_gna[count].lo, hi, __ATOMIC_RELAXED);
            __atomic_store_n(&g_gna[count].hi, e, __ATOMIC_RELAXED);
            __atomic_store_n(&g_ngna, ++count, __ATOMIC_RELEASE);
        }
        i++;
    }
}

// True iff any byte of [a,a+len) lies in a tracked guest PROT_NONE region.
static int gna_hit(uint64_t a, uint64_t len) {
    if (!len || __atomic_load_n(&g_ngna, __ATOMIC_ACQUIRE) == 0) return 0;
    a = nonpie_unfold(a); // registry keys are guest coordinates; callers may hold either (see the rule above)
    uint64_t end = a + len;
    uint64_t first_page = a >> 12, last_page = (end - 1) >> 12;
    uint32_t slot = (uint32_t)(first_page * 2654435761u) & (GNA_NEGATIVE_N - 1);
    for (int attempt = 0; attempt < 4096; ++attempt) {
        uint64_t generation = atomic_load_explicit(&g_gna_generation, memory_order_acquire);
        if (generation & 1) {
            sched_yield();
            continue;
        }
        if (first_page == last_page && g_gna_negative_generation[slot] == generation &&
            g_gna_negative_page[slot] == first_page &&
            atomic_load_explicit(&g_gna_generation, memory_order_acquire) == generation)
            return 0;
        int count = __atomic_load_n(&g_ngna, __ATOMIC_ACQUIRE);
        int hit = 0;
        for (int i = 0; i < count; ++i) {
            uint64_t lo = __atomic_load_n(&g_gna[i].lo, __ATOMIC_RELAXED);
            uint64_t hi = __atomic_load_n(&g_gna[i].hi, __ATOMIC_RELAXED);
            if (a < hi && end > lo) {
                hit = 1;
                break;
            }
        }
        if (atomic_load_explicit(&g_gna_generation, memory_order_acquire) != generation) continue;
        if (!hit && first_page == last_page) {
            g_gna_negative_page[slot] = first_page;
            g_gna_negative_generation[slot] = generation;
        }
        return hit;
    }
    return 1;
}

// True iff EVERY guest page of [a,a+len) is in a tracked guest PROT_NONE region -- the whole-MAPPING
// question, which gna_hit ("any byte") must not be used for: a glibc pthread stack is one mmap whose first
// page is the guard. Walks pages: gna_add does not coalesce a piecewise-mprotect'd reservation's intervals.
static int gna_all(uint64_t a, uint64_t len) {
    if (!len || __atomic_load_n(&g_ngna, __ATOMIC_ACQUIRE) == 0) return 0;
    uint64_t end = a + len;
    for (uint64_t page = a & ~(uint64_t)0xfff; page < end; page += 0x1000)
        if (!gna_hit(page, 1)) return 0;
    return 1;
}

// How many LEADING bytes of [a,a+len) are outside every tracked guest PROT_NONE region. Linux's
// copy_to_user is byte-granular: a read(2) whose destination straddles a PROT_NONE page copies the good
// prefix and returns that SHORT count, reporting EFAULT only when the prefix is empty. gna_hit alone
// cannot express that (it is all-or-nothing), so the read family clamps its count with this instead.
static uint64_t gna_prefix(uint64_t a, uint64_t len) {
    if (!len || __atomic_load_n(&g_ngna, __ATOMIC_ACQUIRE) == 0) return len;
    a = nonpie_unfold(a); // guest-keyed registry; the return is a LENGTH, so the coordinate cancels
    for (int attempt = 0; attempt < 4096; ++attempt) {
        uint64_t generation = atomic_load_explicit(&g_gna_generation, memory_order_acquire);
        if (generation & 1) {
            sched_yield();
            continue;
        }
        uint64_t end = a + len;
        int count = __atomic_load_n(&g_ngna, __ATOMIC_ACQUIRE);
        for (int i = 0; i < count; ++i) {
            uint64_t lo = __atomic_load_n(&g_gna[i].lo, __ATOMIC_RELAXED);
            uint64_t hi = __atomic_load_n(&g_gna[i].hi, __ATOMIC_RELAXED);
            if (a < hi && end > lo) {
                uint64_t first = lo > a ? lo : a;
                if (first - a < end - a) end = first;
            }
        }
        if (atomic_load_explicit(&g_gna_generation, memory_order_acquire) == generation) return end - a;
    }
    return 0;
}

static void gro_add(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    gro_writer_lock();
    atomic_fetch_add_explicit(&g_gro_generation, 1, memory_order_acq_rel);
    gro_clear_raw(lo, hi);
    if (g_ngro < GNA_MAX) {
        __atomic_store_n(&g_gro[g_ngro].lo, lo, __ATOMIC_RELAXED);
        __atomic_store_n(&g_gro[g_ngro].hi, hi, __ATOMIC_RELAXED);
        __atomic_store_n(&g_ngro, g_ngro + 1, __ATOMIC_RELEASE);
    }
    atomic_fetch_add_explicit(&g_gro_generation, 1, memory_order_release);
    gro_writer_unlock();
}

static void gro_clear(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    gro_writer_lock();
    atomic_fetch_add_explicit(&g_gro_generation, 1, memory_order_acq_rel);
    gro_clear_raw(lo, hi);
    atomic_fetch_add_explicit(&g_gro_generation, 1, memory_order_release);
    gro_writer_unlock();
}

static void gro_clear_raw(uint64_t lo, uint64_t hi) {
    for (int i = 0; i < g_ngro;) {
        uint64_t b = __atomic_load_n(&g_gro[i].lo, __ATOMIC_RELAXED);
        uint64_t e = __atomic_load_n(&g_gro[i].hi, __ATOMIC_RELAXED);
        if (lo >= e || hi <= b) {
            i++;
            continue;
        }
        int keep_head = b < lo, keep_tail = hi < e;
        if (!keep_head && !keep_tail) {
            --g_ngro;
            __atomic_store_n(&g_gro[i].lo, __atomic_load_n(&g_gro[g_ngro].lo, __ATOMIC_RELAXED), __ATOMIC_RELAXED);
            __atomic_store_n(&g_gro[i].hi, __atomic_load_n(&g_gro[g_ngro].hi, __ATOMIC_RELAXED), __ATOMIC_RELAXED);
            continue;
        }
        if (keep_head)
            __atomic_store_n(&g_gro[i].hi, lo, __ATOMIC_RELAXED);
        else
            __atomic_store_n(&g_gro[i].lo, hi, __ATOMIC_RELAXED);
        if (keep_head && keep_tail && g_ngro < GNA_MAX) {
            __atomic_store_n(&g_gro[g_ngro].lo, hi, __ATOMIC_RELAXED);
            __atomic_store_n(&g_gro[g_ngro].hi, e, __ATOMIC_RELAXED);
            __atomic_store_n(&g_ngro, g_ngro + 1, __ATOMIC_RELEASE);
        }
        i++;
    }
}

static int gro_hit(uint64_t a, uint64_t len) {
    if (!len || __atomic_load_n(&g_ngro, __ATOMIC_ACQUIRE) == 0) return 0;
    a = nonpie_unfold(a); // guest-keyed registry; a hardware fault address arrives in storage coordinates
    uint64_t end = a + len;
    // RETRY the seqlock instead of answering "read-only" while a writer is mid-update: any concurrent
    // mprotect/mmap (a peer's thread-stack allocation) otherwise EFAULTs an unrelated writable address.
    for (int attempt = 0; attempt < 4096; attempt++) {
        uint64_t generation = atomic_load_explicit(&g_gro_generation, memory_order_acquire);
        if (generation & 1) {
            sched_yield();
            continue;
        }
        int count = __atomic_load_n(&g_ngro, __ATOMIC_ACQUIRE);
        int hit = 0;
        for (int i = 0; i < count; i++) {
            uint64_t lo = __atomic_load_n(&g_gro[i].lo, __ATOMIC_RELAXED);
            uint64_t hi = __atomic_load_n(&g_gro[i].hi, __ATOMIC_RELAXED);
            if (a < hi && end > lo) {
                hit = 1;
                break;
            }
        }
        if (atomic_load_explicit(&g_gro_generation, memory_order_acquire) == generation) return hit;
    }
    return 1; // a writer that never settles: keep the conservative answer
}

// Number of leading bytes before the first guest read-only interval.  Like
// gna_prefix, this answers the span question without touching guest memory.
static uint64_t gro_prefix(uint64_t a, uint64_t len) {
    if (!len || __atomic_load_n(&g_ngro, __ATOMIC_ACQUIRE) == 0) return len;
    a = nonpie_unfold(a);
    for (int attempt = 0; attempt < 4096; ++attempt) {
        uint64_t generation = atomic_load_explicit(&g_gro_generation, memory_order_acquire);
        if (generation & 1) {
            sched_yield();
            continue;
        }
        uint64_t end = a + len;
        int count = __atomic_load_n(&g_ngro, __ATOMIC_ACQUIRE);
        for (int index = 0; index < count; ++index) {
            uint64_t low = __atomic_load_n(&g_gro[index].lo, __ATOMIC_RELAXED);
            uint64_t high = __atomic_load_n(&g_gro[index].hi, __ATOMIC_RELAXED);
            if (a < high && end > low) {
                uint64_t first = low > a ? low : a;
                if (first - a < end - a) end = first;
            }
        }
        if (atomic_load_explicit(&g_gro_generation, memory_order_acquire) == generation) return end - a;
    }
    return 0;
}

static void gnx_writer_lock(void) {
    while (atomic_flag_test_and_set_explicit(&g_gnx_writer, memory_order_acquire))
        sched_yield();
}

static void gnx_writer_unlock(void) {
    atomic_flag_clear_explicit(&g_gnx_writer, memory_order_release);
}

static void gnx_clear_raw(uint64_t lo, uint64_t hi) {
    int count = __atomic_load_n(&g_ngnx, __ATOMIC_RELAXED);
    for (int i = 0; i < count;) {
        uint64_t b = __atomic_load_n(&g_gnx[i].lo, __ATOMIC_RELAXED);
        uint64_t e = __atomic_load_n(&g_gnx[i].hi, __ATOMIC_RELAXED);
        if (lo >= e || hi <= b) {
            ++i;
            continue;
        }
        int keep_head = b < lo, keep_tail = hi < e;
        if (!keep_head && !keep_tail) {
            --count;
            __atomic_store_n(&g_gnx[i].lo, __atomic_load_n(&g_gnx[count].lo, __ATOMIC_RELAXED), __ATOMIC_RELAXED);
            __atomic_store_n(&g_gnx[i].hi, __atomic_load_n(&g_gnx[count].hi, __ATOMIC_RELAXED), __ATOMIC_RELAXED);
            __atomic_store_n(&g_ngnx, count, __ATOMIC_RELEASE);
            continue;
        }
        if (keep_head)
            __atomic_store_n(&g_gnx[i].hi, lo, __ATOMIC_RELAXED);
        else
            __atomic_store_n(&g_gnx[i].lo, hi, __ATOMIC_RELAXED);
        if (keep_head && keep_tail && count < GNA_MAX) {
            __atomic_store_n(&g_gnx[count].lo, hi, __ATOMIC_RELAXED);
            __atomic_store_n(&g_gnx[count].hi, e, __ATOMIC_RELAXED);
            __atomic_store_n(&g_ngnx, ++count, __ATOMIC_RELEASE);
        }
        ++i;
    }
}

static void gnx_add(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    gnx_writer_lock();
    atomic_fetch_add_explicit(&g_gnx_generation, 1, memory_order_acq_rel);
    gnx_clear_raw(lo, hi);
    int count = __atomic_load_n(&g_ngnx, __ATOMIC_RELAXED);
    if (count < GNA_MAX) {
        __atomic_store_n(&g_gnx[count].lo, lo, __ATOMIC_RELAXED);
        __atomic_store_n(&g_gnx[count].hi, hi, __ATOMIC_RELAXED);
        __atomic_store_n(&g_ngnx, count + 1, __ATOMIC_RELEASE);
    }
    atomic_fetch_add_explicit(&g_gnx_generation, 1, memory_order_release);
    gnx_writer_unlock();
}

static void gnx_clear(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    gnx_writer_lock();
    atomic_fetch_add_explicit(&g_gnx_generation, 1, memory_order_acq_rel);
    gnx_clear_raw(lo, hi);
    atomic_fetch_add_explicit(&g_gnx_generation, 1, memory_order_release);
    gnx_writer_unlock();
}

static void gnx_reset(void) {
    gnx_writer_lock();
    atomic_fetch_add_explicit(&g_gnx_generation, 1, memory_order_acq_rel);
    __atomic_store_n(&g_ngnx, 0, __ATOMIC_RELEASE);
    atomic_fetch_add_explicit(&g_gnx_generation, 1, memory_order_release);
    gnx_writer_unlock();
}

typedef struct {
    uint64_t generation;
    uint64_t first;
    uint64_t last;
} guest_exec_page;

static _Thread_local guest_exec_page g_exec_page;
#define GUEST_EXEC_PAGE_CACHE_N 64u
static _Thread_local guest_exec_page g_exec_pages[GUEST_EXEC_PAGE_CACHE_N];
#if HL_NATIVE_TEST_HOOKS
static _Thread_local uint64_t g_gnx_scan_count;
static _Thread_local uint64_t g_guest_exec_validation_count;
#endif

static int gnx_hit(uint64_t a, uint64_t len) {
    if (!len || __atomic_load_n(&g_ngnx, __ATOMIC_ACQUIRE) == 0) return 0;
    a = nonpie_unfold(a);
    uint64_t end = a + len;
    if (end < a) return 1;
    for (int attempt = 0; attempt < 4096; ++attempt) {
        uint64_t generation = atomic_load_explicit(&g_gnx_generation, memory_order_acquire);
        if (generation & 1) {
            sched_yield();
            continue;
        }
        int count = __atomic_load_n(&g_ngnx, __ATOMIC_ACQUIRE);
#if HL_NATIVE_TEST_HOOKS
        g_gnx_scan_count++;
#endif
        int hit = 0;
        for (int i = 0; i < count; ++i) {
            uint64_t lo = __atomic_load_n(&g_gnx[i].lo, __ATOMIC_RELAXED);
            uint64_t hi = __atomic_load_n(&g_gnx[i].hi, __ATOMIC_RELAXED);
            if (a < hi && end > lo) {
                hit = 1;
                break;
            }
        }
        if (atomic_load_explicit(&g_gnx_generation, memory_order_acquire) == generation) return hit;
    }
    return 1;
}

static int guest_exec_direct_valid(uint64_t guest, size_t length) {
#if HL_NATIVE_TEST_HOOKS
    g_guest_exec_validation_count++;
#endif
    if (length == 0) return 1;
    if (guest > UINT64_MAX - length) return 0;
    uint64_t generation = atomic_load_explicit(&g_gnx_generation, memory_order_acquire);
    uint64_t end = guest + length;
    uint64_t first = guest & ~UINT64_C(4095);
    guest_exec_page *cached = &g_exec_pages[(first >> 12) & (GUEST_EXEC_PAGE_CACHE_N - 1u)];
    if (!(generation & 1) && cached->generation == generation && guest >= cached->first && end <= cached->last)
        return 1;
    if (!(generation & 1) && g_exec_page.generation == generation && guest >= g_exec_page.first &&
        end <= g_exec_page.last)
        return 1;

    if (first <= UINT64_MAX - UINT64_C(4096) && !gnx_hit(first, 4096)) {
        uint64_t confirmed = atomic_load_explicit(&g_gnx_generation, memory_order_acquire);
        if (confirmed == generation && !(confirmed & 1)) {
            g_exec_page = (guest_exec_page){confirmed, first, first + UINT64_C(4096)};
            *cached = g_exec_page;
            return 1;
        }
    }
    return !gnx_hit(guest, length);
}

#if HL_NATIVE_TEST_HOOKS
static void *g_nonpie_collision_mapping;
static int g_nonpie_collision_active;
int HL_TARGET_LOCAL(jit_rollover_mapping_test)(uint64_t *result);

static int nonpie_collision_finish_release(int released) {
    if (released != 0) return -EIO;
    g_nonpie_collision_mapping = NULL;
    g_nonpie_collision_active = 0;
    return 0;
}

HL_API int HL_TARGET_LOCAL(exec_page_cache_test)(uint32_t scenario, uint64_t *scans) {
    if (scans == NULL) return -1;
    void *saved = malloc(sizeof g_gnx);
    if (saved == NULL) return -ENOMEM;
    gnx_writer_lock();
    int saved_count = __atomic_load_n(&g_ngnx, __ATOMIC_RELAXED);
    memcpy(saved, g_gnx, sizeof g_gnx);
    gnx_writer_unlock();
    guest_exec_page saved_page = g_exec_page;
    uint64_t guest_page = UINT64_C(0x40000000);
    guest_page -= nonpie_fold(guest_page) & UINT64_C(4095);
    uint64_t page = nonpie_fold(guest_page);
    gnx_reset();
    gnx_add(guest_page + UINT64_C(0x2000), guest_page + UINT64_C(0x3000));
    g_exec_page = (guest_exec_page){0};
    g_gnx_scan_count = 0;
    int result = 0;
    switch (scenario) {
    case 0: // Stable executable page: one scan, then cache hits.
        for (int i = 0; i < 32; ++i)
            if (!guest_exec_direct_valid(page + (uint64_t)i, 15)) result = -2;
        break;
    case 1: // mprotect/MAP_FIXED removing execute invalidates the warm verdict.
        if (!guest_exec_direct_valid(page, 15)) result = -3;
        gnx_add(guest_page, guest_page + UINT64_C(4096));
        if (guest_exec_direct_valid(page, 15)) result = -4;
        break;
    case 2: // munmap followed by an executable remap invalidates both transitions.
        if (!guest_exec_direct_valid(page, 15)) result = -5;
        gnx_add(guest_page, guest_page + UINT64_C(4096));
        gnx_clear(guest_page, guest_page + UINT64_C(4096));
        if (!guest_exec_direct_valid(page, 15)) result = -6;
        break;
    case 3: // exec reset drops the old image's non-executable ranges and cache verdict.
        if (!guest_exec_direct_valid(page, 15)) result = -7;
        gnx_add(guest_page, guest_page + UINT64_C(4096));
        gnx_reset();
        if (!guest_exec_direct_valid(page, 15)) result = -8;
        break;
    case 4: // A partially non-executable page is never cached as wholly valid.
        gnx_add(guest_page + 128, guest_page + 256);
        if (!guest_exec_direct_valid(page, 15) || !guest_exec_direct_valid(page + 16, 15)) result = -9;
        break;
#if defined(HL_X86_DECODE_MEMO_TEST)
    case 5:
    case 6:
    case 7:
    case 8:
    case 9:
    case 10:
    case 11: result = hl_x86_decode_memo_test(scenario, scans); break;
#endif
    case 12: {
        if (g_nonpie_collision_active) {
            result = -EALREADY;
            break;
        }
#if defined(__linux__)
        void *page = mmap((void *)(uintptr_t)UINT64_C(0x400000), 4096, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
        if (page == MAP_FAILED || page != (void *)(uintptr_t)UINT64_C(0x400000)) {
            if (page != MAP_FAILED) (void)munmap(page, 4096);
            result = -EADDRINUSE;
            break;
        }
        g_nonpie_collision_mapping = page;
        g_nonpie_collision_active = 1;
        *scans = 1;
#else
        result = -ENOTSUP;
#endif
        break;
    }
    case 13: {
        if (!g_nonpie_collision_active) {
            result = -ENOENT;
            break;
        }
#if defined(__linux__)
        int release_result = nonpie_collision_finish_release(munmap(g_nonpie_collision_mapping, 4096));
        if (result == 0) result = release_result;
        *scans = 1;
#else
        result = -ENOTSUP;
#endif
        break;
    }
    case 14: { // A failed host release retains ownership so cleanup can be retried.
        if (!g_nonpie_collision_active) {
            result = -ENOENT;
            break;
        }
        result = nonpie_collision_finish_release(-1);
        if (!g_nonpie_collision_active || g_nonpie_collision_mapping == NULL) result = -EIO;
        break;
    }
    case 15:
    case 16:
    case 17: result = map_host_cache_test(scenario, scans); break;
    case 25: result = map_host_cache_test(scenario, scans); break;
    case 18: result = HL_TARGET_LOCAL(jit_rollover_mapping_test)(scans); break;
    case 19: { // Fetch-span hits reuse the page verdict until its authority changes.
        _Alignas(4096) unsigned char page_bytes[4096] = {0};
        unsigned char byte = 0;
        hl_guest_memory_bind(&g_guest_memory_ops);
        hl_guest_fetch_set_direct_validator(guest_exec_direct_valid);
        hl_guest_fetch_set_direct_generation(&g_gnx_generation);
        g_exec_page = (guest_exec_page){0};
        g_guest_exec_validation_count = 0;
        for (size_t i = 0; i < 32; ++i) {
            if (hl_guest_fetch_exec((uint64_t)(uintptr_t)&page_bytes[i], &byte, 1) != 0 || byte != 0) {
                result = -EIO;
                break;
            }
        }
        *scans = g_guest_exec_validation_count;
        break;
    }
    case 20: { // Two hot executable pages retain independent generation-bound verdicts.
        for (int i = 0; i < 32; ++i) {
            uint64_t address = page + ((uint64_t)(i & 1) << 12);
            if (!guest_exec_direct_valid(address, 15)) {
                result = -EIO;
                break;
            }
        }
        *scans = g_gnx_scan_count;
        break;
    }
#if defined(HL_X86_DECODE_MEMO_TEST)
    case 21: result = hl_x86_hot_context_test(); break;
    case 22: result = hl_x86_hot_context_thread_test(); break;
    case 23: result = hl_x86_hot_context_allocation_test(); break;
    case 26:
    case 27:
    case 28:
    case 29:
    case 30:
    case 31:
    case 32:
    case 33:
    case 34: result = hl_x86_decode_authority_test(scenario, scans); break;
    case 35: result = hl_x86_decode_authority_test(scenario, scans); break;
    case 36: result = hl_x86_decode_authority_test(scenario, scans); break;
#endif
    default: result = -10;
    }
    if (scenario <= 4) *scans = g_gnx_scan_count;
    gnx_writer_lock();
    atomic_fetch_add_explicit(&g_gnx_generation, 1, memory_order_acq_rel);
    memcpy(g_gnx, saved, sizeof g_gnx);
    __atomic_store_n(&g_ngnx, saved_count, __ATOMIC_RELEASE);
    atomic_fetch_add_explicit(&g_gnx_generation, 1, memory_order_release);
    gnx_writer_unlock();
    free(saved);
    g_exec_page = saved_page;
    return result;
}
#endif

// execve replaces the whole address space -> drop all tracked PROT_NONE ranges (they're gone with the old
// image; a stale entry could otherwise wrongly EFAULT a fresh mapping the new image lays at the same address).
static void gna_reset(void) {
    gna_writer_lock();
    atomic_fetch_add_explicit(&g_gna_generation, 1, memory_order_acq_rel);
    __atomic_store_n(&g_ngna, 0, __ATOMIC_RELEASE);
    atomic_store_explicit(&g_gna_filter_first, UINT64_MAX, memory_order_relaxed);
    atomic_store_explicit(&g_gna_filter_last, 0, memory_order_relaxed);
    atomic_fetch_add_explicit(&g_gna_generation, 1, memory_order_release);
    gna_writer_unlock();
    __atomic_store_n(&g_ngro, 0, __ATOMIC_RELEASE);
    gnx_reset();
    pthread_mutex_lock(&g_filemap_lock);
    for (int index = 0; index < g_nfilemap; ++index) {
        int retained = g_filemap[index].fd;
        int first = 1;
        for (int previous = 0; previous < index; ++previous)
            if (g_filemap[previous].fd == retained) first = 0;
        if (first && retained >= 0) close(retained);
    }
    g_nfilemap = 0;
    pthread_mutex_unlock(&g_filemap_lock);
    hl_logical_vma_global_reset_quiescent();
    pthread_mutex_lock(&g_bus_transition);
    gbus_lock();
    int changed = g_ngbus != 0 || g_bus_fail_closed || g_bus_prepares != 0;
    atomic_store_explicit(&g_ngbus, 0, memory_order_release);
    g_ngbus_parked = 0;
    g_bus_fail_closed = 0;
    g_bus_prepares = 0;
    gbus_page_reset_locked();
    gbus_filter_rebuild_locked();
    atomic_store_explicit(&g_bus_filter_force, 0, memory_order_release);
    uint64_t generation = changed ? atomic_fetch_add_explicit(&g_bus_generation, 1, memory_order_release) + 1
                                  : atomic_load_explicit(&g_bus_generation, memory_order_relaxed);
    gbus_unlock();
    if (changed) gbus_notify(generation, 0);
    pthread_mutex_unlock(&g_bus_transition);
    /* Soft mode is intentionally sticky across temporary empty logical-VMA
       intervals.  exec/checkpoint image reset is the lifecycle boundary where
       old guarded translations are no longer useful; rotate once here before
       admitting direct, unguarded translations for the replacement image. */
    jit_guest_soft_deactivate();
}

// True iff host virtual address `a` is currently mapped. mincore() is useless on macOS (returns 0 for ANY
// address), so query the VM map directly: mach_vm_region returns the first region at-or-above `a`, and `a`
// is mapped iff it falls inside [start, start+size). Same technique as the x86 loader's lazy_addr_mapped.
// Used to mirror the kernel's fault-tolerant put_user() on the CLEARTID teardown path (futex_wake_addr).
