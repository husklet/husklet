#include "ofd_identity.h"

#include <errno.h>
#include <limits.h>
#include <string.h>

#if defined(HL_NATIVE_TEST_HOOKS) && !defined(_WIN32)
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>
#endif

static int ofd_lineage_valid(hl_ofd_lineage lineage) {
    return lineage.high != 0 || lineage.low != 0;
}

static int ofd_lineage_equal(hl_ofd_lineage first, hl_ofd_lineage second) {
    return first.high == second.high && first.low == second.low;
}

int hl_ofd_identity_valid(hl_ofd_identity identity) {
    return ofd_lineage_valid(identity.lineage) && identity.member != 0 && identity.sequence != 0;
}

int hl_ofd_identity_equal(hl_ofd_identity first, hl_ofd_identity second) {
    return first.lineage.high == second.lineage.high && first.lineage.low == second.lineage.low &&
           first.member == second.member && first.sequence == second.sequence;
}

int hl_ofd_identity_record_valid(hl_ofd_identity identity, uint64_t numeric_id) {
#if defined(HL_OFD_MUTATE_SKIP_PREFLIGHT_IDENTITY)
    (void)identity;
    (void)numeric_id;
    return 1;
#else
    return hl_ofd_identity_valid(identity) && numeric_id == identity.sequence;
#endif
}

int hl_ofd_identity_alias_compatible(hl_ofd_identity first, hl_ofd_identity second) {
#if defined(HL_OFD_MUTATE_ACCEPT_COLLISION)
    (void)first;
    (void)second;
    return 1;
#else
    return first.sequence != second.sequence || hl_ofd_identity_equal(first, second);
#endif
}

int hl_ofd_identity_lineage_compatible(hl_ofd_identity first, hl_ofd_identity second) {
#if defined(HL_OFD_MUTATE_ACCEPT_STALE_LINEAGE)
    (void)first;
    (void)second;
    return 1;
#else
    return ofd_lineage_equal(first.lineage, second.lineage);
#endif
}

static int ofd_namespace_valid(const hl_ofd_namespace *space) {
    return space != NULL && atomic_load_explicit(&space->state, memory_order_acquire) == HL_OFD_NAMESPACE_ACTIVE &&
           space->abi == HL_OFD_NAMESPACE_ABI && space->size == sizeof *space && ofd_lineage_valid(space->lineage) &&
           atomic_load_explicit(&space->next_member, memory_order_relaxed) != 0 &&
           atomic_load_explicit(&space->next_sequence, memory_order_relaxed) != 0;
}

int hl_ofd_namespace_init(hl_ofd_namespace *space, size_t size, hl_ofd_lineage lineage, int storage_is_zeroed) {
    if (space == NULL || size != sizeof *space || !ofd_lineage_valid(lineage)) return EINVAL;
    if (!storage_is_zeroed) {
        if (!ofd_namespace_valid(space) || !ofd_lineage_equal(space->lineage, lineage)) return ESTALE;
        return 0;
    }
#if defined(HL_OFD_MUTATE_ALLOW_LIVE_RESET)
    memset(space, 0, sizeof *space);
#endif
    uint64_t empty = HL_OFD_NAMESPACE_EMPTY;
    if (!atomic_compare_exchange_strong_explicit(&space->state, &empty, HL_OFD_NAMESPACE_INITIALIZING,
                                                 memory_order_acq_rel, memory_order_acquire))
        return EALREADY;
    space->abi = HL_OFD_NAMESPACE_ABI;
    space->size = sizeof *space;
    space->lineage = lineage;
    space->generation_high = 0;
    space->generation_low = 0;
    space->generation_fence = 0;
    atomic_init(&space->next_member, 1);
    atomic_init(&space->next_sequence, 1);
    atomic_store_explicit(&space->state, HL_OFD_NAMESPACE_ACTIVE, memory_order_release);
    return 0;
}

int hl_ofd_member_bind(hl_ofd_member *member, hl_ofd_namespace *space, hl_ofd_lineage lineage, uint64_t ordinal) {
    if (member == NULL || ordinal == 0 || !ofd_namespace_valid(space) ||
        ordinal >= atomic_load_explicit(&space->next_member, memory_order_relaxed) ||
        !ofd_lineage_equal(space->lineage, lineage))
        return EINVAL;
    *member = (hl_ofd_member){space, lineage, ordinal};
    return 0;
}

