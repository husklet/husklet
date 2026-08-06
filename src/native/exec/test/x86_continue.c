#include "support.h"
#include "../src/executor.h"
#include "../src/translation.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>

#define CHECK(value)                                                                                                   \
    do {                                                                                                               \
        if (!(value)) {                                                                                                \
            fprintf(stderr, "x86_continue:%d: %s\n", __LINE__, #value);                                             \
            return 1;                                                                                                  \
        }                                                                                                              \
    } while (0)

#if defined(__aarch64__)
static hl_native_status executable_begin(void *opaque) {
    test_memory *memory = opaque;
    memory->begin_calls++;
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_WRITE) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static hl_native_status executable_end(void *opaque) {
    test_memory *memory = opaque;
    memory->end_calls++;
    __builtin___clear_cache((char *)memory->writable, (char *)memory->writable + memory->capacity);
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_EXEC) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static int run_loop(hl_native_executor *executor, const uint8_t *bytes, size_t size,
                    uint64_t pc, uint64_t budget, uint64_t iterations,
                    uint64_t interrupt, const hl_native_projection *projection,
                    hl_native_x86_64_cpu *state,
                    hl_native_exit *output) {
    hl_native_source_span span = {pc, bytes, size, 7, 16};
    hl_native_source source = {&span, 1, 7, 16};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_X86_64, .state.x86_64 = state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7, .budget = budget,
        .source = &source, .projection = projection};
    static const float one[4] = {1.0f, 1.0f, 1.0f, 1.0f};

    memset(state, 0, sizeof *state);
    state->program = pc;
    state->registers[0] = iterations;
    state->interrupt = interrupt;
    if (projection != NULL) state->registers[3] = projection->views[0].guest_first;
    memcpy(&state->vectors[0], one, sizeof one);
    memcpy(&state->vectors[2], one, sizeof one);
    return hl_native_run(executor, &cpu, &request, output) == HL_NATIVE_OK ? 0 : 1;
}

static int continuation_contract(void) {
    static const uint8_t register_loop[] = {
        0x0f, 0x58, 0xc1,       /* addps xmm0,xmm1 */
        0x83, 0xe8, 0x01,       /* sub eax,1 */
        0x75, 0xf8,             /* jne register_loop */
        0xcc,                   /* typed fallback after finite completion */
    };
    static const uint8_t memory_loop[] = {
        0x0f, 0x58, 0x03,       /* addps xmm0,[rbx] */
        0x83, 0xe8, 0x01,       /* sub eax,1 */
        0x75, 0xf8,             /* jne memory_loop */
        0xcc,
    };
    static const float one[4] = {1.0f, 1.0f, 1.0f, 1.0f};
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    hl_native_executor *executor = NULL;
    hl_native_x86_64_cpu state;
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    hl_native_projection_view view = {
        .guest_first = 0x9000, .guest_last = 0x9010,
        .host_first = (uint64_t)(uintptr_t)one, .mapping_incarnation = 7,
        .permissions = HL_NATIVE_ACCESS_READ,
        .write_policy = HL_NATIVE_WRITE_EXACT, .write_index = 0,
    };
    hl_native_projection projection = {&view, 1, 7, 0};

    memory.write_begin = executable_begin;
    memory.write_end = executable_end;
    hl_native_config config = test_config(&memory, 0);
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);

    CHECK(run_loop(executor, register_loop, sizeof register_loop, 0x8000,
                   1000, 300, 0, NULL, &state, &output) == 0);
    CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK && state.program == 0x8008 &&
          state.registers[0] == 0 && state.executed == 900 && state.budget == 100);

    CHECK(run_loop(executor, register_loop, sizeof register_loop, 0x8000,
                   768, 300, 0, NULL, &state, &output) == 0);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.program == 0x8000 &&
          state.registers[0] == 44 && state.executed == 768 && state.budget == 0);

    CHECK(run_loop(executor, memory_loop, sizeof memory_loop, 0x8100,
                   1000, 300, 0, &projection, &state, &output) == 0);
    CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK && state.program == 0x8108 &&
          state.registers[0] == 0 && state.executed == 900 && state.budget == 100);

    CHECK(run_loop(executor, register_loop, sizeof register_loop, 0x8000,
                   10, 3, 1, NULL, &state, &output) == 0);
    CHECK(output.kind == HL_NATIVE_EXIT_INTERRUPT && state.program == 0x8000 &&
          state.registers[0] == 3 && state.executed == 0 && state.budget == 10);

    {
        hl_native_change invalidate = {.abi = HL_NATIVE_ABI, .size = sizeof(invalidate),
            .kind = HL_NATIVE_INVALIDATE, .mapping_epoch = 7, .first = 0x8000, .last = 0x8008};
        hl_native_translation_key key = {0x8000, 7, 16, 0x8000, 0x8008, 0, 0, 0, 0, 0};
        hl_native_code code;
        CHECK(hl_native_translation_lookup(executor, &key, &code) == HL_NATIVE_HIT);
        CHECK(hl_native_changed(executor, &invalidate, 1) == HL_NATIVE_OK);
        CHECK(hl_native_translation_lookup(executor, &key, &code) == HL_NATIVE_MISS);
    }

    hl_native_destroy(executor);
    return 0;
}

