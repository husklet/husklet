#include "../include/executor.h"
#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/projection.h"
#include "../src/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "aarch64_stitch:%d: %s\n", __LINE__, #value); return 1; } } while (0)

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

#define REGION 256u
#define CALLEE 0x1000u
#define CALLER 0x2000u
#define ENTRY  0x20f0u

/* Mirrors the Rust provider: one borrowed buffer, refilled on every call, so a
 * span handed out earlier aliases whatever the latest call resolved. */
typedef struct borrowed_resolver {
    uint8_t buffer[REGION];
    uint8_t caller[REGION];
    uint8_t callee[REGION];
    uint64_t mapping;
    uint64_t epoch;
} borrowed_resolver;

static int resolve_borrowed(void *opaque, uint64_t pc, uint64_t mapping, uint64_t epoch,
                            hl_native_source_span *output) {
    borrowed_resolver *state = opaque;
    const uint8_t *region;
    uint64_t first;
    if (mapping != state->mapping || epoch != state->epoch) return 0;
    if (pc >= CALLER && pc < CALLER + REGION) {
        first = CALLER;
        region = state->caller;
    } else if (pc >= CALLEE && pc < CALLEE + REGION) {
        first = CALLEE;
        region = state->callee;
    } else {
        return 0;
    }
    memcpy(state->buffer, region, REGION);
    *output = (hl_native_source_span){.guest_first = first, .bytes = state->buffer, .size = REGION,
        .mapping_incarnation = mapping, .instruction_epoch = epoch};
    return 1;
}

static void store(uint8_t *region, unsigned offset, uint32_t word) {
    memcpy(region + offset, &word, sizeof(word));
}

/* A trace entered from a resolved span whose final word tail-calls another
 * resolved region must still translate the bytes of the entered region. */
static int stitch_keeps_entered_bytes(void) {
    executable_memory host = {0};
    hl_native_executor *executor = create(&host);
    CHECK(executor != NULL);
    const hl_native_change replace = {.abi = HL_NATIVE_ABI, .size = sizeof(replace),
        .kind = HL_NATIVE_REPLACE, .mapping_epoch = 11};
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);

    static _Alignas(16) uint8_t stack[4096];
    uint64_t stack_first = (uint64_t)(uintptr_t)stack;
    uint64_t stack_last = (uint64_t)(uintptr_t)(stack + sizeof(stack));
    const hl_a64_view view = {stack_first, stack_last, stack_first, 11,
        HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE, HL_NATIVE_WRITE_EXACT, 0};
    const hl_a64_projection projection = {&view, 1, 11, 0};

    static borrowed_resolver state;
    memset(&state, 0, sizeof(state));
    state.mapping = 11;
    state.epoch = 5;
    for (unsigned offset = 0; offset < REGION; offset += 4) {
        store(state.caller, offset, UINT32_C(0xd503201f));
        store(state.callee, offset, UINT32_C(0xd503201f));
    }
    /* movz x1,#0xaaa ; svc #0 ; nop ; b CALLEE */
    store(state.caller, 0xf0, UINT32_C(0xd2815541));
    store(state.caller, 0xf4, UINT32_C(0xd4000001));
    store(state.caller, 0xfc, UINT32_C(0x17fffbc1));
    /* The callee body sits at the same region offset with a different constant,
     * so translating the wrong region is observable in x1 alone. */
    store(state.callee, 0xf0, UINT32_C(0xd2817761));
    store(state.callee, 0xf4, UINT32_C(0xd4000001));

    /* The batch reaches CALLER by relocation and its own last word is not a
     * branch, so the entering trace itself never stitches. */
    const uint32_t batch_words[] = {UINT32_C(0x17fffc3c), UINT32_C(0xd503201f), UINT32_C(0xd503201f)};
    const hl_native_source_span batch_span = {0x3000, (const uint8_t *)batch_words,
        sizeof(batch_words), 11, 5};
    const hl_native_source batch = {&batch_span, 1, 11, 5};

    hl_native_aarch64_cpu cpu_state = {0};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_AARCH64, .state.aarch64 = &cpu_state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_AARCH64, .mapping_epoch = 11, .projection = &projection,
        .budget = 8, .source = &batch, .source_context = &state,
        .source_resolve = resolve_borrowed};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    cpu_state.program = 0x3000;
    cpu_state.stack = stack_last;

    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL);
    CHECK(cpu_state.program == ENTRY + 4);
    CHECK(cpu_state.registers[1] == 0xaaa);
    hl_native_destroy(executor);
    return 0;
}

#else
static int stitch_keeps_entered_bytes(void) { return 0; }
#endif

int main(void) { return stitch_keeps_entered_bytes(); }
