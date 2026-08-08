/* A store never reaches read_cache, so its only bounded owner selection is the
 * write_cache the miss path emits. Distinct buffers must drive every store
 * through that scan rather than to the dispatcher; one shared buffer must keep
 * the latched window and never enter the scan at all. */
#include "../include/executor.h"
#include "../src/arch/aarch64/assembler.h"
#include "../src/arch/aarch64/guard.h"
#include "../src/arch/aarch64/projection.h"
#include "../src/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>

#define CHECK(v) do { if (!(v)) { fprintf(stderr, "aarch64_store_cache:%d: %s\n", __LINE__, #v); return 1; } } while (0)

#if defined(__aarch64__)

typedef struct executable_memory { uint8_t *address; uint64_t capacity; } executable_memory;

static hl_native_status reserve(void *opaque, uint64_t capacity, uint64_t alignment,
                                uint32_t dual, hl_native_mapping *output) {
    executable_memory *memory = opaque;
    (void)alignment;
    if (dual != 0) return HL_NATIVE_PLATFORM;
    void *address = mmap(NULL, (size_t)capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (address == MAP_FAILED) return HL_NATIVE_MEMORY;
    memory->address = address;
    memory->capacity = capacity;
    *output = (hl_native_mapping){.abi = HL_NATIVE_ABI, .size = sizeof(*output), .handle = 1,
        .writable = (uint64_t)(uintptr_t)address, .executable = (uint64_t)(uintptr_t)address,
        .capacity = capacity};
    return HL_NATIVE_OK;
}
static hl_native_status release(void *opaque, hl_native_handle handle) {
    executable_memory *memory = opaque;
    if (handle != 1 || memory->address == NULL) return HL_NATIVE_ARGUMENT;
    if (munmap(memory->address, (size_t)memory->capacity) != 0) return HL_NATIVE_PLATFORM;
    memory->address = NULL;
    return HL_NATIVE_OK;
}
static hl_native_status publish(void *opaque, hl_native_handle handle, uint64_t offset, uint64_t size) {
    executable_memory *memory = opaque;
    return handle == 1 && offset <= memory->capacity && size <= memory->capacity - offset
        ? HL_NATIVE_OK : HL_NATIVE_ARGUMENT;
}
static hl_native_status repair(void *opaque, hl_native_mapping *mapping, uint32_t preserve) {
    executable_memory *memory = opaque;
    (void)preserve;
    return mapping->handle == 1 && mapping->writable == (uint64_t)(uintptr_t)memory->address
        ? HL_NATIVE_OK : HL_NATIVE_ARGUMENT;
}
static hl_native_status write_begin(void *opaque) {
    executable_memory *memory = opaque;
    return mprotect(memory->address, (size_t)memory->capacity, PROT_READ | PROT_WRITE) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}
static hl_native_status write_end(void *opaque) {
    executable_memory *memory = opaque;
    __builtin___clear_cache((char *)memory->address, (char *)memory->address + memory->capacity);
    return mprotect(memory->address, (size_t)memory->capacity, PROT_READ | PROT_EXEC) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static hl_native_executor *create(executable_memory *host) {
    const hl_native_memory memory = {.abi = HL_NATIVE_ABI, .size = sizeof(memory), .context = host,
        .reserve = reserve, .release = release, .publish = publish, .repair = repair,
        .write_begin = write_begin, .write_end = write_end};
    const hl_native_config config = {.abi = HL_NATIVE_ABI, .size = sizeof(config),
        .capacity = 64u << 20, .alignment = 4096, .flags = HL_NATIVE_DIAGNOSTICS, .memory = &memory};
    hl_native_executor *executor = NULL;
    return hl_native_create(&config, &executor) == HL_NATIVE_OK ? executor : NULL;
}

static _Alignas(64) uint8_t buffer_a[4096];
static _Alignas(64) uint8_t buffer_b[4096];
static _Alignas(64) uint8_t buffer_c[4096];

typedef struct arm_result { uint64_t fast_guards, guards, fallbacks, write_faults; } arm_result;

/* mode: 0 distinct buffers, 1 one shared buffer, 2 store target read-only. */
static int measure(int mode, arm_result *result) {
    executable_memory host = {0};
    hl_native_executor *executor = create(&host);
    CHECK(executor != NULL);
    const hl_native_change replace = {.abi = HL_NATIVE_ABI, .size = sizeof(replace),
        .kind = HL_NATIVE_REPLACE, .mapping_epoch = 11};
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);

    /* Guest addresses distinct from the host buffers, so each view carries its
     * own delta and a misselected slot cannot alias the right destination. */
    const uint64_t a = 0x100000, b = 0x200000, c = 0x300000;
    uint8_t *const hosts[3] = {buffer_a, buffer_b, buffer_c};
    const uint64_t guests[3] = {a, b, c};
    const int shared = mode == 1;
    hl_a64_view views[3];
    for (unsigned i = 0; i < 3; ++i)
        views[i] = (hl_a64_view){guests[i], guests[i] + 4096,
            (uint64_t)(uintptr_t)hosts[i], 11,
            HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE, HL_NATIVE_WRITE_EXACT, i};
    /* Deny writes to the store's own target, which the cache must honour. */
    if (mode == 2) views[2].permissions = HL_A64_PERMISSION_READ;
    const hl_a64_projection projection = {views, 3, 11, 0};

    /* x4=a x3=b x2=c x1=0; the float_simd inner shape, then loop. */
    const uint32_t words[] = {
        UINT32_C(0x3ce16880), /* ldr q0,[x4,x1] */
        UINT32_C(0x3ce16861), /* ldr q1,[x3,x1] */
        UINT32_C(0x3ca16842), /* str q2,[x2,x1] */
        UINT32_C(0x3ca16883), /* str q3,[x4,x1]: a second owner in one run */
        UINT32_C(0x91004021), /* add x1,x1,#16 */
    };
    const hl_native_source_span span = {0x4000, (const uint8_t *)words, sizeof(words), 11, 5};
    const hl_native_source source = {&span, 1, 11, 5};

    hl_native_aarch64_cpu state = {0};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_AARCH64, .state.aarch64 = &state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_AARCH64, .mapping_epoch = 11, .budget = 5,
        .source = &source, .projection = &projection};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};

    memset(buffer_a, 0xa1, sizeof(buffer_a));
    memset(buffer_b, 0xb2, sizeof(buffer_b));
    memset(buffer_c, 0, sizeof(buffer_c));
    static const uint8_t zero[16] = {0};

    const unsigned rounds = 200;
    for (unsigned round = 0; round < rounds; ++round) {
        uint64_t offset = (round % 16) * 16;
        uint64_t pattern[2] = {UINT64_C(0x5150000000000000) + round, ~(uint64_t)round};
        uint64_t second[2] = {UINT64_C(0x2020000000000000) + round, round};
        memcpy(&state.vectors[2 * 2], pattern, sizeof(pattern));
        memcpy(&state.vectors[3 * 2], second, sizeof(second));
        state.program = 0x4000;
        state.registers[4] = a;
        state.registers[3] = shared ? a : b;
        state.registers[2] = shared ? a : c;
        state.registers[1] = offset;
        state.stack = c + 2048;
        state.dirty_count = 0;
        state.dirty_overflow = 0;
        state.dirty_first = UINT64_MAX;
        state.dirty_last = 0;
        state.dirty_view_first = 0;
        state.dirty_view_last = 0;
        memset(state.dirty_records, 0, sizeof(state.dirty_records));
        hl_native_status status = hl_native_run(executor, &cpu, &request, &output);
        if (status != HL_NATIVE_OK) { fprintf(stderr, "run %u: status %d\n", round, (int)status); return 1; }
        if (mode == 2) {
            /* A read-only target must never be written, however the store was
             * resolved, and the fault must name the store's own address. */
            CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK);
            CHECK(memcmp(buffer_c + offset, zero, sizeof(pattern)) == 0);
            CHECK(state.fault_address == c + offset);
            CHECK(state.fault_access == HL_A64_PERMISSION_WRITE && state.fault_size == 16);
            continue;
        }
        /* The selected owner decides the host delta, so a misselected slot
         * lands the store in another buffer or at another offset. */
        if (shared) {
            /* Both stores target the one view; the second overwrites the first. */
            CHECK(memcmp(buffer_a + offset, second, sizeof(second)) == 0);
        } else {
            CHECK(memcmp(buffer_c + offset, pattern, sizeof(pattern)) == 0);
            CHECK(memcmp(buffer_a + offset, second, sizeof(second)) == 0);
            CHECK(buffer_b[offset] == 0xb2);
            /* Two owners were written, so the exact intervals must archive as
             * two records rather than merge across the view boundary. */
            /* The live interval must still be the second store alone, inside
             * its own owner: a merge across owners would widen it. */
            CHECK(state.dirty_view_first == a && state.dirty_view_last == a + 4096);
            CHECK(state.dirty_first == a + offset && state.dirty_last == a + offset + 16);
            /* The first store archived as its own record under its own owner. */
            CHECK(state.dirty_count == 1);
            CHECK(state.dirty_records[0][0] == c && state.dirty_records[0][1] == c + 4096);
            CHECK(state.dirty_records[0][2] == c + offset &&
                  state.dirty_records[0][3] == c + offset + 16);
            CHECK(state.dirty_overflow == 0);
        }
    }

    hl_native_diagnostics stats = {.abi = HL_NATIVE_ABI, .size = sizeof(stats)};
    CHECK(hl_native_diagnose(executor, &stats) == HL_NATIVE_OK);
    result->fast_guards = stats.a64_guard_fast;
    result->guards = stats.a64_guard_full;
    result->fallbacks = stats.a64_guard_fallback;
    result->write_faults = stats.a64_fallback_guard_write;
    hl_native_destroy(executor);
    return 0;
}