static int projected_return_progress(void) {
    static const uint8_t tail[] = {
        0x49, 0x89, 0x11, /* mov [r9],rdx */
        0xc3,             /* ret */
    };
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    hl_native_executor *executor = NULL;
    hl_native_x86_64_cpu state = {0};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    uint64_t destination = 0;
    uint64_t return_targets[64];
    hl_native_projection_view views[] = {
        {.guest_first = 0x9000, .guest_last = 0x9008,
         .host_first = (uint64_t)(uintptr_t)&destination, .mapping_incarnation = 7,
         .permissions = HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE,
         .write_policy = HL_NATIVE_WRITE_EXACT, .write_index = 0},
        {.guest_first = 0xa000, .guest_last = 0xa200,
         .host_first = (uint64_t)(uintptr_t)return_targets, .mapping_incarnation = 7,
         .permissions = HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE,
         .write_policy = HL_NATIVE_WRITE_EXACT, .write_index = 1},
    };
    hl_native_projection projection = {views, 2, 7, 0};
    hl_native_source_span span = {0x8200, tail, sizeof tail, 7, 16};
    hl_native_source source = {&span, 1, 7, 16};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7, .budget = 64,
        .source = &source, .projection = &projection};

    memory.write_begin = executable_begin;
    memory.write_end = executable_end;
    hl_native_config config = test_config(&memory, 0);
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    for (size_t index = 0; index < 64; ++index) return_targets[index] = 0x8200;
    state.program = 0x8200;
    state.registers[2] = UINT64_C(0x123456789abcdef0);
    state.registers[4] = 0xa000;
    state.registers[9] = 0x9000;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 64 &&
          state.budget == 0 && state.program == 0x8200);
    CHECK(destination == UINT64_C(0x123456789abcdef0) && state.registers[4] == 0xa100);
    hl_native_destroy(executor);
    return 0;
}

