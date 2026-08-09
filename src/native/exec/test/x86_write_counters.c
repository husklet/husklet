#define _POSIX_C_SOURCE 200809L
#include "support.h"
#include "../src/translation.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "x86_write_counters:%d: %s\n", __LINE__, #value); return 1; } } while (0)

#if defined(__aarch64__)

/* The x86 entry trampoline publishes at most four views. */
#define VIEWS 4u
#define SPAN 0x100u

static hl_native_status executable_begin(void *opaque) {
    test_memory *memory = opaque;
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_WRITE) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static hl_native_status executable_end(void *opaque) {
    test_memory *memory = opaque;
    __builtin___clear_cache((char *)memory->writable, (char *)memory->writable + memory->capacity);
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_EXEC) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

/* mov [rax],rdx repeated, then syscall. */
static const uint8_t one_store[] = {0x48, 0x89, 0x10, 0x0f, 0x05};
static const uint8_t four_stores[] = {0x48, 0x89, 0x10, 0x48, 0x89, 0x10,
                                      0x48, 0x89, 0x10, 0x48, 0x89, 0x10, 0x0f, 0x05};
/* mov [rax],rdx ; mov [rcx],rdx ; syscall: two views, alternating. */
static const uint8_t two_targets[] = {0x48, 0x89, 0x10, 0x48, 0x89, 0x11, 0x0f, 0x05};
/* Four alternating stores: the third transition returns to a view and interval
 * the journal already owns, which is the only shape that merges. */
static const uint8_t ping_pong[] = {0x48, 0x89, 0x10, 0x48, 0x89, 0x11, 0x48, 0x89, 0x10,
                                    0x48, 0x89, 0x11, 0x0f, 0x05};

/* mov [base+disp8],rdx over four bases and six disjoint displacements, so every
 * store changes both the owning view and the interval and the journal fills. */
static uint8_t sweep[VIEWS * 6u * 4u + 2u];
static const uint8_t base_modrm[VIEWS] = {0x50, 0x51, 0x53, 0x56}; /* rax, rcx, rbx, rsi */

static size_t build_sweep(void) {
    size_t cursor = 0;
    for (unsigned step = 0; step < 6u; ++step)
        for (unsigned base = 0; base < VIEWS; ++base) {
            sweep[cursor++] = 0x48;
            sweep[cursor++] = 0x89;
            sweep[cursor++] = base_modrm[base];
            sweep[cursor++] = (uint8_t)(step * 16u);
        }
    sweep[cursor++] = 0x0f;
    sweep[cursor++] = 0x05;
    return cursor;
}

typedef struct {
    uint64_t backing[VIEWS][SPAN / 8u];
    hl_native_projection_view views[VIEWS];
    hl_native_projection projection;
} world;

static void world_init(world *state) {
    memset(state->backing, 0, sizeof(state->backing));
    for (unsigned index = 0; index < VIEWS; ++index)
        state->views[index] = (hl_native_projection_view){
            0x8000 + index * 0x1000, 0x8000 + index * 0x1000 + SPAN,
            (uint64_t)(uintptr_t)&state->backing[index][0], 7, 3, HL_NATIVE_WRITE_EXACT, 0};
    state->projection = (hl_native_projection){state->views, VIEWS, 7, 0};
}

static void report(const char *label, const hl_native_diagnostics *value) {
    fprintf(stderr,
            "x86_write_counters %s: guard_fast=%llu guard_full=%llu guard_fallback=%llu "
            "dirty_merged=%llu dirty_committed=%llu dirty_overflow=%llu "
            "cache_hit=%llu cache_miss=%llu\n",
            label, (unsigned long long)value->x86_guard_fast,
            (unsigned long long)value->x86_guard_full,
            (unsigned long long)value->x86_guard_fallback,
            (unsigned long long)value->x86_dirty_merged,
            (unsigned long long)value->x86_dirty_committed,
            (unsigned long long)value->x86_dirty_overflow,
            (unsigned long long)value->x86_write_cache_hit,
            (unsigned long long)value->x86_write_cache_miss);
}