int hl_ofd_member_mint(hl_ofd_namespace *space, hl_ofd_member *member) {
    if (!ofd_namespace_valid(space) || member == NULL) return EINVAL;
    uint64_t ordinal = atomic_load_explicit(&space->next_member, memory_order_relaxed);
    for (;;) {
#if defined(HL_OFD_MUTATE_ALLOW_MEMBER_WRAP)
        uint64_t next = ordinal + 1u;
        if (next == 0) next = 1;
#else
        if (ordinal == 0 || ordinal == UINT64_MAX) return EOVERFLOW;
#endif
        if (atomic_compare_exchange_weak_explicit(&space->next_member, &ordinal,
#if defined(HL_OFD_MUTATE_ALLOW_MEMBER_WRAP)
                                                  next,
#else
                                                  ordinal + 1u,
#endif
                                                  memory_order_relaxed, memory_order_relaxed))
            break;
    }
    *member = (hl_ofd_member){space, space->lineage, ordinal};
    return 0;
}

int hl_ofd_namespace_admit_validated(hl_ofd_namespace *space, hl_ofd_generation_binding binding) {
    if (space == NULL || (binding.generation_high == 0 && binding.generation_low == 0) || binding.fence == 0 ||
        binding.next_member == 0 || binding.next_sequence == 0 || !ofd_lineage_valid(binding.lineage))
        return ESTALE;
    uint64_t active = HL_OFD_NAMESPACE_ACTIVE;
    if (!atomic_compare_exchange_strong_explicit(&space->state, &active, HL_OFD_NAMESPACE_UPDATING,
                                                 memory_order_acq_rel, memory_order_acquire))
        return EBUSY;
    if (space->abi != HL_OFD_NAMESPACE_ABI || space->size != sizeof *space ||
#if !defined(HL_OFD_MUTATE_ACCEPT_REPLAY)
        binding.fence <= space->generation_fence ||
#endif
        !ofd_lineage_equal(space->lineage, binding.lineage)) {
        atomic_store_explicit(&space->state, HL_OFD_NAMESPACE_ACTIVE, memory_order_release);
        return ESTALE;
    }
#if defined(HL_OFD_MUTATE_USE_GENERATION_AS_LINEAGE)
    space->lineage = (hl_ofd_lineage){binding.generation_high, binding.generation_low};
#endif
    uint64_t observed_member = atomic_load_explicit(&space->next_member, memory_order_relaxed);
    uint64_t observed = atomic_load_explicit(&space->next_sequence, memory_order_relaxed);
    if (
#if !defined(HL_OFD_MUTATE_ACCEPT_MEMBER_ROLLBACK)
        binding.next_member < observed_member ||
#endif
#if !defined(HL_OFD_MUTATE_ACCEPT_SEQUENCE_ROLLBACK)
        binding.next_sequence < observed ||
#endif
        observed_member == 0 || observed == 0) {
        atomic_store_explicit(&space->state, HL_OFD_NAMESPACE_ACTIVE, memory_order_release);
        return ESTALE;
    }
#if !defined(HL_OFD_MUTATE_SKIP_MEMBER_HIGH_WATER)
    while (observed_member < binding.next_member &&
           !atomic_compare_exchange_weak_explicit(&space->next_member, &observed_member, binding.next_member,
                                                  memory_order_relaxed, memory_order_relaxed)) {}
#endif
    while (observed < binding.next_sequence &&
           !atomic_compare_exchange_weak_explicit(&space->next_sequence, &observed, binding.next_sequence,
                                                  memory_order_relaxed, memory_order_relaxed)) {}
    if (observed == 0 || observed_member == 0) {
        atomic_store_explicit(&space->state, HL_OFD_NAMESPACE_ACTIVE, memory_order_release);
        return EOVERFLOW;
    }
    space->generation_high = binding.generation_high;
    space->generation_low = binding.generation_low;
    space->generation_fence = binding.fence;
    atomic_store_explicit(&space->state, HL_OFD_NAMESPACE_ACTIVE, memory_order_release);
    return 0;
}

static int ofd_member_valid(const hl_ofd_member *member) {
    return member != NULL && member->ordinal != 0 && ofd_namespace_valid(member->space) &&
           ofd_lineage_equal(member->lineage, member->space->lineage);
}

int hl_ofd_identity_mint(hl_ofd_member *member, hl_ofd_identity *identity) {
    if (!ofd_member_valid(member) || identity == NULL) return EINVAL;
    uint64_t sequence = atomic_load_explicit(&member->space->next_sequence, memory_order_relaxed);
    for (;;) {
#if defined(HL_OFD_MUTATE_ALLOW_WRAP)
        uint64_t next = sequence + 1u;
        if (next == 0) next = 1;
#else
        if (sequence == UINT64_MAX) return EOVERFLOW;
        uint64_t next = sequence + 1u;
#endif
        if (sequence == 0) return EOVERFLOW;
        if (atomic_compare_exchange_weak_explicit(&member->space->next_sequence, &sequence, next, memory_order_relaxed,
                                                  memory_order_relaxed))
            break;
    }
    *identity = (hl_ofd_identity){
#if defined(HL_OFD_MUTATE_DROP_LINEAGE)
        .lineage = {0, 0},
#else
        .lineage = member->lineage,
#endif
#if defined(HL_OFD_MUTATE_DROP_MEMBER)
        .member = 0,
#else
        .member = member->ordinal,
#endif
        .sequence = sequence,
    };
    return 0;
}