static int alternating_writable_views_stay_native(void) {
    static const uint8_t loop[] = {
        0x89, 0x03,             /* mov [rbx],eax */
        0x89, 0x11,             /* mov [rcx],edx */
        0x48, 0xff, 0xce,       /* dec rsi */
        0x75, 0xf7,             /* jne loop */
        0xcc,                   /* typed fallback after finite completion */
    };
    uint32_t first = 0;
    uint32_t second = 0;
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    hl_native_executor *executor = NULL;
    hl_native_x86_64_cpu state = {0};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    hl_native_diagnostics diagnostic = {.abi = HL_NATIVE_ABI, .size = sizeof(diagnostic)};
    hl_native_projection_view views[] = {
        {.guest_first = 0x9000, .guest_last = 0x9004,
         .host_first = (uint64_t)(uintptr_t)&first, .mapping_incarnation = 7,
         .permissions = HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE,
         .write_policy = HL_NATIVE_WRITE_EXACT, .write_index = 0},
        {.guest_first = 0xa000, .guest_last = 0xa004,
         .host_first = (uint64_t)(uintptr_t)&second, .mapping_incarnation = 7,
         .permissions = HL_NATIVE_ACCESS_READ | HL_NATIVE_ACCESS_WRITE,
         .write_policy = HL_NATIVE_WRITE_EXACT, .write_index = 1},
    };
    hl_native_projection projection = {views, 2, 7, 0};
    hl_native_source_span span = {0x8300, loop, sizeof loop, 7, 16};
    hl_native_source source = {&span, 1, 7, 16};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7, .budget = 257,
        .source = &source, .projection = &projection};

    memory.write_begin = executable_begin;
    memory.write_end = executable_end;
    hl_native_config config = test_config(&memory, HL_NATIVE_DIAGNOSTICS);
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    state.program = 0x8300;
    state.registers[0] = UINT64_C(0x11223344);
    state.registers[2] = UINT64_C(0x55667788);
    state.registers[3] = 0x9000;
    state.registers[1] = 0xa000;
    state.registers[6] = 64;
    state.dirty_first = UINT64_MAX;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK && state.program == 0x8309);
    CHECK(first == UINT32_C(0x11223344) && second == UINT32_C(0x55667788));
    CHECK(state.executed == 256 && state.budget == 1 && state.registers[6] == 0);
    CHECK(state.dirty_count == 2 && state.dirty_overflow == 0);
    CHECK(state.dirty_records[0][0] == 0x9000 && state.dirty_records[0][1] == 0x9004 &&
          state.dirty_records[0][2] == 0x9000 && state.dirty_records[0][3] == 0x9004);
    CHECK(state.dirty_records[1][0] == 0xa000 && state.dirty_records[1][1] == 0xa004 &&
          state.dirty_records[1][2] == 0xa000 && state.dirty_records[1][3] == 0xa004);
    CHECK(state.dirty_view_first == 0xa000 && state.dirty_view_last == 0xa004 &&
          state.dirty_first == 0xa000 && state.dirty_last == 0xa004);
    CHECK(state.fault_access == 0 && state.fault_size == 0);
    CHECK(hl_native_diagnose(executor, &diagnostic) == HL_NATIVE_OK);
    CHECK(diagnostic.operand_cache_hits == 0 && diagnostic.operand_callbacks == 0 &&
          diagnostic.x86_public_exits == 1);
    hl_native_destroy(executor);
    return 0;
}

static int cold_dynamic_chain_is_bounded(void) {
    enum { BLOCKS = 66, STRIDE = 16 };
    uint8_t bytes[BLOCKS * STRIDE];
    const uint64_t first = 0xb000;
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    hl_native_executor *executor = NULL;
    hl_native_x86_64_cpu state = {0};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    hl_native_diagnostics diagnostic = {.abi = HL_NATIVE_ABI, .size = sizeof(diagnostic)};
    hl_native_source_span span = {first, bytes, sizeof bytes, 9, 21};
    hl_native_source source = {&span, 1, 9, 21};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_X86_64, .mapping_epoch = 9, .budget = 4096,
        .source = &source};

    memset(bytes, 0xcc, sizeof bytes);
    for (size_t index = 0; index + 1 < BLOCKS; ++index) {
        uint64_t target = first + (index + 1) * STRIDE;
        uint8_t *block = &bytes[index * STRIDE];
        block[0] = 0x48; block[1] = 0xb8; /* movabs target,rax */
        memcpy(&block[2], &target, sizeof target);
        block[10] = 0xff; block[11] = 0xe0; /* jmp rax */
    }
    memory.write_begin = executable_begin;
    memory.write_end = executable_end;
    hl_native_config config = test_config(&memory, HL_NATIVE_DIAGNOSTICS);
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    state.program = first;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.program == first + 64 * STRIDE);
    CHECK(state.executed == 128 && state.budget == 4096 - 128);
    CHECK(hl_native_diagnose(executor, &diagnostic) == HL_NATIVE_OK);
    CHECK(diagnostic.x86_cold_builds == 64 && diagnostic.x86_cold_quota_exits == 1);

    request.budget = state.budget;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK && state.program == first + 65 * STRIDE);
    CHECK(hl_native_diagnose(executor, &diagnostic) == HL_NATIVE_OK);
    CHECK(diagnostic.x86_cold_builds == 66 && diagnostic.x86_cold_quota_exits == 1);
    hl_native_destroy(executor);
    return 0;
}
#endif

int main(void) {
#if defined(__aarch64__)
    if (continuation_contract() != 0) return 1;
    if (projected_return_progress() != 0) return 1;
    if (alternating_writable_views_stay_native() != 0) return 1;
    return cold_dynamic_chain_is_bounded();
#else
    return 0;
#endif
}