int main(void) {
    static world state;
    size_t sweep_size = build_sweep();
    hl_native_source_span spans[] = {{0x7000, one_store, sizeof(one_store), 7, 9},
                                     {0x7100, four_stores, sizeof(four_stores), 7, 9},
                                     {0x7200, two_targets, sizeof(two_targets), 7, 9},
                                     {0x7300, sweep, sweep_size, 7, 9},
                                     {0x7400, ping_pong, sizeof(ping_pong), 7, 9}};
    hl_native_source source = {spans, 5, 7, 9};
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    memory.write_begin = executable_begin;
    memory.write_end = executable_end;
    hl_native_config config = test_config(&memory, HL_NATIVE_DIAGNOSTICS);
    hl_native_executor *executor = NULL;
    hl_native_x86_64_cpu cpu_state = {0};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_X86_64, .state.x86_64 = &cpu_state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7, .budget = 128, .source = &source};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    hl_native_diagnostics before = {.abi = HL_NATIVE_ABI, .size = sizeof(before)};
    hl_native_diagnostics after = {.abi = HL_NATIVE_ABI, .size = sizeof(after)};

    world_init(&state);
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    request.projection = &state.projection;

    /* Four stores to one address. Every one resolves in the write cache and
     * branches past the bounds guard. */
    cpu_state = (hl_native_x86_64_cpu){.program = 0x7100, .registers = {[0] = 0x8000, [2] = 11}};
    CHECK(hl_native_diagnose(executor, &before) == HL_NATIVE_OK);
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL);
    CHECK(state.backing[0][0] == 11);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    report("same-view", &after);
    CHECK(after.x86_guard_fast - before.x86_guard_fast == 4);
    CHECK(after.x86_write_cache_hit - before.x86_write_cache_hit == 4);
    /* The control: nothing here left the cache, so the guard counters hold. */
    CHECK(after.x86_guard_full == before.x86_guard_full);
    CHECK(after.x86_write_cache_miss == before.x86_write_cache_miss);
    CHECK(after.x86_guard_fallback == before.x86_guard_fallback);
    CHECK(after.x86_dirty_overflow == before.x86_dirty_overflow);
    /* One view, one interval: the journal is never archived. */
    CHECK(after.x86_dirty_committed == before.x86_dirty_committed);
    CHECK(after.x86_dirty_merged == before.x86_dirty_merged);

    /* Two views alternating inside one run: the first transition appends the
     * archived interval. */
    before = after;
    cpu_state = (hl_native_x86_64_cpu){.program = 0x7200,
        .registers = {[0] = 0x8000, [1] = 0x9000, [2] = 21}};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL);
    CHECK(state.backing[0][0] == 21 && state.backing[1][0] == 21);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    report("two-view", &after);
    CHECK(after.x86_dirty_committed > before.x86_dirty_committed);
    CHECK(after.x86_dirty_overflow == before.x86_dirty_overflow);
    CHECK(after.x86_guard_full == before.x86_guard_full);

    /* Four alternating stores in one run: the third transition finds the record
     * that already owns the interval and merges instead of appending. */
    before = after;
    cpu_state = (hl_native_x86_64_cpu){.program = 0x7400,
        .registers = {[0] = 0x8000, [1] = 0x9000, [2] = 31}};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    report("ping-pong", &after);
    CHECK(after.x86_dirty_merged > before.x86_dirty_merged);
    CHECK(after.x86_dirty_overflow == before.x86_dirty_overflow);

    /* A store no published view covers: the cache finds nothing, the full guard
     * runs and rejects. */
    before = after;
    cpu_state = (hl_native_x86_64_cpu){.program = 0x7000, .registers = {[0] = 0x30000, [2] = 5}};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    report("uncovered", &after);
    CHECK(after.x86_write_cache_miss > before.x86_write_cache_miss);
    CHECK(after.x86_guard_full > before.x86_guard_full);
    CHECK(after.x86_guard_fallback > before.x86_guard_fallback);
    /* The control: an uncovered store cannot resolve in the cache. */
    CHECK(after.x86_guard_fast == before.x86_guard_fast);
    CHECK(after.x86_write_cache_hit == before.x86_write_cache_hit);
    CHECK(after.x86_dirty_committed == before.x86_dirty_committed);

    /* Twenty-four disjoint (view, interval) pairs against a sixteen-record
     * journal. */
    before = after;
    cpu_state = (hl_native_x86_64_cpu){.program = 0x7300,
        .registers = {[0] = 0x8000, [1] = 0x9000, [2] = 77, [3] = 0xa000, [6] = 0xb000}};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(hl_native_diagnose(executor, &after) == HL_NATIVE_OK);
    report("saturate", &after);
    CHECK(after.x86_dirty_overflow > before.x86_dirty_overflow);

    /* The write-path group is the tail of the ABI record, so a caller that
     * declares a shorter size must not have it written past its buffer. */
    {
        struct { hl_native_diagnostics value; unsigned char canary[64]; } bounded;
        memset(&bounded, 0xa5, sizeof(bounded));
        bounded.value.abi = HL_NATIVE_ABI;
        bounded.value.size = 384;
        CHECK(hl_native_diagnose(executor, &bounded.value) == HL_NATIVE_OK);
        for (size_t index = 384; index < sizeof(hl_native_diagnostics); ++index)
            CHECK(((const unsigned char *)&bounded.value)[index] == 0xa5);
        for (size_t index = 0; index < sizeof(bounded.canary); ++index)
            CHECK(bounded.canary[index] == 0xa5);
    }

    hl_native_destroy(executor);
    return 0;
}

#else
int main(void) { return 0; }
#endif
