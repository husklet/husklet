/* read_cache resolves loads against the bounded generation-qualified view cache.
 * Loads spread over three owners in one entry must all stay native rather than
 * reaching the dispatcher, the selected owner must decide the host delta, and a
 * view carrying only write permission must never satisfy a load. */
#include "../include/executor.h"
#include "../src/arch/aarch64/assembler.h"
#include "../src/arch/aarch64/guard.h"
#include "../src/arch/aarch64/projection.h"
#include "../src/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>

#define CHECK(v) do { if (!(v)) { fprintf(stderr, "aarch64_read_cache:%d: %s\n", __LINE__, #v); return 1; } } while (0)

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

/* A distinct value per buffer, so a misselected slot returns the wrong one. */
#define VALUE_A UINT64_C(0xa1a1a1a100000000)
#define VALUE_B UINT64_C(0xb2b2b2b200000000)
#define VALUE_C UINT64_C(0xc3c3c3c300000000)

typedef struct arm_result { uint64_t cache_hits, guards, fallbacks, read_faults; } arm_result;

/* mode: 0 distinct buffers, 1 one shared buffer, 2 third view lacks read. */
static int measure(int mode, arm_result *result) {
    executable_memory host = {0};
    hl_native_executor *executor = create(&host);
    CHECK(executor != NULL);
    const hl_native_change replace = {.abi = HL_NATIVE_ABI, .size = sizeof(replace),
        .kind = HL_NATIVE_REPLACE, .mapping_epoch = 11};
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);

    const uint64_t a = 0x100000, b = 0x200000, c = 0x300000;
    uint8_t *const hosts[3] = {buffer_a, buffer_b, buffer_c};
    const uint64_t guests[3] = {a, b, c};
    const int shared = mode == 1;
    hl_a64_view views[3];
    for (unsigned i = 0; i < 3; ++i)
        views[i] = (hl_a64_view){guests[i], guests[i] + 4096,
            (uint64_t)(uintptr_t)hosts[i], 11,
            HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE, HL_NATIVE_WRITE_EXACT, i};
    /* Deny reads of the third load's own target, which the cache must honour.
     * Write permission alone must never satisfy a load. */
    if (mode == 2) views[2].permissions = HL_A64_PERMISSION_WRITE;
    const hl_a64_projection projection = {views, 3, 11, 0};

    /* Three loads, one per owner, then a syscall to end the run. */
    const uint32_t words[] = {
        UINT32_C(0xf9400085), /* ldr x5,[x4] */
        UINT32_C(0xf9400066), /* ldr x6,[x3] */
        UINT32_C(0xf9400047), /* ldr x7,[x2] */
        UINT32_C(0xd4000001), /* svc #0 */
    };
    const hl_native_source_span span = {0x4000, (const uint8_t *)words, sizeof(words), 11, 5};
    const hl_native_source source = {&span, 1, 11, 5};

    hl_native_aarch64_cpu state = {0};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_AARCH64, .state.aarch64 = &state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_AARCH64, .mapping_epoch = 11, .budget = 8,
        .source = &source, .projection = &projection};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};

    memset(buffer_a, 0, sizeof(buffer_a));
    memset(buffer_b, 0, sizeof(buffer_b));
    memset(buffer_c, 0, sizeof(buffer_c));

    const unsigned rounds = 200;
    for (unsigned round = 0; round < rounds; ++round) {
        uint64_t offset = (round % 16) * 8;
        const uint64_t value_a = VALUE_A + round, value_b = VALUE_B + round,
                       value_c = VALUE_C + round;
        memcpy(buffer_a + offset, &value_a, sizeof(value_a));
        memcpy(buffer_b + offset, &value_b, sizeof(value_b));
        memcpy(buffer_c + offset, &value_c, sizeof(value_c));
        memset(&state, 0, sizeof(state));
        state.program = 0x4000;
        state.registers[4] = a + offset;
        state.registers[3] = (shared ? a : b) + offset;
        state.registers[2] = (shared ? a : c) + offset;
        state.stack = a + 2048;
        state.dirty_first = UINT64_MAX;
        hl_native_status status = hl_native_run(executor, &cpu, &request, &output);
        if (status != HL_NATIVE_OK) { fprintf(stderr, "run %u: status %d\n", round, (int)status); return 1; }
        if (mode == 2) {
            /* A view carrying only write permission must never satisfy the
             * load, and the fault must name the load's own address and access. */
            CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK);
            CHECK(state.fault_address == c + offset);
            CHECK(state.fault_access == HL_A64_PERMISSION_READ && state.fault_size == 8);
            /* The two permitted loads still completed before it. */
            CHECK(state.registers[5] == value_a && state.registers[6] == value_b);
            CHECK(state.registers[7] == 0);
            continue;
        }
        /* The selected owner decides the host delta, so a misselected slot
         * returns another buffer's value or another offset's. */
        CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL);
        CHECK(state.registers[5] == value_a);
        CHECK(state.registers[6] == (shared ? value_a : value_b));
        CHECK(state.registers[7] == (shared ? value_a : value_c));
        /* A load journals nothing. */
        CHECK(state.dirty_count == 0 && state.memory_written == 0);
    }

    hl_native_diagnostics stats = {.abi = HL_NATIVE_ABI, .size = sizeof(stats)};
    CHECK(hl_native_diagnose(executor, &stats) == HL_NATIVE_OK);
    result->cache_hits = stats.a64_guard_fast;
    result->guards = stats.a64_guard_full;
    result->fallbacks = stats.a64_guard_fallback;
    result->read_faults = stats.a64_fallback_guard_read;
    hl_native_destroy(executor);
    return 0;
}

int main(void) {
    const unsigned rounds = 200;
    arm_result distinct = {0}, shared = {0}, denied = {0};
    if (measure(0, &distinct) != 0 || measure(1, &shared) != 0 || measure(2, &denied) != 0)
        return 1;
    /* Three owners in one entry: the cache resolves every load itself, so none
     * of them reaches the dispatcher. */
    CHECK(distinct.guards == 3 * rounds);
    CHECK(distinct.fallbacks == 0 && distinct.read_faults == 0);
    /* One owner: the same three loads, still all resolved. */
    CHECK(shared.guards == 3 * rounds);
    CHECK(shared.fallbacks == 0 && shared.read_faults == 0);
    /* The write-only owner is never selected: only the two permitted loads
     * reach resume, and the third leaves as a read-permission fallback. */
    CHECK(denied.guards == 2 * rounds);
    CHECK(denied.fallbacks == rounds && denied.read_faults == rounds);
    return 0;
}

#else
int main(void) { return 0; }
#endif
