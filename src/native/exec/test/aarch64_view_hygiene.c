#include "../include/executor.h"
#include "../src/arch/aarch64/assembler.h"
#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/guard.h"
#include "../src/arch/aarch64/projection.h"
#include "../src/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "aarch64_view_hygiene:%d: %s\n", __LINE__, #value); return 1; } } while (0)

#if defined(__aarch64__)

typedef struct executable_memory {
    uint8_t *address;
    uint64_t capacity;
} executable_memory;

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
        .capacity = 64u << 20, .alignment = 4096, .flags = HL_NATIVE_DIAGNOSTICS,
        .memory = &memory};
    hl_native_executor *executor = NULL;
    return hl_native_create(&config, &executor) == HL_NATIVE_OK ? executor : NULL;
}

/* Publishing fewer views than a previous activation must retire the slots the
 * new projection does not own, delta and permissions included. */
static int publish_retires_unused_slots(void) {
    executable_memory host = {0};
    hl_native_executor *executor = create(&host);
    CHECK(executor != NULL);
    const hl_native_change replace = {.abi = HL_NATIVE_ABI, .size = sizeof(replace),
        .kind = HL_NATIVE_REPLACE, .mapping_epoch = 11};
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);

    static _Alignas(16) uint8_t stack[8192];
    uint64_t first = (uint64_t)(uintptr_t)stack;
    uint64_t middle = first + 4096;
    uint64_t last = first + sizeof(stack);

    const hl_a64_view pair[2] = {
        {first, middle, first, 11, HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE,
         HL_NATIVE_WRITE_EXACT, 0},
        {middle, last, middle, 11, HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE,
         HL_NATIVE_WRITE_EXACT, 1},
    };
    const hl_a64_view single = pair[0];
    const hl_a64_projection two = {pair, 2, 11, 0};
    const hl_a64_projection one = {&single, 1, 11, 0};

    /* One nop, then a self branch the budget cuts short: the run only has to
     * reach the entry publication. */
    const uint32_t words[] = {UINT32_C(0xd503201f)};
    const hl_native_source_span span = {0x3000, (const uint8_t *)words, sizeof(words), 11, 5};
    const hl_native_source source = {&span, 1, 11, 5};

    hl_native_aarch64_cpu state = {0};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_AARCH64, .state.aarch64 = &state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_AARCH64, .mapping_epoch = 11, .budget = 1,
        .source = &source, .projection = &two};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    state.program = 0x3000;
    state.stack = last;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(state.read_count == 2 && state.read_views[1][1] != 0);

    request.projection = &one;
    request.budget = 1;
    state.program = 0x3000;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(state.read_count == 1);
    CHECK(state.read_views[1][0] == 0 && state.read_views[1][1] == 0 &&
          state.read_views[1][2] == 0 && state.read_views[1][3] == 0);
    hl_native_destroy(executor);
    return 0;
}

/* The cached view scan keeps read_count in x17 across slots, so the permission
 * word it tests before falling through to the next slot must land elsewhere. */
static int cache_scan_keeps_its_bound(void) {
    static uint8_t buffer[HL_A64_GUARDED_MAX_BYTES];
    for (unsigned pass = 0; pass < 2; ++pass) {
        hl_a64_assembler assembler;
        hl_a64_guard guard = {0};
        uint32_t required = pass == 0 ? HL_A64_PERMISSION_READ : HL_A64_PERMISSION_WRITE;
        unsigned bound_checks = 0;
        unsigned scan_tests = 0;
        unsigned bound_register_tests = 0;
        CHECK(hl_a64_assembler_begin(&assembler, buffer, buffer, sizeof(buffer)));
        hl_a64_guard_begin(&assembler, 8, required, &guard);
        hl_a64_guard_finish(&assembler, &guard);
        CHECK(hl_a64_assembler_ok(&assembler));
        size_t words = hl_a64_assembler_size(&assembler) / sizeof(uint32_t);
        for (size_t index = 0; index < words; ++index) {
            uint32_t word;
            memcpy(&word, buffer + index * sizeof(uint32_t), sizeof(word));
            /* subs against read_count in x17: the write scan compares it per
             * slot, the read loop decrements it as the remaining-slot counter. */
            if ((word & UINT32_C(0xffc00000)) == UINT32_C(0xf1000000) &&
                ((word & 31u) == 31u || (word & 31u) == 17u) && ((word >> 5) & 31u) == 17u)
                bound_checks++;
            /* tbz/tbnz -- the per-slot permission test. */
            if ((word & UINT32_C(0x7e000000)) == UINT32_C(0x36000000)) {
                if ((word & 31u) == 17u) bound_register_tests++;
                else scan_tests++;
            }
        }
        /* Only the single-view guard, which has no bound to preserve, may test
         * the register the scan keeps read_count in. */
        CHECK(bound_checks >= 1 && scan_tests >= 1 && bound_register_tests <= 1);
    }
    return 0;
}

#else
static int publish_retires_unused_slots(void) { return 0; }
static int cache_scan_keeps_its_bound(void) { return 0; }
#endif

int main(void) { return publish_retires_unused_slots() || cache_scan_keeps_its_bound(); }