/* Guest contiguity alone must not extend a live exact interval: two adjacent
 * owners meeting at a page boundary have to archive separately. */
static int measure_adjacent(void) {
    executable_memory host = {0};
    hl_native_executor *executor = create(&host);
    CHECK(executor != NULL);
    const hl_native_change replace = {.abi = HL_NATIVE_ABI, .size = sizeof(replace),
        .kind = HL_NATIVE_REPLACE, .mapping_epoch = 11};
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);

    const uint64_t low = 0x100000, high = 0x101000;
    const hl_a64_view views[2] = {
        {low, high, (uint64_t)(uintptr_t)buffer_a, 11,
         HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE, HL_NATIVE_WRITE_EXACT, 0},
        {high, high + 4096, (uint64_t)(uintptr_t)buffer_b, 11,
         HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE, HL_NATIVE_WRITE_EXACT, 1},
    };
    const hl_a64_projection projection = {views, 2, 11, 0};

    const uint32_t words[] = {
        UINT32_C(0x3d800042), /* str q2,[x2]: last 16 bytes of the low owner */
        UINT32_C(0x3d800063), /* str q3,[x3]: first 16 bytes of the high owner */
    };
    const hl_native_source_span span = {0x4000, (const uint8_t *)words, sizeof(words), 11, 5};
    const hl_native_source source = {&span, 1, 11, 5};

    hl_native_aarch64_cpu state = {0};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_AARCH64, .state.aarch64 = &state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_AARCH64, .mapping_epoch = 11, .budget = 3,
        .source = &source, .projection = &projection};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};

    state.program = 0x4000;
    state.registers[2] = high - 16;
    state.registers[3] = high;
    state.stack = high + 2048;
    state.dirty_first = UINT64_MAX;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);

    /* The live interval belongs to the high owner alone. */
    CHECK(state.dirty_view_first == high && state.dirty_view_last == high + 4096);
    CHECK(state.dirty_first == high && state.dirty_last == high + 16);
    /* The low owner archived its own record instead of merging across. */
    CHECK(state.dirty_count == 1 && state.dirty_overflow == 0);
    CHECK(state.dirty_records[0][0] == low && state.dirty_records[0][1] == high);
    CHECK(state.dirty_records[0][2] == high - 16 && state.dirty_records[0][3] == high);
    hl_native_destroy(executor);
    return 0;
}

