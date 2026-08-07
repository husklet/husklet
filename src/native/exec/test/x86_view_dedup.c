#include "support.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "x86_view_dedup:%d: %s\n", __LINE__, #value); return 1; } } while (0)

#if defined(__aarch64__)

static hl_native_status executable_begin(void *opaque) {
    test_memory *memory = opaque;
    memory->begin_calls++;
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_WRITE) == 0
               ? HL_NATIVE_OK
               : HL_NATIVE_PLATFORM;
}

static hl_native_status executable_end(void *opaque) {
    test_memory *memory = opaque;
    memory->end_calls++;
    __builtin___clear_cache((char *)memory->writable, (char *)memory->writable + memory->capacity);
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_EXEC) == 0
               ? HL_NATIVE_OK
               : HL_NATIVE_PLATFORM;
}

static int resolve_source(void *opaque, uint64_t pc, uint64_t mapping, uint64_t epoch,
                          hl_native_source_span *output) {
    const hl_native_source_span *span = opaque;
    if (pc < span->guest_first || pc >= span->guest_first + span->size ||
        mapping != span->mapping_incarnation || epoch != span->instruction_epoch)
        return 0;
    *output = *span;
    return 1;
}

typedef struct narrowing_provider {
    uint64_t guest;
    uint64_t storage[2];
    uint32_t calls;
    uint32_t read_permissions;
    uint32_t write_permissions;
} narrowing_provider;

/* Answers the read fault with a read-only view and the write fault with a
 * read/write view of the very same range and host bytes, which is how a
 * permission-keyed duplicate reaches view_install during a run. */
static uint32_t resolve_operand(void *opaque, uint64_t address, uint64_t size, uint32_t access,
                                uint64_t mapping, uint64_t epoch, hl_native_projection_view *output) {
    narrowing_provider *provider = opaque;
    (void)epoch;
    provider->calls++;
    if (address < provider->guest || size == 0 ||
        address + size > provider->guest + sizeof provider->storage)
        return HL_NATIVE_OPERAND_FAULT;
    *output = (hl_native_projection_view){
        .guest_first = provider->guest,
        .guest_last = provider->guest + sizeof provider->storage,
        .host_first = (uint64_t)(uintptr_t)provider->storage,
        .mapping_incarnation = mapping,
        .permissions = (access & HL_NATIVE_ACCESS_WRITE) != 0 ? provider->write_permissions
                                                              : provider->read_permissions,
        .write_policy = HL_NATIVE_WRITE_EXACT};
    return HL_NATIVE_OPERAND_RESOLVED;
}

static size_t duplicate_ranges(const hl_native_x86_64_cpu *cpu) {
    size_t duplicates = 0;
    for (size_t outer = 0; outer < cpu->read_count; ++outer)
        for (size_t inner = outer + 1; inner < cpu->read_count; ++inner)
            if (cpu->read_views[outer][0] == cpu->read_views[inner][0] &&
                cpu->read_views[outer][1] == cpu->read_views[inner][1] &&
                cpu->read_views[outer][2] == cpu->read_views[inner][2])
                duplicates++;
    return duplicates;
}

static int narrowed_view_is_not_republished(void) {
    static const uint8_t program[] = {
        0x48, 0x8b, 0x03,       /* mov rax,[rbx] */
        0x48, 0x83, 0xc0, 0x01, /* add rax,1 */
        0x48, 0x89, 0x03,       /* mov [rbx],rax */
        0x0f, 0x05,             /* syscall */
    };
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    memory.write_begin = executable_begin;
    memory.write_end = executable_end;
    hl_native_config config = test_config(&memory, 0);
    hl_native_executor *executor = NULL;
    hl_native_x86_64_cpu state = {0};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
                         .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
    narrowing_provider provider = {.guest = 0x7000,
                                   .read_permissions = HL_NATIVE_ACCESS_READ,
                                   .write_permissions = HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE};
    hl_native_source_span span = {0x6000, program, sizeof(program), 7, 14};
    hl_native_source source = {&span, 1, 7, 14};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7, .budget = 4, .source = &source,
        .source_context = &span, .source_resolve = resolve_source,
        .operand_context = &provider, .operand_resolve = resolve_operand};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};

    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    provider.storage[0] = 5;
    state = (hl_native_x86_64_cpu){.program = 0x6000, .registers = {[3] = provider.guest}};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL);
    CHECK(state.registers[0] == 6 && provider.storage[0] == 6);
    /* Both faults reached the resolver, so both views were offered to the cache. */
    CHECK(provider.calls == 2);
    CHECK(state.read_count >= 1);
    CHECK(duplicate_ranges(&state) == 0);
    /* The surviving entry is the one that answers writes as well as reads. */
    CHECK(state.read_views[0][3] == (HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE));

    hl_native_destroy(executor);
    return 0;
}

#else
static int narrowed_view_is_not_republished(void) { return 0; }
#endif

int main(void) { return narrowed_view_is_not_republished(); }