int hl_ofd_identity_reattach(hl_ofd_member *member, hl_ofd_identity identity) {
    if (!ofd_member_valid(member) || !hl_ofd_identity_valid(identity) ||
        !hl_ofd_identity_lineage_compatible((hl_ofd_identity){member->lineage, member->ordinal, 1}, identity) ||
        identity.sequence == UINT64_MAX)
        return ESTALE;
    uint64_t observed = atomic_load_explicit(&member->space->next_sequence, memory_order_relaxed);
#if !defined(HL_OFD_MUTATE_SKIP_REATTACH)
    uint64_t required = identity.sequence + 1u;
    while (observed < required &&
           !atomic_compare_exchange_weak_explicit(&member->space->next_sequence, &observed, required,
                                                  memory_order_relaxed, memory_order_relaxed)) {}
#endif
    return observed == 0 ? EOVERFLOW : 0;
}

#if defined(HL_NATIVE_TEST_HOOKS)
static int ofd_fixture_core(uint32_t scenario, hl_ofd_namespace *space) {
    const hl_ofd_lineage lineage = {UINT64_C(0x0123456789abcdef), UINT64_C(0xfedcba9876543210)};
    hl_ofd_member first;
    hl_ofd_generation_binding initial_generation = {.generation_high = 97,
                                                    .generation_low = 89,
                                                    .fence = 1,
                                                    .lineage = lineage,
                                                    .next_member = 8,
                                                    .next_sequence = 1};
    if (hl_ofd_namespace_init(space, sizeof *space, lineage, 1) != 0 ||
        hl_ofd_namespace_admit_validated(space, initial_generation) != 0 ||
        hl_ofd_member_bind(&first, space, lineage, 7) != 0)
        return 10;
    if (scenario == 0) {
        hl_ofd_identity a, b;
        if (hl_ofd_identity_mint(&first, &a) != 0 || hl_ofd_identity_mint(&first, &b) != 0) return 11;
        return !hl_ofd_identity_valid(a) || !hl_ofd_identity_valid(b) || hl_ofd_identity_equal(a, b) || a.member != 7 ||
                       !ofd_lineage_equal(a.lineage, lineage)
                   ? 12
                   : 0;
    }
    if (scenario == 1) {
#if defined(_WIN32)
        return 0;
#else
        int transfer[2];
        if (pipe(transfer) != 0) return 13;
        pid_t child = fork();
        if (child == 0) {
            close(transfer[0]);
            hl_ofd_member child_member;
            hl_ofd_identity identity = {0};
            int result = hl_ofd_member_mint(space, &child_member) != 0 || child_member.ordinal != 8 ||
                         hl_ofd_identity_mint(&child_member, &identity) != 0 ||
                         write(transfer[1], &identity, sizeof identity) != (ssize_t)sizeof identity;
            _exit(result ? 1 : 0);
        }
        close(transfer[1]);
        hl_ofd_identity parent, from_child;
        int status = 0;
        int failed = child < 0 || hl_ofd_identity_mint(&first, &parent) != 0 ||
                     read(transfer[0], &from_child, sizeof from_child) != (ssize_t)sizeof from_child ||
                     waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0 ||
                     !hl_ofd_identity_valid(from_child) || from_child.member != 8 ||
                     hl_ofd_identity_equal(parent, from_child) || parent.sequence == from_child.sequence;
        close(transfer[0]);
        return failed ? 14 : 0;
#endif
    }
    if (scenario == 2) {
        hl_ofd_identity restored = {lineage, 91, UINT64_C(0x100000000)};
        hl_ofd_identity next;
        return hl_ofd_identity_reattach(&first, restored) != 0 || hl_ofd_identity_mint(&first, &next) != 0 ||
                       next.sequence <= restored.sequence
                   ? 15
                   : 0;
    }
    if (scenario == 3) {
        hl_ofd_identity stale = {0};
        stale.lineage.high = lineage.high ^ 1u;
        stale.lineage.low = lineage.low;
        stale.member = 7;
        stale.sequence = 4;
        return hl_ofd_identity_reattach(&first, stale) == ESTALE ? 0 : 16;
    }
    if (scenario == 4) {
        atomic_store_explicit(&space->next_sequence, UINT64_MAX, memory_order_relaxed);
        hl_ofd_identity identity;
        return hl_ofd_identity_mint(&first, &identity) == EOVERFLOW &&
                       atomic_load_explicit(&space->next_sequence, memory_order_relaxed) == UINT64_MAX
                   ? 0
                   : 17;
    }
    if (scenario == 5) {
        hl_ofd_identity before, after;
        hl_ofd_generation_binding first_generation = {.generation_high = 101,
                                                      .generation_low = 103,
                                                      .fence = 2,
                                                      .lineage = lineage,
                                                      .next_member = 9,
                                                      .next_sequence = 1};
        hl_ofd_generation_binding second_generation = {.generation_high = 107,
                                                       .generation_low = 109,
                                                       .fence = 3,
                                                       .lineage = lineage,
                                                       .next_member = 9,
                                                       .next_sequence = 2};
        if (hl_ofd_namespace_admit_validated(space, first_generation) != 0 ||
            hl_ofd_identity_mint(&first, &before) != 0 ||
            hl_ofd_namespace_init(space, sizeof *space, lineage, 0) != 0 ||
            hl_ofd_namespace_admit_validated(space, second_generation) != 0 ||
            hl_ofd_member_bind(&first, space, lineage, 7) != 0 || hl_ofd_identity_reattach(&first, before) != 0 ||
            hl_ofd_identity_mint(&first, &after) != 0)
            return 18;
        return !hl_ofd_identity_equal(before, before) || after.sequence <= before.sequence ||
                       !ofd_lineage_equal(after.lineage, before.lineage)
                   ? 19
                   : 0;
    }
    if (scenario == 6) {
        hl_ofd_identity before, after;
        if (hl_ofd_identity_mint(&first, &before) != 0) return 20;
        int reset = hl_ofd_namespace_init(space, sizeof *space, lineage, 1);
        if (reset == 0 && hl_ofd_namespace_admit_validated(space, initial_generation) == 0 &&
            hl_ofd_member_bind(&first, space, lineage, 7) == 0 && hl_ofd_identity_mint(&first, &after) == 0 &&
            after.sequence <= before.sequence)
            return 21;
        return reset == EALREADY && ofd_namespace_valid(space) ? 0 : 22;
    }
    if (scenario == 7) { return hl_ofd_namespace_admit_validated(space, initial_generation) == ESTALE ? 0 : 23; }
    if (scenario == 8) {
        atomic_store_explicit(&space->next_member, UINT64_MAX, memory_order_relaxed);
        hl_ofd_member member;
        return hl_ofd_member_mint(space, &member) == EOVERFLOW &&
                       atomic_load_explicit(&space->next_member, memory_order_relaxed) == UINT64_MAX
                   ? 0
                   : 24;
    }
    if (scenario == 9) {
        hl_ofd_generation_binding advanced = initial_generation;
        advanced.fence = 2;
        advanced.next_member = 41;
        if (hl_ofd_namespace_admit_validated(space, advanced) != 0) return 25;
        hl_ofd_member member;
        return hl_ofd_member_mint(space, &member) == 0 && member.ordinal == 41 ? 0 : 26;
    }
    if (scenario == 10) {
        hl_ofd_identity first_identity = {lineage, 7, 43};
        hl_ofd_identity collision = {lineage, 8, 43};
        return hl_ofd_identity_alias_compatible(first_identity, collision) ? 28 : 0;
    }
    if (scenario == 11 || scenario == 12) {
        hl_ofd_identity identity;
        if (scenario == 12 && hl_ofd_identity_mint(&first, &identity) != 0) return 28;
        hl_ofd_generation_binding advanced = initial_generation;
        advanced.fence = 2;
        advanced.next_member = scenario == 11 ? 7 : 8;
        advanced.next_sequence = 1;
        return hl_ofd_namespace_admit_validated(space, advanced) == ESTALE ? 0 : 29;
    }
    if (scenario == 13) {
        hl_ofd_identity invalid = {{0, 0}, 0, 0};
        return hl_ofd_identity_record_valid(invalid, 7) ? 30 : 0;
    }
    return 27;
}

int hl_ofd_identity_fixture(uint32_t scenario) {
#if defined(_WIN32)
    hl_ofd_namespace space;
    memset(&space, 0, sizeof space);
    return ofd_fixture_core(scenario, &space);
#else
    hl_ofd_namespace *space = mmap(NULL, sizeof *space, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (space == MAP_FAILED) return 21;
    int result = ofd_fixture_core(scenario, space);
    munmap(space, sizeof *space);
    return result;
#endif
}
#endif