int main(void) {
    const unsigned rounds = 200;
    arm_result distinct = {0}, shared = {0}, denied = {0};
    if (measure(0, &distinct) != 0 || measure(1, &shared) != 0 || measure(2, &denied) != 0)
        return 1;
    if (measure_adjacent() != 0) return 1;
    /* a64_guard_fast counts read-cache and write-cache resolutions alike, so the
     * write-side claim below is the total less this fixed read-side baseline: the
     * two loads are resolved by the read cache once per round in every mode. */
    const unsigned reads = 2 * rounds;
    /* Three buffers: every store misses the single latched window, and the
     * write cache resolves all of them without a dispatcher exit. */
    CHECK(distinct.guards == 4 * rounds);
    CHECK(distinct.fast_guards == reads + 2 * rounds);
    CHECK(distinct.fallbacks == 0 && distinct.write_faults == 0);
    /* One buffer: the store reuses the already-active view and never scans. */
    CHECK(shared.guards == 4 * rounds);
    CHECK(shared.fast_guards == reads + 0);
    CHECK(shared.fallbacks == 0 && shared.write_faults == 0);
    /* A denied store is never resolved by the cache; only the two loads reach
     * resume, and every store leaves as a write-permission fallback. */
    CHECK(denied.fast_guards == reads + 0);
    CHECK(denied.guards == 2 * rounds);
    CHECK(denied.fallbacks == rounds && denied.write_faults == rounds);
    return 0;
}

#else
int main(void) { return 0; }
#endif
