#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>

#if !defined(__aarch64__)
int main(void) {
    return 0;
}
#else

enum { SLOTS = 128, SLOT_BYTES = 64, OVERFLOW_LINES = 80 };

static void emit_ret_imm(uint32_t *code, unsigned value) {
    code[0] = 0x52800000u | ((value & 0xffffu) << 5); /* movz w0,#value */
    code[1] = 0xd65f03c0u;                            /* ret */
}

static void publish_lines(void *base, unsigned lines) {
    for (unsigned i = 0; i < lines; i++) {
        void *line = (char *)base + (size_t)i * SLOT_BYTES;
        __asm__ volatile("dc cvau,%0" : : "r"(line) : "memory");
    }
    __asm__ volatile("dsb ish" : : : "memory");
    for (unsigned i = 0; i < lines; i++) {
        void *line = (char *)base + (size_t)i * SLOT_BYTES;
        __asm__ volatile("ic ivau,%0" : : "r"(line) : "memory");
    }
    __asm__ volatile("dsb ish\n\tisb" : : : "memory");
}

static unsigned call_slot(uint32_t *base, unsigned slot) {
    unsigned (*function)(void) =
        (unsigned (*)(void))((char *)base + (size_t)slot * SLOT_BYTES);
    return function();
}

int main(void) {
    uint32_t *arena = mmap(NULL, SLOTS * SLOT_BYTES,
                           PROT_READ | PROT_WRITE | PROT_EXEC,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (arena == MAP_FAILED) return 1;

    /*
     * Exercise a tier-2-promotable loop before the first SMC event.  DBT reads
     * the freshly stored guest bytes directly, so no guest IC instruction is
     * needed for the initial entry.  The first rewrite/ISB must conservatively
     * retire this pre-SMC promoted body.
     */
    uint32_t *loop = arena;
    loop[0] = 0x52800001u | (2000u << 5); /* movz w1,#2000 */
    loop[1] = 0x52800000u;                /* movz w0,#0 */
    loop[2] = 0x11000400u;                /* add w0,w0,#1 */
    loop[3] = 0x71000421u;                /* subs w1,w1,#1 */
    loop[4] = 0x54ffffc1u;                /* b.ne loop[2] */
    loop[5] = 0xd65f03c0u;
    unsigned before = call_slot(arena, 0);
    loop[2] = 0x11000800u; /* add w0,w0,#2 */
    publish_lines(loop, 1);
    unsigned after = call_slot(arena, 0);
    if (before != 2000 || after != 4000) {
        printf("smc-targeted loop before=%u after=%u\n", before, after);
        return 1;
    }

    for (unsigned i = 1; i < SLOTS; i++)
        emit_ret_imm((uint32_t *)((char *)arena + (size_t)i * SLOT_BYTES),
                     1000u + i);
    publish_lines(arena, SLOTS);
    uint64_t checksum = 0;
    for (unsigned i = 1; i < SLOTS; i++)
        checksum = checksum * 131u + call_slot(arena, i);

    /* More than the queued-range capacity: overflow must fall back whole, not lose a rewrite. */
    for (unsigned i = 1; i <= OVERFLOW_LINES; i++)
        emit_ret_imm((uint32_t *)((char *)arena + (size_t)i * SLOT_BYTES),
                     3000u + i);
    publish_lines((char *)arena + SLOT_BYTES, OVERFLOW_LINES);
    for (unsigned i = 1; i <= OVERFLOW_LINES; i++) {
        unsigned got = call_slot(arena, i);
        if (got != 3000u + i) {
            printf("smc-targeted overflow slot=%u got=%u\n", i, got);
            return 1;
        }
        checksum = checksum * 131u + got;
    }

    /*
     * A generated constant branch is translated after SMC activation, hence
     * lowered through the invalidatable shared IBTC.  Rewriting only its
     * target must retain the trampoline but clear/refill its edge.
     */
    unsigned trampoline_slot = 120, target_slot = 121;
    uint32_t *trampoline =
        (uint32_t *)((char *)arena + (size_t)trampoline_slot * SLOT_BYTES);
    uint32_t *target =
        (uint32_t *)((char *)arena + (size_t)target_slot * SLOT_BYTES);
    intptr_t delta = (char *)target - (char *)trampoline;
    trampoline[0] = 0x14000000u | ((uint32_t)(delta / 4) & 0x03ffffffu);
    emit_ret_imm(target, 7001);
    publish_lines(trampoline, 2);
    for (unsigned i = 0; i < 1000; i++)
        if (call_slot(arena, trampoline_slot) != 7001) return 1;
    emit_ret_imm(target, 7002);
    publish_lines(target, 1);
    unsigned chained = call_slot(arena, trampoline_slot);
    unsigned unrelated = call_slot(arena, 119);
    if (chained != 7002 || unrelated != 1119) {
        printf("smc-targeted chain=%u unrelated=%u\n", chained, unrelated);
        return 1;
    }

    checksum ^= ((uint64_t)before << 48) ^ ((uint64_t)after << 32) ^
                ((uint64_t)chained << 16) ^ unrelated;
    printf("smc-targeted checksum=%llu\n", (unsigned long long)checksum);
    return 0;
}

#endif
